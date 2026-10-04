use bcs::{from_bytes, to_bytes};

use ech_db_protocol::errors::{CodecError, DbError};
use ech_db_protocol::hash::Blake2b256;
use ech_db_protocol::ids::Stream;
use ech_db_protocol::keys::{LogKey, SysKey};
use ech_db_protocol::values::{
    ArchiveRecord, AttestedIntent, ExportCursor, Head, IntentBody, ProgramBundle, RecoveryProgress,
    WindowBlob,
};

use ech_db_server::config::Config;
use ech_db_server::exec::{ExecMode, Executor};
use ech_db_server::fdb::Fdb;
use ech_db_server::program::Programs;
use ech_db_server::s3::{S3Error, S3};
use ech_db_server::sui::{Anchor, Sui, SuiError};
use ech_db_server::windows::Clock;

#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    #[error("anchor not found")]
    NoAnchor,
    #[error("s3: {0}")]
    S3(#[from] S3Error),
    #[error("sui: {0}")]
    Sui(#[from] SuiError),
    #[error("fdb: {0}")]
    Fdb(#[from] DbError),
    #[error("fdb open: {0}")]
    FdbOpen(#[from] foundationdb::FdbError),
    #[error("codec: {0}")]
    Codec(#[from] CodecError),
    #[error("missing window object {0}")]
    MissingWindow(u64),
    #[error("window chain mismatch at window {0}")]
    Chain(u64),
    #[error("target window mismatch")]
    Target,
    #[error("root already has fdb state")]
    RootNotEmpty,
    #[error("integrity: {0}")]
    Integrity(String),
}

pub struct Recovery {
    fdb: Fdb,
    s3: S3,
    sui: Sui,
    executor: Executor,
    window_ms: u64,
}

struct VerifiedWindow {
    number: u64,
    hash: [u8; 32],
    blob: WindowBlob,
}

impl Recovery {
    pub fn new(config: &Config, executor: Executor) -> Result<Self, RecoveryError> {
        let fdb = Fdb::open(&config.fdb)?;
        Ok(Self {
            fdb,
            s3: S3::new(&config.s3),
            sui: Sui::new(&config.sui)?,
            executor,
            window_ms: config.window.length_ms(),
        })
    }

    pub async fn run(&self, root: [u8; 32]) -> Result<(), RecoveryError> {
        let anchor = self
            .sui
            .anchor_of(&root)
            .await?
            .ok_or(RecoveryError::NoAnchor)?;
        let windows = self.verify_chain(root, &anchor).await?;
        let target = windows.last().ok_or(RecoveryError::Target)?;
        self.apply(root, &windows, target.number).await?;
        self.finalize(root, target).await?;
        Ok(())
    }

    async fn verify_chain(
        &self,
        root: [u8; 32],
        anchor: &Anchor,
    ) -> Result<Vec<VerifiedWindow>, RecoveryError> {
        let mut windows = Vec::new();
        let mut expected_prev = [0u8; 32];
        let mut number = 0u64;
        loop {
            let key = self.s3.key(&root, number);
            let bytes = self
                .s3
                .get_optional(&key)
                .await?
                .ok_or(RecoveryError::MissingWindow(number))?;
            let hash = Blake2b256::hash(&[bytes.as_slice()]);
            let blob: WindowBlob = from_bytes(&bytes).map_err(|_| RecoveryError::Chain(number))?;
            if to_bytes(&blob).map_err(CodecError::from)? != bytes {
                return Err(RecoveryError::Chain(number));
            }
            if blob.format_version != WindowBlob::FORMAT_VERSION
                || blob.root != root
                || blob.window_number != number
                || blob.prev_hash != expected_prev
            {
                return Err(RecoveryError::Chain(number));
            }
            expected_prev = hash;
            let done = hash == anchor.cur_hash;
            windows.push(VerifiedWindow {
                number,
                hash,
                blob,
            });
            if done {
                break;
            }
            number = number.checked_add(1).ok_or(RecoveryError::Target)?;
        }
        let target = windows.last().ok_or(RecoveryError::Target)?;
        if target.blob.prev_hash != anchor.prev_hash {
            return Err(RecoveryError::Target);
        }
        Ok(windows)
    }

    async fn apply(
        &self,
        root: [u8; 32],
        windows: &[VerifiedWindow],
        target: u64,
    ) -> Result<(), RecoveryError> {
        let trx = self.fdb.trx()?;
        if trx.get_raw(&SysKey::head(&root)).await?.is_some() {
            return Err(RecoveryError::RootNotEmpty);
        }
        let progress: Option<RecoveryProgress> = trx.get_bcs(&SysKey::recovery(&root)).await?;
        drop(trx);
        for window in windows {
            if window.number > target {
                break;
            }
            let skip = match &progress {
                Some(progress) if progress.window > window.number => window.blob.records.len() as u64,
                Some(progress) if progress.window == window.number => progress.processed,
                _ => 0,
            };
            self.apply_window(root, window, skip).await?;
        }
        Ok(())
    }

    async fn apply_window(
        &self,
        root: [u8; 32],
        window: &VerifiedWindow,
        skip: u64,
    ) -> Result<(), RecoveryError> {
        let records = &window.blob.records;
        let mut index = 0u64;
        while (index as usize) < records.len() {
            match &records[index as usize] {
                ArchiveRecord::ProgramChunk { .. } => {
                    let start = index;
                    let (program_hash, chunk_count, group) = Self::chunk_group(records, index);
                    let end = start + group.len() as u64;
                    if skip >= end {
                        index = end;
                        continue;
                    }
                    if skip > start {
                        return Err(RecoveryError::Integrity(format!(
                            "progress splits program group in window {}",
                            window.number
                        )));
                    }
                    self.apply_program(root, window.number, &program_hash, chunk_count, &group, end)
                        .await?;
                    index = end;
                }
                ArchiveRecord::Intent(intent) => {
                    let end = index + 1;
                    if skip >= end {
                        index = end;
                        continue;
                    }
                    self.apply_event(root, window.number, intent, end).await?;
                    index = end;
                }
            }
        }
        if records.is_empty() {
            let progress = RecoveryProgress {
                window: window.number,
                processed: 0,
            };
            let trx = self.fdb.trx()?;
            trx.set_bcs(&SysKey::recovery(&root), &progress)?;
            trx.commit().await?;
        }
        Ok(())
    }

    fn chunk_group(
        records: &[ArchiveRecord],
        start: u64,
    ) -> ([u8; 32], u64, Vec<(u64, Vec<u8>)>) {
        let ArchiveRecord::ProgramChunk {
            program_hash,
            chunk_count,
            ..
        } = &records[start as usize]
        else {
            return ([0u8; 32], 0, Vec::new());
        };
        let mut group = Vec::new();
        let mut index = start as usize;
        while index < records.len() {
            let ArchiveRecord::ProgramChunk {
                program_hash: hash,
                chunk_seq,
                chunk_count: count,
                bytes,
            } = &records[index]
            else {
                break;
            };
            if hash != program_hash || count != chunk_count {
                break;
            }
            group.push((*chunk_seq, bytes.clone()));
            index += 1;
            if group.len() as u64 == *chunk_count {
                break;
            }
        }
        (*program_hash, *chunk_count, group)
    }

    async fn apply_program(
        &self,
        root: [u8; 32],
        window_number: u64,
        program_hash: &[u8; 32],
        chunk_count: u64,
        group: &[(u64, Vec<u8>)],
        processed: u64,
    ) -> Result<(), RecoveryError> {
        if group.len() as u64 != chunk_count {
            return Err(RecoveryError::Integrity(format!(
                "incomplete program group in window {window_number}"
            )));
        }
        let mut encoded = Vec::new();
        for (position, (chunk_seq, bytes)) in group.iter().enumerate() {
            if *chunk_seq != position as u64 + 1 {
                return Err(RecoveryError::Integrity(format!(
                    "chunk sequence gap in window {window_number}"
                )));
            }
            encoded.extend_from_slice(bytes);
        }
        let bundle: ProgramBundle = from_bytes(&encoded).map_err(|_| {
            RecoveryError::Integrity(format!("program chunk decode in window {window_number}"))
        })?;
        let computed = bundle.hash()?;
        if computed != *program_hash {
            return Err(RecoveryError::Integrity(format!(
                "program hash mismatch in window {window_number}"
            )));
        }
        let trx = self.fdb.trx()?;
        let stream = Stream::program(*program_hash).id();
        if trx.get_raw(&LogKey::seq(&root, &stream)).await?.is_some() {
            return Err(RecoveryError::Integrity(format!(
                "program already present in window {window_number}"
            )));
        }
        for (position, (_, bytes)) in group.iter().enumerate() {
            trx.set_raw(&LogKey::data(&root, &stream, position as u64 + 1), bytes);
        }
        trx.set_bcs(&LogKey::seq(&root, &stream), &chunk_count)?;
        trx.set_bcs(
            &SysKey::recovery(&root),
            &RecoveryProgress {
                window: window_number,
                processed,
            },
        )?;
        trx.commit().await?;
        Ok(())
    }

    async fn apply_event(
        &self,
        root: [u8; 32],
        window_number: u64,
        attested: &AttestedIntent,
        processed: u64,
    ) -> Result<(), RecoveryError> {
        if attested.intent.body.version != IntentBody::FORMAT_VERSION {
            return Err(RecoveryError::Integrity(format!(
                "intent version in window {window_number}"
            )));
        }
        if attested.intent.body.root != root {
            return Err(RecoveryError::Integrity(format!(
                "intent root in window {window_number}"
            )));
        }
        if !attested.intent.verify() {
            return Err(RecoveryError::Integrity(format!(
                "intent signature in window {window_number}"
            )));
        }
        if !attested.verify_attestation() {
            return Err(RecoveryError::Integrity(format!(
                "attestation signature in window {window_number}"
            )));
        }
        let stream_id = Stream::intent(attested.intent.body.author).id();
        let program_hash = attested.intent.body.call.program_hash;
        let module = Programs::module(&self.fdb, self.executor.modules(), &root, &program_hash).await?;
        let progress = RecoveryProgress {
            window: window_number,
            processed,
        };
        let progress_value = to_bytes(&progress).map_err(CodecError::from)?;
        let trx = self.fdb.trx()?;
        let mode = ExecMode::Recovery {
            progress_key: SysKey::recovery(&root),
            progress_value,
        };
        self.executor
            .execute_append(&trx, &root, &stream_id, attested, &module, &mode)
            .await
            .map_err(|error| RecoveryError::Integrity(format!("event replay: {error}")))?;
        trx.commit().await?;
        Ok(())
    }

    async fn finalize(&self, root: [u8; 32], target: &VerifiedWindow) -> Result<(), RecoveryError> {
        let closes_at_unix_ms = Head::initial(Clock::now_unix_ms(), self.window_ms)
            .ok_or(RecoveryError::Target)?
            .closes_at_unix_ms;
        let head = Head {
            number: target.number.checked_add(1).ok_or(RecoveryError::Target)?,
            closes_at_unix_ms,
        };
        let trx = self.fdb.trx()?;
        if trx.get_raw(&SysKey::head(&root)).await?.is_some() {
            return Err(RecoveryError::RootNotEmpty);
        }
        trx.set_bcs(
            &SysKey::export_cursor(&root),
            &ExportCursor {
                number: target.number,
                hash: target.hash,
            },
        )?;
        trx.set_bcs(&SysKey::head(&root), &head)?;
        trx.set_raw(&SysKey::window_number(&root, head.number), b"");
        trx.clear(&SysKey::recovery(&root));
        trx.commit().await?;
        Ok(())
    }
}
