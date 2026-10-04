use prost_types::FieldMask;
use sui_crypto::ed25519::Ed25519PrivateKey;
use sui_crypto::SuiSigner;
use sui_rpc::field::FieldMaskUtil;
use sui_rpc::proto::sui::rpc::v2 as rpc;
use sui_sdk_types::{Address, Digest, Identifier, Transaction};
use sui_transaction_builder::{Function, ObjectInput, TransactionBuilder};

use ech_db_protocol::hash::Blake2b256;

use crate::config::SuiConfig;

pub const ANCHOR_CONFLICT: u64 = 1;

const SUI_COIN_TYPE: &str = "0x2::coin::Coin<0x2::sui::SUI>";
const PAGE_SIZE: u32 = 50;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Anchor {
    pub prev_hash: [u8; 32],
    pub cur_hash: [u8; 32],
}

#[derive(Debug, thiserror::Error)]
pub enum SuiError {
    #[error("sui io: {0}")]
    Io(#[from] std::io::Error),
    #[error("sui publisher key is invalid")]
    Key,
    #[error("sui transport: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("sui rpc: {0}")]
    Rpc(tonic::Status),
    #[error("sui transaction inputs are stale")]
    Stale,
    #[error("sui response: {0}")]
    Response(String),
    #[error("sui transaction build: {0}")]
    Build(String),
    #[error("sui anchor conflict")]
    AnchorConflict,
    #[error("sui transaction failed: {0}")]
    TransactionFailed(String),
}

impl SuiError {
    pub fn is_retryable(&self) -> bool {
        match self {
            SuiError::Transport(_) | SuiError::Stale => true,
            SuiError::Rpc(status) => matches!(
                status.code(),
                tonic::Code::Unavailable
                    | tonic::Code::DeadlineExceeded
                    | tonic::Code::ResourceExhausted
                    | tonic::Code::Aborted
                    | tonic::Code::Cancelled
                    | tonic::Code::Internal
                    | tonic::Code::Unknown
            ),
            SuiError::Io(_)
            | SuiError::Key
            | SuiError::Response(_)
            | SuiError::Build(_)
            | SuiError::AnchorConflict
            | SuiError::TransactionFailed(_) => false,
        }
    }
}

pub struct SuiErrors;

impl SuiErrors {
    pub fn db(error: &SuiError) -> ech_db_protocol::errors::DbError {
        if error.is_retryable() {
            ech_db_protocol::errors::DbError::unavailable()
        } else {
            ech_db_protocol::errors::DbError::integrity()
        }
    }
}

pub struct Sui {
    client: sui_rpc::client::Client,
    package: Address,
    registry: Address,
    registry_id: String,
    cap: Address,
    cap_id: String,
    key: Ed25519PrivateKey,
    sender: Address,
    sender_id: String,
}

#[derive(serde::Deserialize)]
struct AnchorValue {
    prev_hash: Vec<u8>,
    cur_hash: Vec<u8>,
}

impl Sui {
    const GAS_BUDGET: u64 = 50_000_000;
    const REBUILD_ATTEMPTS: usize = 5;

    pub fn new(config: &SuiConfig) -> Result<Self, SuiError> {
        let raw = std::fs::read_to_string(&config.publisher_key_path)?;
        let key_hex = raw.trim().trim_start_matches("0x").to_string();
        let seed: [u8; 32] = parse_hex(&key_hex)?;
        let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
        let verifying = signing.verifying_key().to_bytes();
        let sender = Address::new(Blake2b256::hash(&[&[0x00u8], verifying.as_slice()]));
        let endpoint = tonic::transport::Endpoint::from_shared(config.rpc_url.clone())?;
        let endpoint = endpoint.tls_config(
            tonic::transport::ClientTlsConfig::new().with_webpki_roots(),
        )?;
        Ok(Self {
            client: sui_rpc::client::Client::from_endpoint(&endpoint),
            package: parse_address(&config.package_id)?,
            registry: parse_address(&config.registry_id)?,
            registry_id: config.registry_id.clone(),
            cap: parse_address(&config.publisher_cap_id)?,
            cap_id: config.publisher_cap_id.clone(),
            key: Ed25519PrivateKey::new(seed),
            sender,
            sender_id: format!("0x{}", hex::encode(sender.as_bytes())),
        })
    }

