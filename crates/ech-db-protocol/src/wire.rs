use serde::{Deserialize, Serialize};

use crate::errors::DbError;
use crate::keys::{Range, ReadKey};
use crate::values::{AttestedIntent, ProgramBundle};

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Auth {
    pub pubkey: [u8; 32],
    #[serde(with = "crate::fixed64")]
    pub signature: [u8; 64],
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Open {
    pub protocol_version: u32,
    pub auth: Auth,
}

impl Open {
    pub const VERSION: u32 = 1;
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub enum OpenResponse {
    Opened,
    Error(DbError),
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Chunk {
    pub message_id: u64,
    pub bytes: Vec<u8>,
    pub last: bool,
}

impl Chunk {
    pub const MAX_BYTES: usize = 64 * 1024;
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub enum Request {
    UploadProgram(ProgramBundle),
    GetProgram([u8; 32]),
    Append(AttestedIntent),
    BeginRead,
    Get(Vec<ReadKey>),
    GetRange(Range, Option<u64>),
    CloseRead,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct StateWrite {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct AppendResult {
    pub output: Vec<u8>,
    pub writes: Vec<StateWrite>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub enum Response {
    ProgramUploaded([u8; 32]),
    Program(ProgramBundle),
    Appended(AppendResult),
    ReadOpened,
    Values(Vec<Option<Vec<u8>>>),
    RangeValues(Vec<(Vec<u8>, Vec<u8>)>),
    ReadClosed,
    Error(DbError),
}

pub enum ClientStreamItem {
    Open(Open),
    Chunk(Chunk),
}

pub enum ServerStreamItem {
    OpenResponse(OpenResponse),
    Chunk(Chunk),
}

pub struct ChunkAssembler {
    next_message_id: u64,
    pending: Vec<u8>,
    assembling: bool,
}

impl ChunkAssembler {
    pub fn new() -> Self {
        Self {
            next_message_id: 0,
            pending: Vec::new(),
            assembling: false,
        }
    }

    pub fn feed(&mut self, chunk: &Chunk) -> Result<Option<Vec<u8>>, DbError> {
        if chunk.message_id != self.next_message_id {
            return Err(DbError::protocol());
        }
        if !self.assembling {
            self.assembling = true;
            self.pending.clear();
        }
        if chunk.bytes.len() > Chunk::MAX_BYTES {
            return Err(DbError::protocol());
        }
        self.pending.extend_from_slice(&chunk.bytes);
        if !chunk.last {
            return Ok(None);
        }
        let message = core::mem::take(&mut self.pending);
        self.assembling = false;
        self.next_message_id = self
            .next_message_id
            .checked_add(1)
            .ok_or_else(DbError::protocol)?;
        Ok(Some(message))
    }
}

impl Default for ChunkAssembler {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ChunkSplitter {
    next_message_id: u64,
}

impl ChunkSplitter {
    pub fn new() -> Self {
        Self { next_message_id: 0 }
    }

    pub fn split(&mut self, message: &[u8]) -> Vec<Chunk> {
        let message_id = self.next_message_id;
        self.next_message_id = self
            .next_message_id
            .checked_add(1)
            .expect("message id overflow");
        let mut pieces: Vec<&[u8]> = message.chunks(Chunk::MAX_BYTES).collect();
        if pieces.is_empty() {
            pieces.push(&[]);
        }
        let last_index = pieces.len() - 1;
        pieces
            .into_iter()
            .enumerate()
            .map(|(index, bytes)| Chunk {
                message_id,
                bytes: bytes.to_vec(),
                last: index == last_index,
            })
            .collect()
    }
}

impl Default for ChunkSplitter {
    fn default() -> Self {
        Self::new()
    }
}
