use ech_db_protocol::errors::DbError;
use ech_db_protocol::ids::Stream;
use ech_db_protocol::keys::SysKey;
use ech_db_protocol::values::{AttestedIntent, Head, IntentBody};
use ech_db_protocol::wire::AppendResult;

use crate::exec::ExecMode;
use crate::program::Programs;
use crate::server::Server;

impl Server {
    pub async fn append(&self, root: [u8; 32], attested: AttestedIntent) -> Result<AppendResult, DbError> {
        if attested.intent.body.version != IntentBody::FORMAT_VERSION {
            return Err(DbError::protocol());
        }
        if attested.intent.body.root != root {
            return Err(DbError::unauthorized());
        }
        if !attested.intent.verify() {
            return Err(DbError::unauthorized());
        }
        if !attested.verify_attestation() {
            return Err(DbError::unauthorized());
        }
        let stream_id = Stream::intent(attested.intent.body.author).id();
        let program_hash = attested.intent.body.call.program_hash;
        let module = Programs::module(&self.fdb, self.executor.modules(), &root, &program_hash).await?;
        self.ensure_root(root).await?;
        let trx = self.fdb.trx()?;
        let head: Head = trx
            .get_bcs(&SysKey::head(&root))
            .await?
            .ok_or_else(DbError::integrity)?;
        let result = self
            .executor
            .execute_append(
                &trx,
                &root,
                &stream_id,
                &attested,
                &module,
                &ExecMode::Live {
                    head_number: head.number,
                },
            )
            .await?;
        trx.commit().await?;
        Ok(result)
    }
}
