use aws_credential_types::Credentials;
use aws_sdk_s3::config::{BehaviorVersion, Region};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;

use crate::config::S3Config;

#[derive(Debug, thiserror::Error)]
pub enum S3Error {
    #[error("s3 object not found")]
    NotFound,
    #[error("s3 immutable object differs")]
    Conflict,
    #[error("s3 transport: {0}")]
    Transport(String),
    #[error("s3 service: {0}")]
    Service(String),
    #[error("s3 body: {0}")]
    Body(String),
}

impl S3Error {
    pub fn is_retryable(&self) -> bool {
        match self {
            S3Error::Transport(_) | S3Error::Service(_) => true,
            S3Error::NotFound | S3Error::Conflict | S3Error::Body(_) => false,
        }
    }
}

pub struct S3Errors;

impl S3Errors {
    pub fn db(error: &S3Error) -> ech_db_protocol::errors::DbError {
        if error.is_retryable() {
            ech_db_protocol::errors::DbError::unavailable()
        } else {
            ech_db_protocol::errors::DbError::integrity()
        }
    }
}

#[derive(Debug)]
pub enum PutOutcome {
    Stored,
    AlreadyPresent,
}

pub struct S3 {
    client: Client,
    bucket: String,
    prefix: String,
}

impl S3 {
    pub fn new(config: &S3Config) -> Self {
        let credentials = Credentials::new(
            config.access_key.clone(),
            config.secret_key.clone(),
            None,
            None,
            "ech-db",
        );
        let s3_config = aws_sdk_s3::Config::builder()
            .endpoint_url(&config.endpoint)
            .region(Region::new(config.region.clone()))
            .credentials_provider(credentials)
            .force_path_style(true)
            .behavior_version(BehaviorVersion::latest())
            .build();
        Self {
            client: Client::from_conf(s3_config),
            bucket: config.bucket.clone(),
            prefix: config.prefix.clone(),
        }
    }

    pub fn key(&self, root: &[u8; 32], number: u64) -> String {
        format!("{}/{}/{:016X}.bin", self.prefix, hex::encode(root), number)
    }

    pub async fn put_immutable(&self, key: &str, bytes: &[u8]) -> Result<PutOutcome, S3Error> {
        match self.get(key).await {
            Ok(existing) => {
                if existing == bytes {
                    return Ok(PutOutcome::AlreadyPresent);
                }
                return Err(S3Error::Conflict);
            }
            Err(S3Error::NotFound) => {}
            Err(error) => return Err(error),
        }
        let result = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(bytes.to_vec()))
            .send()
            .await;
        if let Err(error) = result {
            return Err(Self::service_error(&error));
        }
        let stored = self.get(key).await?;
        if stored != bytes {
            return Err(S3Error::Conflict);
        }
        Ok(PutOutcome::Stored)
    }

    pub async fn get(&self, key: &str) -> Result<Vec<u8>, S3Error> {
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| {
                let not_found = error
                    .raw_response()
                    .map(|response| response.status().as_u16() == 404)
                    .unwrap_or(false);
                if not_found {
                    S3Error::NotFound
                } else {
                    Self::service_error(&error)
                }
            })?;
        let data = output.body.collect().await.map_err(|error| S3Error::Body(error.to_string()))?;
        Ok(data.into_bytes().to_vec())
    }

    pub async fn get_optional(&self, key: &str) -> Result<Option<Vec<u8>>, S3Error> {
        match self.get(key).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(S3Error::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn service_error<E>(error: &aws_sdk_s3::error::SdkError<E>) -> S3Error
    where
        E: std::fmt::Debug,
    {
        let rendered = format!("{error:?}");
        if rendered.contains("ResponseError") || error.raw_response().is_some() {
            S3Error::Service(rendered)
        } else {
            S3Error::Transport(rendered)
        }
    }
}
