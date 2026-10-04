use ech_db_protocol::errors::{CodecError, DbError, SignError};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("db: {0}")]
    Db(#[from] DbError),
    #[error("transport: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("status: {0}")]
    Status(#[from] tonic::Status),
    #[error("codec: {0}")]
    Codec(#[from] CodecError),
    #[error("sign: {0}")]
    Sign(#[from] SignError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol violation")]
    Protocol,
    #[error("page limit out of range")]
    PageLimit,
    #[error("stream closed")]
    StreamClosed,
    #[error("endpoint is not configured")]
    NoEndpoint,
}

impl Error {
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Db(error) => error.is_retryable(),
            Error::Transport(_) => true,
            Error::Status(status) => Self::status_retryable(status),
            Error::StreamClosed => true,
            Error::Codec(_) | Error::Sign(_) | Error::Io(_) | Error::Protocol | Error::PageLimit | Error::NoEndpoint => false,
        }
    }

    fn status_retryable(status: &tonic::Status) -> bool {
        matches!(
            status.code(),
            tonic::Code::Unavailable
                | tonic::Code::DeadlineExceeded
                | tonic::Code::Aborted
                | tonic::Code::ResourceExhausted
                | tonic::Code::Cancelled
        )
    }
}
