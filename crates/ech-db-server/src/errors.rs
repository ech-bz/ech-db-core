#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config: {0}")]
    Config(#[from] crate::config::ConfigError),
    #[error("fdb: {0}")]
    Fdb(#[from] ::foundationdb::FdbError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("transport: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("sui: {0}")]
    Sui(#[from] crate::sui::SuiError),
    #[error("s3: {0}")]
    S3(#[from] crate::s3::S3Error),
}
