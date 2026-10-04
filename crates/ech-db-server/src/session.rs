use std::convert::Infallible;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::{Buf, BufMut};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder, Streaming};
use tonic::codegen::{http, Body, BoxFuture, Service as TowerService, StdError};
use tonic::server::{NamedService, StreamingService};
use tonic::{Code, Request, Response, Status};

use ech_db_protocol::errors::DbError;
use ech_db_protocol::wire::{
    Chunk, ChunkAssembler, ChunkSplitter, ClientStreamItem, Open, OpenResponse, Request as DbRequest,
    Response as DbResponse, ServerStreamItem,
};

use crate::reader::ReadSession;
use crate::server::Server;

pub const SERVICE_PATH: &str = "/ech.db.v1.Database/Exchange";

#[derive(Clone)]
pub struct ExchangeServer {
    inner: Arc<Server>,
}

impl ExchangeServer {
    pub fn new(inner: Arc<Server>) -> Self {
        Self { inner }
    }
}

impl NamedService for ExchangeServer {
    const NAME: &'static str = "ech.db.v1.Database";
}

impl<B> TowerService<http::Request<B>> for ExchangeServer
where
    B: Body + Send + 'static,
    B::Error: Into<StdError> + Send + 'static,
{
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        match req.uri().path() {
            SERVICE_PATH => {
                let server = self.inner.clone();
                let fut = async move {
                    let method = ExchangeMethod { server };
                    let mut grpc = tonic::server::Grpc::new(ExchangeCodec);
                    let response = grpc.streaming(method, req).await;
                    Ok(response)
                };
                Box::pin(fut)
            }
            _ => {
                let mut response = http::Response::new(tonic::body::Body::empty());
                let headers = response.headers_mut();
                headers.insert(Status::GRPC_STATUS, (Code::Unimplemented as i32).into());
                headers.insert(http::header::CONTENT_TYPE, tonic::metadata::GRPC_CONTENT_TYPE);
                Box::pin(async { Ok(response) })
            }
        }
    }
}

struct ExchangeMethod {
    server: Arc<Server>,
}

impl StreamingService<ClientStreamItem> for ExchangeMethod {
    type Response = ServerStreamItem;
    type ResponseStream = ReceiverStream<Result<ServerStreamItem, Status>>;
    type Future = BoxFuture<Response<Self::ResponseStream>, Status>;

    fn call(&mut self, request: Request<Streaming<ClientStreamItem>>) -> Self::Future {
        let server = self.server.clone();
        Box::pin(async move {
            let (sender, receiver) = mpsc::channel(8);
            tokio::spawn(async move {
                server.run_session(request.into_inner(), sender).await;
            });
            Ok(Response::new(ReceiverStream::new(receiver)))
        })
    }
}

pub struct ExchangeCodec;

pub struct ServerFrames {
    first: bool,
}

pub struct ClientFrames {
    first: bool,
}

impl Codec for ExchangeCodec {
    type Encode = ServerStreamItem;
    type Decode = ClientStreamItem;
    type Encoder = ServerFrames;
    type Decoder = ClientFrames;

    fn encoder(&mut self) -> Self::Encoder {
        ServerFrames { first: true }
    }

    fn decoder(&mut self) -> Self::Decoder {
        ClientFrames { first: true }
    }
}

impl Encoder for ServerFrames {
    type Item = ServerStreamItem;
    type Error = Status;

    fn encode(&mut self, item: ServerStreamItem, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        let bytes = match item {
            ServerStreamItem::OpenResponse(response) => {
                if !self.first {
                    return Err(Status::internal("unexpected open response"));
                }
                self.first = false;
                bcs::to_bytes(&response).map_err(|_| Status::internal("encode"))?
            }
            ServerStreamItem::Chunk(chunk) => {
                if self.first {
                    return Err(Status::internal("expected open response"));
                }
                bcs::to_bytes(&chunk).map_err(|_| Status::internal("encode"))?
            }
        };
        dst.put_slice(&bytes);
        Ok(())
    }
}

impl Decoder for ClientFrames {
    type Item = ClientStreamItem;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<ClientStreamItem>, Status> {
        let bytes = src.copy_to_bytes(src.remaining());
        if self.first {
            self.first = false;
            bcs::from_bytes::<Open>(&bytes)
                .map(|open| Some(ClientStreamItem::Open(open)))
                .map_err(|_| Status::invalid_argument("open frame"))
        } else {
            bcs::from_bytes::<Chunk>(&bytes)
                .map(|chunk| Some(ClientStreamItem::Chunk(chunk)))
                .map_err(|_| Status::invalid_argument("chunk frame"))
        }
    }
}

impl Server {
    fn verify_auth(open: &Open) -> Result<[u8; 32], DbError> {
        if open.protocol_version != Open::VERSION {
            return Err(DbError::protocol());
        }
        let key = ed25519_dalek::VerifyingKey::from_bytes(&open.auth.pubkey)
            .map_err(|_| DbError::unauthorized())?;
        let signature = ed25519_dalek::Signature::from_bytes(&open.auth.signature);
        key.verify_strict(&open.auth.pubkey, &signature)
            .map_err(|_| DbError::unauthorized())?;
        Ok(open.auth.pubkey)
    }

