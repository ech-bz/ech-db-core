use std::future::Future;

use ed25519_dalek::{Signer, SigningKey};
use tokio::sync::Mutex;
use tonic::transport::{Channel, Endpoint};

use ech_db_protocol::errors::CodecError;
use ech_db_protocol::values::{AttestedIntent, IntentBody, ProgramBundle, SignedIntent};
use ech_db_protocol::wire::{AppendResult, Auth, Request as DbRequest, Response as DbResponse};

use crate::call::Call;
use crate::error::Error;
use crate::read::ReadSession;
use crate::retry::Backoff;
use crate::session::Exchange;

pub struct Client {
    signing: SigningKey,
    pubkey: [u8; 32],
    endpoint: Option<String>,
    channel: Mutex<Option<Channel>>,
}

impl Client {
    pub fn new(privkey: [u8; 32]) -> Self {
        let signing = SigningKey::from_bytes(&privkey);
        let pubkey = signing.verifying_key().to_bytes();
        Self {
            signing,
            pubkey,
            endpoint: None,
            channel: Mutex::new(None),
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    pub fn root(&self) -> [u8; 32] {
        self.pubkey
    }

    async fn channel(&self) -> Result<Channel, Error> {
        let mut channel = self.channel.lock().await;
        if let Some(channel) = channel.as_ref() {
            return Ok(channel.clone());
        }
        let endpoint = self.endpoint.as_ref().ok_or(Error::NoEndpoint)?;
        let endpoint = Endpoint::from_shared(endpoint.clone())?;
        let connected = endpoint.connect().await?;
        *channel = Some(connected.clone());
        Ok(connected)
    }

    fn auth(&self) -> Auth {
        let signature = self.signing.sign(&self.pubkey);
        Auth {
            pubkey: self.pubkey,
            signature: signature.to_bytes(),
        }
    }

    async fn exchange(&self) -> Result<Exchange, Error> {
        let channel = self.channel().await?;
        Exchange::connect(&channel, self.auth()).await
    }

    async fn with_retry<F, Fut, T>(&self, mut operation: F) -> Result<T, Error>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, Error>>,
    {
        let mut backoff = Backoff::new();
        loop {
            match operation().await {
                Ok(value) => return Ok(value),
                Err(error) if error.is_retryable() => {
                    tokio::time::sleep(backoff.delay()).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub async fn upload_program(&self, bundle: ProgramBundle) -> Result<[u8; 32], Error> {
        self.with_retry(|| async {
            let mut exchange = self.exchange().await?;
            match exchange.request(DbRequest::UploadProgram(bundle.clone())).await? {
                DbResponse::ProgramUploaded(hash) => Ok(hash),
                _ => Err(Error::Protocol),
            }
        })
        .await
    }

    pub async fn get_program(&self, hash: [u8; 32]) -> Result<ProgramBundle, Error> {
        self.with_retry(|| async {
            let mut exchange = self.exchange().await?;
            match exchange.request(DbRequest::GetProgram(hash)).await? {
                DbResponse::Program(bundle) => Ok(bundle),
                _ => Err(Error::Protocol),
            }
        })
        .await
    }

    pub fn sign_intent(
        &self,
        expected_seq: u64,
        tweak: [u8; 32],
        call: &Call,
    ) -> Result<SignedIntent, Error> {
        let body = IntentBody {
            version: IntentBody::FORMAT_VERSION,
            root: self.pubkey,
            author: self.pubkey,
            tweak,
            expected_seq,
            call: call.invocation(),
        };
        Ok(SignedIntent::sign(body, &self.signing)?)
    }

    pub async fn append<T>(&self, intent: &SignedIntent, responses: Vec<u8>) -> Result<T, Error>
    where
        T: serde::de::DeserializeOwned,
    {
        let result = self.append_raw(intent, responses).await?;
        Ok(bcs::from_bytes(&result.output).map_err(CodecError::from)?)
    }

    pub async fn append_raw(&self, intent: &SignedIntent, responses: Vec<u8>) -> Result<AppendResult, Error> {
        let attested =
            AttestedIntent::attest(intent, &self.signing, responses, Self::timestamp_ms()?)?;
        self.with_retry(|| async {
            let mut exchange = self.exchange().await?;
            match exchange.request(DbRequest::Append(attested.clone())).await? {
                DbResponse::Appended(result) => Ok(result),
                _ => Err(Error::Protocol),
            }
        })
        .await
    }

    fn timestamp_ms() -> Result<u64, Error> {
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Protocol)?;
        u64::try_from(elapsed.as_millis()).map_err(|_| Error::Protocol)
    }

    pub async fn read<F, Fut, T>(&self, callback: F) -> Result<T, Error>
    where
        F: Fn(ReadSession) -> Fut + Send + Sync,
        Fut: Future<Output = Result<T, Error>> + Send,
    {
        let mut backoff = Backoff::new();
        loop {
            match self.read_once(&callback).await {
                Ok(value) => return Ok(value),
                Err(error) if error.is_retryable() => {
                    tokio::time::sleep(backoff.delay()).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn read_once<F, Fut, T>(&self, callback: &F) -> Result<T, Error>
    where
        F: Fn(ReadSession) -> Fut,
        Fut: Future<Output = Result<T, Error>>,
    {
        let mut exchange = self.exchange().await?;
        match exchange.request(DbRequest::BeginRead).await? {
            DbResponse::ReadOpened => {}
            _ => return Err(Error::Protocol),
        }
        let session = ReadSession::new(exchange);
        let value = callback(session.clone()).await?;
        session.close().await?;
        Ok(value)
    }
}
