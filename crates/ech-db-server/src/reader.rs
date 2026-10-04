use ech_db_protocol::errors::DbError;
use ech_db_protocol::keys::{Range, ReadKey};

use crate::fdb::{Fdb, Trx};

pub struct ReadSession {
    trx: Trx,
}

impl ReadSession {
    pub fn open(fdb: &Fdb) -> Result<Self, DbError> {
        Ok(Self { trx: fdb.trx()? })
    }

    pub async fn get(
        &self,
        root: &[u8; 32],
        keys: &[ReadKey],
    ) -> Result<Vec<Option<Vec<u8>>>, DbError> {
        let mut values = Vec::with_capacity(keys.len());
        for key in keys {
            values.push(self.trx.get_raw(&key.physical(root)).await?);
        }
        Ok(values)
    }

    pub async fn get_range(
        &self,
        root: &[u8; 32],
        range: &Range,
        limit: Option<u64>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, DbError> {
        let (begin, end) = range.physical(root);
        let prefix_len = begin.len() - range.start.len();
        let rows = self.trx.range_raw_limited(&begin, &end, limit).await?;
        Ok(rows
            .into_iter()
            .map(|(key, value)| (key[prefix_len..].to_vec(), value))
            .collect())
    }
}