    pub async fn anchor_of(&self, root: &[u8; 32]) -> Result<Option<Anchor>, SuiError> {
        let table = self.anchor_table().await?;
        let name = bcs::to_bytes(&root.to_vec()).map_err(|_| SuiError::Response("name bcs".into()))?;
        let read_mask = FieldMask::from_paths(["name", "value"]);
        let mut page_token = None;
        loop {
            let mut request = rpc::ListDynamicFieldsRequest::default();
            request.parent = Some(table.clone());
            request.page_size = Some(PAGE_SIZE);
            request.page_token = page_token.take();
            request.read_mask = Some(read_mask.clone());
            let response = self
                .client
                .clone()
                .state_client()
                .list_dynamic_fields(request)
                .await
                .map_err(SuiError::Rpc)?
                .into_inner();
            for field in response.dynamic_fields {
                let stored_name = field.name.and_then(|name| name.value);
                if stored_name.as_deref() != Some(name.as_slice()) {
                    continue;
                }
                let value = field
                    .value
                    .and_then(|value| value.value)
                    .ok_or_else(|| SuiError::Response("anchor value missing".into()))?;
                let stored: AnchorValue = bcs::from_bytes(&value)
                    .map_err(|_| SuiError::Response("anchor value decode".into()))?;
                return Ok(Some(Anchor {
                    prev_hash: parse_hash(&stored.prev_hash)?,
                    cur_hash: parse_hash(&stored.cur_hash)?,
                }));
            }
            page_token = response.next_page_token;
            if page_token.is_none() {
                return Ok(None);
            }
        }
    }

