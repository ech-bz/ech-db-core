use std::sync::Arc;

use serde::de::DeserializeOwned;
use tokio::sync::Mutex;

use ech_db_protocol::errors::CodecError;
use ech_db_protocol::keys::{Range, ReadKey};
use ech_db_protocol::wire::Request as DbRequest;
use ech_db_protocol::wire::Response as DbResponse;

use crate::error::Error;
use crate::session::Exchange;

#[derive(Clone)]
pub struct ReadSession {
    exchange: Arc<Mutex<Exchange>>,
}

impl ReadSession {
    pub(crate) fn new(exchange: Exchange) -> Self {
        Self {
            exchange: Arc::new(Mutex::new(exchange)),
        }
    }

    pub(crate) async fn close(&self) -> Result<(), Error> {
        let mut exchange = self.exchange.lock().await;
        match exchange.request(DbRequest::CloseRead).await? {
            DbResponse::ReadClosed => Ok(()),
            _ => Err(Error::Protocol),
        }
    }

    pub async fn get_raw(&self, keys: &[ReadKey]) -> Result<Vec<Option<Vec<u8>>>, Error> {
        let mut exchange = self.exchange.lock().await;
        match exchange.request(DbRequest::Get(keys.to_vec())).await? {
            DbResponse::Values(values) => Ok(values),
            _ => Err(Error::Protocol),
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, keys: &[ReadKey]) -> Result<Vec<Option<T>>, Error> {
        let values = self.get_raw(keys).await?;
        values
            .into_iter()
            .map(|value| match value {
                Some(bytes) => Ok(Some(bcs::from_bytes(&bytes).map_err(CodecError::from)?)),
                None => Ok(None),
            })
            .collect()
    }

    pub async fn get_range_raw(&self, range: &Range) -> Result<Vec<(Vec<u8>, Vec<u8>)>, Error> {
        self.get_range_raw_limited(range, None).await
    }

    pub async fn get_range_raw_limited(
        &self,
        range: &Range,
        limit: Option<u64>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, Error> {
        let mut exchange = self.exchange.lock().await;
        match exchange.request(DbRequest::GetRange(range.clone(), limit)).await? {
            DbResponse::RangeValues(values) => Ok(values),
            _ => Err(Error::Protocol),
        }
    }

    pub async fn get_range<T: DeserializeOwned>(
        &self,
        range: &Range,
    ) -> Result<Vec<(Vec<u8>, T)>, Error> {
        let values = self.get_range_raw(range).await?;
        values
            .into_iter()
            .map(|(key, value)| Ok((key, bcs::from_bytes(&value).map_err(CodecError::from)?)))
            .collect()
    }
}
