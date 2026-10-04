use std::net::SocketAddr;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config io: {0}")]
    Io(#[from] std::io::Error),
    #[error("config parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("window length_secs is out of range")]
    Window,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub fdb: FdbConfig,
    pub window: WindowConfig,
    pub s3: S3Config,
    pub sui: SuiConfig,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&raw)?;
        if config.window.length_secs == 0 || config.window.length_secs > u64::MAX / 1000 {
            return Err(ConfigError::Window);
        }
        Ok(config)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowConfig {
    pub length_secs: u64,
}

impl WindowConfig {
    pub fn length_ms(&self) -> u64 {
        self.length_secs * 1000
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FdbConfig {
    pub cluster_file: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub access_key: String,
    pub secret_key: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuiConfig {
    pub rpc_url: String,
    pub package_id: String,
    pub registry_id: String,
    pub publisher_cap_id: String,
    pub publisher_key_path: std::path::PathBuf,
}
