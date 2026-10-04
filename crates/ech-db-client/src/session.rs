use bytes::{Buf, BufMut};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder, Streaming};
use tonic::codegen::http;
use tonic::transport::Channel;
use tonic::{Request, Status};

use ech_db_protocol::errors::CodecError;
use ech_db_protocol::wire::{
    Auth, Chunk, ChunkAssembler, ChunkSplitter, ClientStreamItem, Open, OpenResponse,
    Request as DbRequest, Response as DbResponse, ServerStreamItem,
};

use crate::error::Error;

pub const SERVICE_PATH: &str = "/ech.db.v1.Database/Exchange";

pub struct ClientCodec;

pub struct ClientEncoder {
    first: bool,
}

pub struct ServerDecoder {
    first: bool,
}

impl Codec for ClientCodec {
    type Encode = ClientStreamItem;
    type Decode = ServerStreamItem;
    type Encoder = ClientEncoder;
    type Decoder = ServerDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        ClientEncoder { first: true }
    }

    fn decoder(&mut self) -> Self::Decoder {
        ServerDecoder { first: true }
    }
}

impl Encoder for ClientEncoder {
    type Item = ClientStreamItem;
    type Error = Status;

    fn encode(&mut self, item: ClientStreamItem, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        let bytes = match item {
            ClientStreamItem::Open(open) => {
                if !self.first {
                    return Err(Status::internal("unexpected open frame"));
                }
                self.first = false;
                bcs::to_bytes(&open).map_err(|_| Status::internal("encode"))?
            }
            ClientStreamItem::Chunk(chunk) => {
                if self.first {
                    return Err(Status::internal("expected open frame"));
                }
                bcs::to_bytes(&chunk).map_err(|_| Status::internal("encode"))?
            }
        };
        dst.put_slice(&bytes);
        Ok(())
    }
}

impl Decoder for ServerDecoder {
    type Item = ServerStreamItem;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<ServerStreamItem>, Status> {
        let bytes = src.copy_to_bytes(src.remaining());
        if self.first {
            self.first = false;
            bcs::from_bytes::<OpenResponse>(&bytes)
                .map(|response| Some(ServerStreamItem::OpenResponse(response)))
                .map_err(|_| Status::invalid_argument("open response frame"))
        } else {
            bcs::from_bytes::<Chunk>(&bytes)
                .map(|chunk| Some(ServerStreamItem::Chunk(chunk)))
                .map_err(|_| Status::invalid_argument("chunk frame"))
        }
    }
}

pub struct Exchange {
    tx: mpsc::Sender<ClientStreamItem>,
    rx: Streaming<ServerStreamItem>,
    splitter: ChunkSplitter,
    assembler: ChunkAssembler,
}

impl Exchange {
    pub async fn connect(channel: &Channel, auth: Auth) -> Result<Self, Error> {
        let (tx, request_rx) = mpsc::channel(4);
        tx.send(ClientStreamItem::Open(Open {
            protocol_version: Open::VERSION,
            auth,
        }))
        .await
        .map_err(|_| Error::StreamClosed)?;
        let mut grpc = tonic::client::Grpc::new(channel.clone());
        grpc.ready().await?;
        let path = http::uri::PathAndQuery::from_static(SERVICE_PATH);
        let response = grpc
            .streaming(
                Request::new(ReceiverStream::new(request_rx)),
                path,
                ClientCodec,
            )
            .await?;
        let mut exchange = Self {
            tx,
            rx: response.into_inner(),
            splitter: ChunkSplitter::new(),
            assembler: ChunkAssembler::new(),
        };
        match exchange.rx.message().await? {
            Some(ServerStreamItem::OpenResponse(OpenResponse::Opened)) => Ok(exchange),
            Some(ServerStreamItem::OpenResponse(OpenResponse::Error(error))) => Err(Error::Db(error)),
            _ => Err(Error::Protocol),
        }
    }

    pub async fn request(&mut self, request: DbRequest) -> Result<DbResponse, Error> {
        let bytes = bcs::to_bytes(&request).map_err(CodecError::from)?;
        for chunk in self.splitter.split(&bytes) {
            self.tx
                .send(ClientStreamItem::Chunk(chunk))
                .await
                .map_err(|_| Error::StreamClosed)?;
        }
        loop {
            match self.rx.message().await? {
                Some(ServerStreamItem::Chunk(chunk)) => {
                    if let Some(message) = self.assembler.feed(&chunk)? {
                        let response: DbResponse = bcs::from_bytes(&message).map_err(CodecError::from)?;
                        return match response {
                            DbResponse::Error(error) => Err(Error::Db(error)),
                            response => Ok(response),
                        };
                    }
                }
                Some(ServerStreamItem::OpenResponse(_)) => return Err(Error::Protocol),
                None => return Err(Error::StreamClosed),
            }
        }
    }
}