    async fn send_chunks(
        out: &mpsc::Sender<Result<ServerStreamItem, Status>>,
        splitter: &mut ChunkSplitter,
        response: DbResponse,
    ) -> Result<(), ()> {
        let bytes = match bcs::to_bytes(&response) {
            Ok(bytes) => bytes,
            Err(_) => return Err(()),
        };
        for chunk in splitter.split(&bytes) {
            out.send(Ok(ServerStreamItem::Chunk(chunk)))
                .await
                .map_err(|_| ())?;
        }
        Ok(())
    }

    pub async fn run_session(
        self: &Arc<Self>,
        mut inbound: Streaming<ClientStreamItem>,
        out: mpsc::Sender<Result<ServerStreamItem, Status>>,
    ) {
        let open = match inbound.message().await {
            Ok(Some(ClientStreamItem::Open(open))) => open,
            _ => {
                let response = OpenResponse::Error(DbError::protocol());
                let _ = out
                    .send(Ok(ServerStreamItem::OpenResponse(response)))
                    .await;
                return;
            }
        };
        let root = match Self::verify_auth(&open) {
            Ok(root) => root,
            Err(error) => {
                let _ = out
                    .send(Ok(ServerStreamItem::OpenResponse(OpenResponse::Error(error))))
                    .await;
                return;
            }
        };
        if out
            .send(Ok(ServerStreamItem::OpenResponse(OpenResponse::Opened)))
            .await
            .is_err()
        {
            return;
        }
        let mut assembler = ChunkAssembler::new();
        let mut splitter = ChunkSplitter::new();
        let mut reader: Option<ReadSession> = None;
        loop {
            let frame = match inbound.message().await {
                Ok(Some(frame)) => frame,
                Ok(None) => return,
                Err(_) => return,
            };
            let ClientStreamItem::Chunk(chunk) = frame else {
                let _ = Self::send_chunks(&out, &mut splitter, DbResponse::Error(DbError::protocol())).await;
                return;
            };
            let message = match assembler.feed(&chunk) {
                Ok(Some(message)) => message,
                Ok(None) => continue,
                Err(error) => {
                    let _ = Self::send_chunks(&out, &mut splitter, DbResponse::Error(error)).await;
                    return;
                }
            };
            let request: DbRequest = match bcs::from_bytes(&message) {
                Ok(request) => request,
                Err(_) => {
                    let _ = Self::send_chunks(&out, &mut splitter, DbResponse::Error(DbError::bad_bcs())).await;
                    return;
                }
            };
            let (response, close) = self.dispatch(&mut reader, root, request).await;
            if Self::send_chunks(&out, &mut splitter, response).await.is_err() {
                return;
            }
            if close {
                return;
            }
        }
    }

    async fn dispatch(
        &self,
        reader: &mut Option<ReadSession>,
        root: [u8; 32],
        request: DbRequest,
    ) -> (DbResponse, bool) {
        match request {
            DbRequest::UploadProgram(bundle) => {
                let response = match self.upload_program(root, bundle).await {
                    Ok(hash) => DbResponse::ProgramUploaded(hash),
                    Err(error) => DbResponse::Error(error),
                };
                (response, true)
            }
            DbRequest::GetProgram(hash) => {
                let response = match self.get_program(root, hash).await {
                    Ok(bundle) => DbResponse::Program(bundle),
                    Err(error) => DbResponse::Error(error),
                };
                (response, true)
            }
            DbRequest::Append(intent) => {
                let response = match self.append(root, intent).await {
                    Ok(result) => DbResponse::Appended(result),
                    Err(error) => DbResponse::Error(error),
                };
                (response, true)
            }
            DbRequest::BeginRead => {
                if reader.is_some() {
                    return (DbResponse::Error(DbError::protocol()), true);
                }
                match ReadSession::open(&self.fdb) {
                    Ok(session) => {
                        *reader = Some(session);
                        (DbResponse::ReadOpened, false)
                    }
                    Err(error) => (DbResponse::Error(error), true),
                }
            }
            DbRequest::Get(keys) => {
                let Some(session) = reader.as_ref() else {
                    return (DbResponse::Error(DbError::protocol()), true);
                };
                match session.get(&root, &keys).await {
                    Ok(values) => (DbResponse::Values(values), false),
                    Err(error) => (DbResponse::Error(error), true),
                }
            }
            DbRequest::GetRange(range, limit) => {
                let Some(session) = reader.as_ref() else {
                    return (DbResponse::Error(DbError::protocol()), true);
                };
                match session.get_range(&root, &range, limit).await {
                    Ok(values) => (DbResponse::RangeValues(values), false),
                    Err(error) => (DbResponse::Error(error), true),
                }
            }
            DbRequest::CloseRead => {
                if reader.take().is_none() {
                    return (DbResponse::Error(DbError::protocol()), true);
                }
                (DbResponse::ReadClosed, true)
            }
        }
    }
}
