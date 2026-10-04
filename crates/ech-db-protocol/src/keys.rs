use foundationdb_tuple::{pack, pack_with_versionstamp, Bytes, Versionstamp};
use serde::{Deserialize, Serialize};

use crate::ids::StreamId;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Namespace {
    Log,
    State,
}

impl Namespace {
    pub fn name(self) -> &'static str {
        match self {
            Namespace::Log => "log",
            Namespace::State => "state",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct ReadKey {
    pub namespace: Namespace,
    pub suffix: Vec<u8>,
}

impl ReadKey {
    pub fn log_seq(stream: &StreamId) -> Self {
        Self {
            namespace: Namespace::Log,
            suffix: pack(&(Bytes::from(stream.as_array().as_slice()), "seq")),
        }
    }

    pub fn log_data(stream: &StreamId, seq: u64) -> Self {
        Self {
            namespace: Namespace::Log,
            suffix: pack(&(Bytes::from(stream.as_array().as_slice()), "data", seq)),
        }
    }

    pub fn state(user_key: Vec<u8>) -> Self {
        Self {
            namespace: Namespace::State,
            suffix: user_key,
        }
    }

    pub fn physical(&self, root: &[u8; 32]) -> Vec<u8> {
        let mut key = pack(&(Bytes::from(root.as_slice()), self.namespace.name()));
        key.extend_from_slice(&self.suffix);
        key
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Range {
    pub namespace: Namespace,
    pub start: Vec<u8>,
    pub end: Option<Vec<u8>>,
}

impl Range {
    pub fn state(start: Vec<u8>, end: Option<Vec<u8>>) -> Self {
        Self {
            namespace: Namespace::State,
            start,
            end,
        }
    }

    pub fn log_stream(stream: &StreamId) -> Self {
        let start = pack(&(Bytes::from(stream.as_array().as_slice()),));
        let end = Prefix::successor(&start).expect("stream prefix is not all 0xff");
        Self {
            namespace: Namespace::Log,
            start,
            end: Some(end),
        }
    }

    pub fn log_data(stream: &StreamId) -> Self {
        let start = pack(&(Bytes::from(stream.as_array().as_slice()), "data"));
        let end = Prefix::successor(&start);
        Self {
            namespace: Namespace::Log,
            start,
            end,
        }
    }

    pub fn physical(&self, root: &[u8; 32]) -> (Vec<u8>, Vec<u8>) {
        let prefix = pack(&(Bytes::from(root.as_slice()), self.namespace.name()));
        let mut begin = prefix.clone();
        begin.extend_from_slice(&self.start);
        let end = match &self.end {
            Some(end) => {
                let mut end_key = prefix.clone();
                end_key.extend_from_slice(end);
                end_key
            }
            None => Prefix::successor(&prefix).expect("namespace prefix is not all 0xff"),
        };
        (begin, end)
    }
}

pub struct Prefix;

impl Prefix {
    pub fn successor(prefix: &[u8]) -> Option<Vec<u8>> {
        let mut key = prefix.to_vec();
        while let Some(last) = key.last_mut() {
            if *last < 0xff {
                *last += 1;
                return Some(key);
            }
            key.pop();
        }
        None
    }
}

pub struct LogKey;

impl LogKey {
    pub fn seq(root: &[u8; 32], stream: &StreamId) -> Vec<u8> {
        pack(&(
            Bytes::from(root.as_slice()),
            "log",
            Bytes::from(stream.as_array().as_slice()),
            "seq",
        ))
    }

    pub fn data(root: &[u8; 32], stream: &StreamId, seq: u64) -> Vec<u8> {
        pack(&(
            Bytes::from(root.as_slice()),
            "log",
            Bytes::from(stream.as_array().as_slice()),
            "data",
            seq,
        ))
    }

    pub fn data_prefix(root: &[u8; 32], stream: &StreamId) -> Vec<u8> {
        pack(&(
            Bytes::from(root.as_slice()),
            "log",
            Bytes::from(stream.as_array().as_slice()),
            "data",
        ))
    }
}

pub struct StateKey;

impl StateKey {
    pub fn prefix(root: &[u8; 32]) -> Vec<u8> {
        pack(&(Bytes::from(root.as_slice()), "state"))
    }

    pub fn physical(root: &[u8; 32], user_key: &[u8]) -> Vec<u8> {
        let mut key = Self::prefix(root);
        key.extend_from_slice(user_key);
        key
    }
}

pub struct SysKey;

impl SysKey {
    pub fn head(root: &[u8; 32]) -> Vec<u8> {
        pack(&(Bytes::from(root.as_slice()), "sys", "head"))
    }

    pub fn export_cursor(root: &[u8; 32]) -> Vec<u8> {
        pack(&(Bytes::from(root.as_slice()), "sys", "export_cursor"))
    }

    pub fn window_number(root: &[u8; 32], number: u64) -> Vec<u8> {
        pack(&(Bytes::from(root.as_slice()), "sys", "window", number))
    }

    pub fn windows_prefix(root: &[u8; 32]) -> Vec<u8> {
        pack(&(Bytes::from(root.as_slice()), "sys", "window"))
    }

    pub fn recovery(root: &[u8; 32]) -> Vec<u8> {
        pack(&(Bytes::from(root.as_slice()), "sys", "recovery"))
    }
}

pub struct WindowKey;

impl WindowKey {
    pub fn prefix(root: &[u8; 32], number: u64) -> Vec<u8> {
        pack(&(
            Bytes::from(root.as_slice()),
            "window",
            number,
        ))
    }

    pub fn record(root: &[u8; 32], number: u64, user_version: u16) -> Vec<u8> {
        pack_with_versionstamp(&(
            Bytes::from(root.as_slice()),
            "window",
            number,
            Versionstamp::incomplete(user_version),
        ))
    }

    pub fn records_range(root: &[u8; 32], number: u64) -> (Vec<u8>, Vec<u8>) {
        let begin = Self::prefix(root, number);
        let end = Prefix::successor(&begin).expect("window prefix is not all 0xff");
        (begin, end)
    }
}
