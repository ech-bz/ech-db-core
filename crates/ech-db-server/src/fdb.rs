use bcs::{from_bytes, to_bytes};
use foundationdb::options::MutationType;
use foundationdb::{Database, FdbError, RangeOption, Transaction};
use futures::TryStreamExt;
use serde::de::DeserializeOwned;
use serde::Serialize;

use ech_db_protocol::errors::{DbError, DbErrorCode, RetryClass};

use crate::config::FdbConfig;

pub struct Fdb {
    db: Database,
}

impl Fdb {
    pub fn open(config: &FdbConfig) -> Result<Self, FdbError> {
        Ok(Self {
            db: Database::new(Some(&config.cluster_file))?,
        })
    }

    pub fn trx(&self) -> Result<Trx, DbError> {
        Ok(Trx {
            inner: self.db.create_trx().map_err(FdbErrors::classify)?,
        })
    }
}

pub struct FdbErrors;

impl FdbErrors {
    pub fn classify(err: FdbError) -> DbError {
        if err.is_maybe_committed() {
            DbError::fdb(err.code(), RetryClass::UnknownOutcome)
        } else if err.is_retryable() {
            DbError::fdb(err.code(), RetryClass::Retry)
        } else {
            DbError::fdb(err.code(), RetryClass::Never)
        }
    }
}

pub struct Trx {
    pub inner: Transaction,
}

impl Trx {
    pub async fn get_raw(&self, key: &[u8]) -> Result<Option<Vec<u8>>, DbError> {
        self.inner
            .get(key, false)
            .await
            .map(|value| value.map(|slice| slice.to_vec()))
            .map_err(FdbErrors::classify)
    }

    pub async fn get_bcs<T: DeserializeOwned>(&self, key: &[u8]) -> Result<Option<T>, DbError> {
        match self.get_raw(key).await? {
            None => Ok(None),
            Some(bytes) => from_bytes::<T>(&bytes)
                .map(Some)
                .map_err(|_| DbError::never(DbErrorCode::Integrity)),
        }
    }

    pub fn set_raw(&self, key: &[u8], value: &[u8]) {
        self.inner.set(key, value);
    }

    pub fn set_bcs<T: Serialize>(&self, key: &[u8], value: &T) -> Result<(), DbError> {
        let bytes = to_bytes(value).map_err(|_| DbError::never(DbErrorCode::Integrity))?;
        self.inner.set(key, &bytes);
        Ok(())
    }

    pub fn clear(&self, key: &[u8]) {
        self.inner.clear(key);
    }

    pub fn clear_range(&self, begin: &[u8], end: &[u8]) {
        self.inner.clear_range(begin, end);
    }

    pub fn set_versionstamped_key(&self, key: &[u8], value: &[u8]) {
        self.inner
            .atomic_op(key, value, MutationType::SetVersionstampedKey);
    }

    pub async fn range_raw(&self, begin: &[u8], end: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, DbError> {
        self.range_raw_limited(begin, end, None).await
    }

    pub async fn range_raw_limited(
        &self,
        begin: &[u8],
        end: &[u8],
        limit: Option<u64>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, DbError> {
        let mut option = RangeOption::from((begin.to_vec(), end.to_vec()));
        if let Some(limit) = limit {
            let limit = usize::try_from(limit).map_err(|_| DbError::resource_limit())?;
            option.limit = Some(limit);
        }
        let mut stream = self.inner.get_ranges_keyvalues(option, false);
        let mut rows = Vec::new();
        while let Some(kv) = stream.try_next().await.map_err(FdbErrors::classify)? {
            rows.push((kv.key().to_vec(), kv.value().to_vec()));
            if let Some(limit) = limit {
                if rows.len() as u64 >= limit {
                    break;
                }
            }
        }
        Ok(rows)
    }

    pub async fn commit(self) -> Result<(), DbError> {
        self.inner
            .commit()
            .await
            .map(|_| ())
            .map_err(|error| FdbErrors::classify(FdbError::from(error)))
    }
}
