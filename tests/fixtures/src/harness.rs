use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use aws_credential_types::Credentials;
use ed25519_dalek::SigningKey;
use aws_sdk_s3::config::{BehaviorVersion, Region};
use aws_sdk_s3::Client as S3Client;

use ech_db_client::Client;
use ech_db_protocol::errors::{DbError, DbErrorCode};
use ech_db_protocol::keys::SysKey;
use ech_db_protocol::values::{AttestedIntent, Head, ProgramBundle, SignedIntent};
use ech_db_server::config::{Config, FdbConfig, S3Config, SuiConfig, WindowConfig};
use ech_db_server::fdb::Fdb;
use ech_db_server::server::Server;
use ech_db_server::sui::Sui as SuiNode;
use ech_db_server::windows::Clock;

use crate::mock_sui::MockSui;

pub struct LiveSui {
    pub rpc_url: String,
    pub package_id: String,
    pub registry_id: String,
    pub publisher_cap_id: String,
    pub publisher_key_path: PathBuf,
}

pub struct TestEnv {
    pub fdb_cluster_file: String,
    pub s3_endpoint: String,
    pub s3_region: String,
    pub s3_bucket: String,
    pub s3_access_key: String,
    pub s3_secret_key: String,
    pub sui: Option<LiveSui>,
}

impl TestEnv {
    pub fn from_env() -> Self {
        let sui_rpc = std::env::var("ECH_TEST_SUI_RPC").ok();
        let sui_package = std::env::var("ECH_TEST_SUI_PACKAGE_ID").ok();
        let sui_registry = std::env::var("ECH_TEST_SUI_REGISTRY_ID").ok();
        let sui_cap = std::env::var("ECH_TEST_SUI_PUBLISHER_CAP_ID").ok();
        let sui_key = std::env::var("ECH_TEST_SUI_PUBLISHER_KEY").ok();
        let sui = match (sui_rpc, sui_package, sui_registry, sui_cap, sui_key) {
            (None, None, None, None, None) => None,
            (
                Some(rpc_url),
                Some(package_id),
                Some(registry_id),
                Some(publisher_cap_id),
                Some(publisher_key_path),
            ) => Some(LiveSui {
                rpc_url,
                package_id,
                registry_id,
                publisher_cap_id,
                publisher_key_path: PathBuf::from(publisher_key_path),
            }),
            _ => panic!("ECH_TEST_SUI_* must be set together"),
        };
        Self {
            fdb_cluster_file: required("ECH_TEST_FDB_CLUSTER_FILE"),
            s3_endpoint: required("ECH_TEST_S3_ENDPOINT"),
            s3_region: required("ECH_TEST_S3_REGION"),
            s3_bucket: required("ECH_TEST_S3_BUCKET"),
            s3_access_key: required("ECH_TEST_S3_ACCESS_KEY"),
            s3_secret_key: required("ECH_TEST_S3_SECRET_KEY"),
            sui,
        }
    }
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set"))
}

pub struct Harness {
    pub server: Arc<Server>,
    pub sui: SuiAccess,
    pub config: Config,
    pub grpc: Option<SocketAddr>,
    pub s3: S3Client,
    pub bucket: String,
    dir: PathBuf,
}

pub enum SuiAccess {
    Mock(MockSui),
    Live(SuiNode),
}

impl SuiAccess {
    pub async fn anchor(&self, root: &[u8; 32]) -> Option<([u8; 32], [u8; 32])> {
        match self {
            Self::Mock(mock) => mock.anchor(root).await,
            Self::Live(sui) => sui
                .anchor_of(root)
                .await
                .expect("sui anchor read")
                .map(|anchor| (anchor.prev_hash, anchor.cur_hash)),
        }
    }

    pub async fn set_anchor(&self, root: [u8; 32], prev: [u8; 32], cur: [u8; 32]) {
        match self {
            Self::Mock(mock) => mock.set_anchor(root, prev, cur).await,
            Self::Live(sui) => sui.anchor(&root, &prev, &cur).await.expect("sui anchor write"),
        }
    }

