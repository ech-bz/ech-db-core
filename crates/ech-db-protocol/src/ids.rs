use crate::hash::Blake2b256;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct StreamId([u8; 32]);

impl StreamId {
    pub fn as_array(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Intent,
    Program,
}

pub struct Stream {
    kind: Kind,
    raw: [u8; 32],
}

impl Stream {
    pub fn intent(pubkey: [u8; 32]) -> Self {
        Self {
            kind: Kind::Intent,
            raw: pubkey,
        }
    }

    pub fn program(hash: [u8; 32]) -> Self {
        Self {
            kind: Kind::Program,
            raw: hash,
        }
    }

    pub fn id(&self) -> StreamId {
        let hash = match self.kind {
            Kind::Intent => Blake2b256::hash(&[b"intent", &self.raw]),
            Kind::Program => Blake2b256::hash(&[b"program", &self.raw]),
        };
        StreamId(hash)
    }
}