    pub async fn anchor(
        &self,
        root: &[u8; 32],
        prev_hash: &[u8; 32],
        cur_hash: &[u8; 32],
    ) -> Result<(), SuiError> {
        let mut attempts = 0;
        loop {
            attempts += 1;
            match self.anchor_once(root, prev_hash, cur_hash).await {
                Err(SuiError::Stale) if attempts < Self::REBUILD_ATTEMPTS => {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
                result => return result,
            }
        }
    }

    async fn anchor_once(
        &self,
        root: &[u8; 32],
        prev_hash: &[u8; 32],
        cur_hash: &[u8; 32],
    ) -> Result<(), SuiError> {
        let transaction = self.build_anchor(root, prev_hash, cur_hash).await?;
        let signature = self
            .key
            .sign_transaction(&transaction)
            .map_err(|_| SuiError::Key)?;
        let mut request = rpc::ExecuteTransactionRequest::default();
        request.transaction = Some(rpc::Transaction::from(transaction));
        request.signatures = vec![rpc::UserSignature::from(signature)];
        request.read_mask = Some(FieldMask::from_paths(["effects.status"]));
        let response = self
            .client
            .clone()
            .execution_client()
            .execute_transaction(request)
            .await
            .map_err(|status| {
                if status.code() == tonic::Code::InvalidArgument
                    && status.message().contains("needs to be rebuilt")
                {
                    SuiError::Stale
                } else {
                    SuiError::Rpc(status)
                }
            })?
            .into_inner();
        let status = response
            .transaction
            .and_then(|transaction| transaction.effects)
            .and_then(|effects| effects.status)
            .ok_or_else(|| SuiError::Response("effects missing".into()))?;
        if status.success == Some(true) {
            return Ok(());
        }
        let error = status
            .error
            .ok_or_else(|| SuiError::Response("error missing".into()))?;
        if let Some(rpc::execution_error::ErrorDetails::Abort(abort)) = &error.error_details {
            if abort.abort_code == Some(ANCHOR_CONFLICT) {
                return Err(SuiError::AnchorConflict);
            }
        }
        Err(SuiError::TransactionFailed(
            error.description.unwrap_or_else(|| "unknown".into()),
        ))
    }

    async fn anchor_table(&self) -> Result<String, SuiError> {
        let mut request = rpc::GetObjectRequest::default();
        request.object_id = Some(self.registry_id.clone());
        request.read_mask = Some(FieldMask::from_paths(["contents"]));
        let object = self
            .client
            .clone()
            .ledger_client()
            .get_object(request)
            .await
            .map_err(SuiError::Rpc)?
            .into_inner()
            .object
            .ok_or_else(|| SuiError::Response("registry missing".into()))?;
        let contents = object
            .contents
            .and_then(|bcs| bcs.value)
            .ok_or_else(|| SuiError::Response("registry contents missing".into()))?;
        let table: [u8; 32] = contents
            .get(32..64)
            .ok_or_else(|| SuiError::Response("registry layout".into()))?
            .try_into()
            .map_err(|_| SuiError::Response("registry layout".into()))?;
        Ok(format!("0x{}", hex::encode(table)))
    }

    async fn build_anchor(
        &self,
        root: &[u8; 32],
        prev_hash: &[u8; 32],
        cur_hash: &[u8; 32],
    ) -> Result<Transaction, SuiError> {
        let shared_version = self.shared_version().await?;
        let (cap_version, cap_digest) = self.object_ref(&self.cap_id).await?;
        let (coin, coin_version, coin_digest) = self.gas_coin().await?;
        let price = self.reference_gas_price().await?;

        let mut builder = TransactionBuilder::new();
        let registry = builder.object(ObjectInput::shared(self.registry, shared_version, true));
        let cap = builder.object(ObjectInput::owned(self.cap, cap_version, cap_digest).with_mutable(false));
        let root = builder.pure(&root.to_vec());
        let prev = builder.pure(&prev_hash.to_vec());
        let cur = builder.pure(&cur_hash.to_vec());
        let function = Function::new(
            self.package,
            identifier("registry")?,
            identifier("anchor")?,
        );
        builder.move_call(function, vec![registry, cap, root, prev, cur]);
        builder.set_sender(self.sender);
        builder.set_gas_budget(Self::GAS_BUDGET);
        builder.set_gas_price(price);
        builder.add_gas_objects([ObjectInput::owned(coin, coin_version, coin_digest)]);
        builder
            .try_build()
            .map_err(|error| SuiError::Build(error.to_string()))
    }

    async fn shared_version(&self) -> Result<u64, SuiError> {
        let object = self.object(&self.registry_id).await?;
        let owner = object
            .owner
            .ok_or_else(|| SuiError::Response("owner missing".into()))?;
        if owner.kind != Some(rpc::owner::OwnerKind::Shared as i32) {
            return Err(SuiError::Response("registry is not shared".into()));
        }
        owner
            .version
            .ok_or_else(|| SuiError::Response("shared version missing".into()))
    }

    async fn object_ref(&self, id_str: &str) -> Result<(u64, Digest), SuiError> {
        let object = self.object(id_str).await?;
        let version = object
            .version
            .ok_or_else(|| SuiError::Response("object version missing".into()))?;
        let digest = object
            .digest
            .ok_or_else(|| SuiError::Response("object digest missing".into()))?;
        Ok((version, parse_digest(&digest)?))
    }

    async fn object(&self, id: &str) -> Result<rpc::Object, SuiError> {
        let mut request = rpc::GetObjectRequest::default();
        request.object_id = Some(id.to_string());
        request.read_mask = Some(FieldMask::from_paths([
            "object_id",
            "version",
            "digest",
            "owner",
        ]));
        self.client
            .clone()
            .ledger_client()
            .get_object(request)
            .await
            .map_err(SuiError::Rpc)?
            .into_inner()
            .object
            .ok_or_else(|| SuiError::Response("object missing".into()))
    }

    async fn gas_coin(&self) -> Result<(Address, u64, Digest), SuiError> {
        let mut request = rpc::ListOwnedObjectsRequest::default();
        request.owner = Some(self.sender_id.clone());
        request.page_size = Some(PAGE_SIZE);
        request.read_mask = Some(FieldMask::from_paths(["object_id", "version", "digest"]));
        request.object_type = Some(SUI_COIN_TYPE.to_string());
        let response = self
            .client
            .clone()
            .state_client()
            .list_owned_objects(request)
            .await
            .map_err(SuiError::Rpc)?
            .into_inner();
        let coin = response
            .objects
            .into_iter()
            .next()
            .ok_or_else(|| SuiError::Response("no gas coins".into()))?;
        let id = coin
            .object_id
            .ok_or_else(|| SuiError::Response("coin id missing".into()))?;
        let version = coin
            .version
            .ok_or_else(|| SuiError::Response("coin version missing".into()))?;
        let digest = coin
            .digest
            .ok_or_else(|| SuiError::Response("coin digest missing".into()))?;
        Ok((parse_address(&id)?, version, parse_digest(&digest)?))
    }

    async fn reference_gas_price(&self) -> Result<u64, SuiError> {
        let mut request = rpc::GetEpochRequest::default();
        request.read_mask = Some(FieldMask::from_paths(["reference_gas_price"]));
        let epoch = self
            .client
            .clone()
            .ledger_client()
            .get_epoch(request)
            .await
            .map_err(SuiError::Rpc)?
            .into_inner()
            .epoch
            .ok_or_else(|| SuiError::Response("epoch missing".into()))?;
        epoch
            .reference_gas_price
            .ok_or_else(|| SuiError::Response("gas price missing".into()))
    }
}

fn identifier(name: &str) -> Result<Identifier, SuiError> {
    Identifier::new(name).map_err(|error| SuiError::Build(error.to_string()))
}

fn parse_hex(text: &str) -> Result<[u8; 32], SuiError> {
    let bytes = hex::decode(text).map_err(|_| SuiError::Key)?;
    bytes.as_slice().try_into().map_err(|_| SuiError::Key)
}

fn parse_address(text: &str) -> Result<Address, SuiError> {
    let raw = text.trim().trim_start_matches("0x");
    let bytes = hex::decode(raw).map_err(|_| SuiError::Response(format!("bad address {text}")))?;
    let bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| SuiError::Response(format!("bad address {text}")))?;
    Ok(Address::new(bytes))
}

fn parse_digest(text: &str) -> Result<Digest, SuiError> {
    text.trim()
        .parse::<Digest>()
        .map_err(|_| SuiError::Response(format!("bad digest {text}")))
}

fn parse_hash(bytes: &[u8]) -> Result<[u8; 32], SuiError> {
    bytes
        .try_into()
        .map_err(|_| SuiError::Response("hash length".into()))
}