    pub async fn wait_anchor(&self, root: &[u8; 32], prev: [u8; 32], cur: [u8; 32]) {
        for _ in 0..100 {
            if self.anchor(root).await == Some((prev, cur)) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("sui anchor did not settle");
    }
}

fn boot_fdb() {
    static NETWORK: std::sync::OnceLock<foundationdb::api::NetworkAutoStop> = std::sync::OnceLock::new();
    NETWORK.get_or_init(|| unsafe { foundationdb::boot() });
}

impl Harness {
    pub async fn start(env: TestEnv) -> Self {
        let dir = std::env::temp_dir().join(format!("ech-db-tests-{}", hex::encode(rand::random::<[u8; 8]>())));
        std::fs::create_dir_all(&dir).unwrap();
        let (sui, sui_config) = match &env.sui {
            Some(live) => {
                let config = SuiConfig {
                    rpc_url: live.rpc_url.clone(),
                    package_id: live.package_id.clone(),
                    registry_id: live.registry_id.clone(),
                    publisher_cap_id: live.publisher_cap_id.clone(),
                    publisher_key_path: live.publisher_key_path.clone(),
                };
                (
                    SuiAccess::Live(SuiNode::new(&config).unwrap()),
                    config,
                )
            }
            None => {
                let mock = MockSui::start().await;
                let sui_key_path = dir.join("sui.key");
                std::fs::write(&sui_key_path, hex::encode(rand::random::<[u8; 32]>())).unwrap();
                let config = SuiConfig {
                    rpc_url: mock.url(),
                    package_id: format!("0x{}", hex::encode([0x11u8; 32])),
                    registry_id: format!("0x{}", hex::encode([0x22u8; 32])),
                    publisher_cap_id: format!("0x{}", hex::encode([0x33u8; 32])),
                    publisher_key_path: sui_key_path,
                };
                (SuiAccess::Mock(mock), config)
            }
        };
        let config = Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            fdb: FdbConfig {
                cluster_file: env.fdb_cluster_file.clone(),
            },
            window: WindowConfig { length_secs: 600 },
            s3: S3Config {
                endpoint: env.s3_endpoint.clone(),
                region: env.s3_region.clone(),
                bucket: env.s3_bucket.clone(),
                prefix: format!("test-{}", hex::encode(rand::random::<[u8; 8]>())),
                access_key: env.s3_access_key.clone(),
                secret_key: env.s3_secret_key.clone(),
            },
            sui: sui_config,
        };
        ensure_bucket(&config.s3).await;
        let s3 = s3_client(&config.s3);
        boot_fdb();
        let fdb = Fdb::open(&config.fdb).unwrap();
        let server = Server::new(config.clone(), fdb).unwrap();
        Self {
            server,
            sui,
            config,
            grpc: None,
            s3,
            bucket: env.s3_bucket.clone(),
            dir,
        }
    }

    pub async fn start_grpc(&mut self) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = self.server.clone();
        tokio::spawn(async move {
            server.run_with_listener(listener).await.unwrap();
        });
        self.grpc = Some(address);
        address
    }

    pub fn client(&self, seed: [u8; 32]) -> Client {
        let address = self.grpc.expect("grpc server is not started");
        Client::new(seed).with_endpoint(format!("http://localhost:{}", address.port()))
    }

    pub fn bundle(&self, wat: &str) -> ProgramBundle {
        ProgramBundle {
            format_version: 1,
            execution_profile: 1,
            wasm: wat::parse_str(wat).unwrap(),
        }
    }

    pub fn counter_bundle(&self) -> ProgramBundle {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/wasm32-unknown-unknown/release/counter_program.wasm");
        let wasm = std::fs::read(&path).unwrap_or_else(|_| {
            panic!(
                "missing {}: run cargo build -p counter-program --release --target wasm32-unknown-unknown",
                path.display()
            )
        });
        ProgramBundle {
            format_version: 1,
            execution_profile: 1,
            wasm,
        }
    }

    pub async fn append(
        &self,
        root: [u8; 32],
        intent: SignedIntent,
        signer: &SigningKey,
    ) -> Result<Vec<u8>, DbError> {
        let attested = AttestedIntent::attest(&intent, signer, Vec::new(), Clock::now_unix_ms())
            .map_err(|_| DbError::never(DbErrorCode::Integrity))?;
        self.server.append(root, attested).await.map(|result| result.output)
    }

    pub async fn force_close_window(&self, root: [u8; 32]) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let trx = self.server.fdb.trx().unwrap();
        let head: Head = trx.get_bcs(&SysKey::head(&root)).await.unwrap().unwrap();
        let forced = Head {
            number: head.number,
            closes_at_unix_ms: now.saturating_sub(1),
        };
        trx.set_bcs(&SysKey::head(&root), &forced).unwrap();
        trx.commit().await.unwrap();
        self.server.close_windows(root).await.unwrap();
    }

    pub fn data_dir(&self) -> PathBuf {
        self.dir.clone()
    }

    pub async fn s3_get(&self, key: &str) -> Option<Vec<u8>> {
        let output = self
            .s3
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await;
        match output {
            Ok(output) => {
                let data = output.body.collect().await.unwrap();
                Some(data.into_bytes().to_vec())
            }
            Err(error) => {
                let missing = error
                    .raw_response()
                    .map(|response| response.status().as_u16() == 404)
                    .unwrap_or(false);
                if missing {
                    None
                } else {
                    panic!("s3 get: {error:?}");
                }
            }
        }
    }
}

fn s3_client(config: &S3Config) -> S3Client {
    let credentials = Credentials::new(
        config.access_key.clone(),
        config.secret_key.clone(),
        None,
        None,
        "ech-db-tests",
    );
    let s3_config = aws_sdk_s3::Config::builder()
        .endpoint_url(&config.endpoint)
        .region(Region::new(config.region.clone()))
        .credentials_provider(credentials)
        .force_path_style(true)
        .behavior_version(BehaviorVersion::latest())
        .build();
    S3Client::from_conf(s3_config)
}

async fn ensure_bucket(config: &S3Config) {
    let client = s3_client(config);
    match client
        .create_bucket()
        .bucket(&config.bucket)
        .send()
        .await
    {
        Ok(_) => {}
        Err(error) => {
            let existed = error
                .raw_response()
                .map(|response| response.status().as_u16() == 409)
                .unwrap_or(false);
            if !existed {
                panic!("create bucket: {error:?}");
            }
        }
    }
}
