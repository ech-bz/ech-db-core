use bcs::{from_bytes, to_bytes};
use wasmi::Module;

use ech_db_protocol::errors::{DbError, DbErrorCode};
use ech_db_protocol::ids::Stream;
use ech_db_protocol::keys::{LogKey, SysKey, WindowKey};
use ech_db_protocol::values::{ArchiveRecord, Head, ProgramBundle, ProgramChunks};

use crate::abi::Abi;
use crate::fdb::{Fdb, Trx};
use crate::server::Server;
use crate::wasm::{ModuleCache, Profile};

pub struct Programs;

impl Programs {
    pub fn prepare(
        bundle: ProgramBundle,
        modules: &ModuleCache,
    ) -> Result<([u8; 32], Vec<u8>), DbError> {
        if bundle.format_version != ProgramBundle::FORMAT_VERSION {
            return Err(DbError::invalid_wasm());
        }
        if bundle.execution_profile != Profile::ID {
            return Err(DbError::unsupported_profile(bundle.execution_profile));
        }
        let module = Module::new(modules.engine(), &bundle.wasm).map_err(|_| DbError::invalid_wasm())?;
        Abi::check(&module)?;
        Abi::check_profile_shape(&bundle.wasm)?;
        let encoded = to_bytes(&bundle).map_err(|_| DbError::invalid_wasm())?;
        let hash = bundle.hash().map_err(|_| DbError::invalid_wasm())?;
        modules.insert(hash, module);
        Ok((hash, encoded))
    }

    pub async fn load(fdb: &Fdb, root: &[u8; 32], hash: &[u8; 32]) -> Result<ProgramBundle, DbError> {
        let trx = fdb.trx()?;
        Self::load_in(&trx, root, hash).await
    }

    pub async fn module(
        fdb: &Fdb,
        modules: &ModuleCache,
        root: &[u8; 32],
        hash: &[u8; 32],
    ) -> Result<Module, DbError> {
        if let Some(module) = modules.get(hash) {
            return Ok(module);
        }
        let bundle = Self::load(fdb, root, hash).await?;
        if bundle.execution_profile != Profile::ID {
            return Err(DbError::unsupported_profile(bundle.execution_profile));
        }
        let module = Module::new(modules.engine(), &bundle.wasm).map_err(|_| DbError::invalid_wasm())?;
        modules.insert(*hash, module.clone());
        Ok(module)
    }

    pub async fn load_in(trx: &Trx, root: &[u8; 32], hash: &[u8; 32]) -> Result<ProgramBundle, DbError> {
        let stream = Stream::program(*hash).id();
        let count: u64 = trx
            .get_bcs(&LogKey::seq(root, &stream))
            .await?
            .ok_or_else(|| DbError::program_not_found())?;
        if count > u16::MAX as u64 {
            return Err(DbError::never(DbErrorCode::Integrity));
        }
        let mut encoded = Vec::new();
        for seq in 1..=count {
            let chunk = trx
                .get_raw(&LogKey::data(root, &stream, seq))
                .await?
                .ok_or_else(|| DbError::never(DbErrorCode::Integrity))?;
            encoded.extend_from_slice(&chunk);
        }
        let bundle: ProgramBundle =
            from_bytes(&encoded).map_err(|_| DbError::never(DbErrorCode::Integrity))?;
        let computed = bundle.hash().map_err(|_| DbError::never(DbErrorCode::Integrity))?;
        if computed != *hash {
            return Err(DbError::never(DbErrorCode::Integrity));
        }
        Ok(bundle)
    }

    pub fn split(encoded: &[u8]) -> Result<Vec<&[u8]>, DbError> {
        let chunks = ProgramChunks::split(encoded);
        if chunks.is_empty() {
            return Err(DbError::invalid_wasm());
        }
        if chunks.len() > u16::MAX as usize {
            return Err(DbError::resource_limit());
        }
        Ok(chunks)
    }
}

impl Server {
    pub async fn upload_program(&self, root: [u8; 32], bundle: ProgramBundle) -> Result<[u8; 32], DbError> {
        let (hash, encoded) = Programs::prepare(bundle, self.executor.modules())?;
        let chunks = Programs::split(&encoded)?;
        let count = chunks.len() as u64;
        self.ensure_root(root).await?;
        let trx = self.fdb.trx()?;
        let stream = Stream::program(hash).id();
        if trx.get_raw(&LogKey::seq(&root, &stream)).await?.is_some() {
            return Ok(hash);
        }
        let head: Head = trx
            .get_bcs(&SysKey::head(&root))
            .await?
            .ok_or_else(|| DbError::integrity())?;
        for (index, chunk) in chunks.iter().enumerate() {
            let seq = index as u64 + 1;
            trx.set_raw(&LogKey::data(&root, &stream, seq), chunk);
            let archived = ArchiveRecord::ProgramChunk {
                program_hash: hash,
                chunk_seq: seq,
                chunk_count: count,
                bytes: chunk.to_vec(),
            };
            let value = to_bytes(&archived).map_err(|_| DbError::integrity())?;
            trx.set_versionstamped_key(&WindowKey::record(&root, head.number, index as u16), &value);
        }
        trx.set_bcs(&LogKey::seq(&root, &stream), &count)?;
        trx.commit().await?;
        Ok(hash)
    }

    pub async fn get_program(&self, root: [u8; 32], hash: [u8; 32]) -> Result<ProgramBundle, DbError> {
        let trx = self.fdb.trx()?;
        Programs::load_in(&trx, &root, &hash).await
    }
}
