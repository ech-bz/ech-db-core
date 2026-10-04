use bcs::{from_bytes, to_bytes};
use foundationdb_tuple::unpack;

use ech_db_protocol::errors::DbError;
use ech_db_protocol::hash::Blake2b256;
use ech_db_protocol::keys::{Prefix, SysKey, WindowKey};
use ech_db_protocol::values::{ArchiveRecord, ExportCursor, Head, WindowBlob};

use crate::s3::PutOutcome;
use crate::server::Server;
use crate::sui::SuiError;

#[derive(Debug)]
pub enum ExportOutcome {
    Exported,
    Idle,
}

impl Server {
    pub async fn export_once(&self, root: [u8; 32]) -> Result<ExportOutcome, DbError> {
        let trx = self.fdb.trx()?;
        let cursor: Option<ExportCursor> = trx.get_bcs(&SysKey::export_cursor(&root)).await?;
        let head: Head = trx
            .get_bcs(&SysKey::head(&root))
            .await?
            .ok_or_else(DbError::integrity)?;
        let marker_prefix = SysKey::windows_prefix(&root);
        let marker_end = Prefix::successor(&marker_prefix).ok_or_else(DbError::integrity)?;
        let markers = trx.range_raw(&marker_prefix, &marker_end).await?;
        let min = markers
            .first()
            .map(|(key, _)| unpack::<u64>(&key[marker_prefix.len()..]))
            .transpose()
            .map_err(|_| DbError::integrity())?;
        let expected = cursor
            .map(|cursor| cursor.number.checked_add(1).ok_or_else(DbError::integrity))
            .transpose()?
            .unwrap_or(0);
        let Some(min) = min else {
            return Ok(ExportOutcome::Idle);
        };
        if min >= head.number {
            return Ok(ExportOutcome::Idle);
        }
        if min != expected {
            return Err(DbError::integrity());
        }
        let (begin, end) = WindowKey::records_range(&root, min);
        let rows = trx.range_raw(&begin, &end).await?;
        drop(trx);
        let mut records = Vec::with_capacity(rows.len());
        for (_, value) in rows {
            records.push(from_bytes::<ArchiveRecord>(&value).map_err(|_| DbError::integrity())?);
        }
        let check = self.fdb.trx()?;
        if check
            .get_raw(&SysKey::window_number(&root, min))
            .await?
            .is_none()
        {
            return Ok(ExportOutcome::Idle);
        }
        drop(check);
        let prev_hash = cursor.map(|cursor| cursor.hash).unwrap_or([0u8; 32]);
        let blob = WindowBlob {
            format_version: WindowBlob::FORMAT_VERSION,
            root,
            window_number: min,
            prev_hash,
            records,
        };
        let blob_bytes = to_bytes(&blob).map_err(|_| DbError::integrity())?;
        let blob_hash = Blake2b256::hash(&[blob_bytes.as_slice()]);
        let key = self.s3.key(&root, min);
        match self.s3.put_immutable(&key, &blob_bytes).await {
            Ok(PutOutcome::Stored) | Ok(PutOutcome::AlreadyPresent) => {}
            Err(error) => return Err(crate::s3::S3Errors::db(&error)),
        }
        match self.sui.anchor(&root, &prev_hash, &blob_hash).await {
            Ok(()) => {}
            Err(SuiError::AnchorConflict) => {
                let check = self.fdb.trx()?;
                if check
                    .get_raw(&SysKey::window_number(&root, min))
                    .await?
                    .is_some()
                {
                    return Err(DbError::integrity());
                }
                return Ok(ExportOutcome::Idle);
            }
            Err(error) => return Err(crate::sui::SuiErrors::db(&error)),
        }
        let trx = self.fdb.trx()?;
        let current: Option<ExportCursor> = trx.get_bcs(&SysKey::export_cursor(&root)).await?;
        if current != cursor {
            return Ok(ExportOutcome::Idle);
        }
        if trx
            .get_raw(&SysKey::window_number(&root, min))
            .await?
            .is_none()
        {
            return Ok(ExportOutcome::Idle);
        }
        trx.clear_range(&begin, &end);
        trx.clear(&SysKey::window_number(&root, min));
        trx.set_bcs(
            &SysKey::export_cursor(&root),
            &ExportCursor {
                number: min,
                hash: blob_hash,
            },
        )?;
        trx.commit().await?;
        Ok(ExportOutcome::Exported)
    }
}
