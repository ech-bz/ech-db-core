use std::collections::HashMap;
use std::sync::Arc;

use sui_rpc::proto::sui::rpc::v2 as rpc;
use sui_sdk_types::{Input, Transaction, TransactionKind};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_stream::wrappers::TcpListenerStream;

type AnchorState = Arc<Mutex<HashMap<[u8; 32], ([u8; 32], [u8; 32])>>>;

const REGISTRY: [u8; 32] = [0x22; 32];
const TABLE: [u8; 32] = [0x24; 32];
const CAP: [u8; 32] = [0x33; 32];
const CAP_DIGEST: [u8; 32] = [0x55; 32];
const COIN: [u8; 32] = [0x66; 32];
const COIN_DIGEST: [u8; 32] = [0x77; 32];
const SENDER: [u8; 32] = [0x44; 32];

pub struct MockSui {
    url: String,
    state: AnchorState,
}

impl MockSui {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state: AnchorState = Arc::new(Mutex::new(HashMap::new()));
        let ledger = Ledger;
        let state_service = StateService {
            state: state.clone(),
        };
        let execution = Execution {
            state: state.clone(),
        };
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(rpc::ledger_service_server::LedgerServiceServer::new(
                    ledger,
                ))
                .add_service(rpc::state_service_server::StateServiceServer::new(
                    state_service,
                ))
                .add_service(
                    rpc::transaction_execution_service_server::TransactionExecutionServiceServer::new(
                        execution,
                    ),
                )
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
                .unwrap();
        });
        Self {
            url: format!("http://{address}"),
            state,
        }
    }

    pub fn url(&self) -> String {
        self.url.clone()
    }

    pub async fn anchor(&self, root: &[u8; 32]) -> Option<([u8; 32], [u8; 32])> {
        let state = self.state.lock().await;
        state.get(root).copied()
    }

    pub async fn set_anchor(&self, root: [u8; 32], prev: [u8; 32], cur: [u8; 32]) {
        let mut state = self.state.lock().await;
        state.insert(root, (prev, cur));
    }
}

struct Ledger;

struct StateService {
    state: AnchorState,
}

struct Execution {
    state: AnchorState,
}

#[tonic::async_trait]
impl rpc::ledger_service_server::LedgerService for Ledger {
    async fn get_object(
        &self,
        request: tonic::Request<rpc::GetObjectRequest>,
    ) -> Result<tonic::Response<rpc::GetObjectResponse>, tonic::Status> {
        let requested = request.into_inner().object_id.unwrap_or_default();
        let requested = requested.trim_start_matches("0x").to_ascii_lowercase();
        let mut owner = rpc::Owner::default();
        let mut object = rpc::Object::default();
        object.object_id = Some(format!("0x{requested}"));
        object.version = Some(1);
        if requested == hex::encode(REGISTRY) {
            owner.kind = Some(rpc::owner::OwnerKind::Shared as i32);
            owner.version = Some(1);
            object.digest = Some(digest_string(&CAP_DIGEST));
            let mut contents = Vec::with_capacity(72);
            contents.extend_from_slice(&REGISTRY);
            contents.extend_from_slice(&TABLE);
            contents.extend_from_slice(&0u64.to_le_bytes());
            let mut bcs = rpc::Bcs::default();
            bcs.name = Some("ech_anchor::registry::Registry".to_string());
            bcs.value = Some(contents.into());
            object.contents = Some(bcs);
        } else if requested == hex::encode(CAP) {
            owner.kind = Some(rpc::owner::OwnerKind::Address as i32);
            owner.address = Some(hex_string(&SENDER));
            object.digest = Some(digest_string(&CAP_DIGEST));
        } else {
            return Err(tonic::Status::not_found("object not found"));
        }
        object.owner = Some(owner);
        let mut response = rpc::GetObjectResponse::default();
        response.object = Some(object);
        Ok(tonic::Response::new(response))
    }

    async fn get_epoch(
        &self,
        _request: tonic::Request<rpc::GetEpochRequest>,
    ) -> Result<tonic::Response<rpc::GetEpochResponse>, tonic::Status> {
        let mut epoch = rpc::Epoch::default();
        epoch.epoch = Some(0);
        epoch.reference_gas_price = Some(1000);
        let mut response = rpc::GetEpochResponse::default();
        response.epoch = Some(epoch);
        Ok(tonic::Response::new(response))
    }
}

#[tonic::async_trait]
impl rpc::state_service_server::StateService for StateService {
    async fn list_dynamic_fields(
        &self,
        _request: tonic::Request<rpc::ListDynamicFieldsRequest>,
    ) -> Result<tonic::Response<rpc::ListDynamicFieldsResponse>, tonic::Status> {
        let state = self.state.lock().await;
        let mut dynamic_fields = Vec::new();
        for (root, (prev, cur)) in state.iter() {
            let stored = AnchorValue {
                prev_hash: prev.to_vec(),
                cur_hash: cur.to_vec(),
            };
            let mut name = rpc::Bcs::default();
            name.name = Some("vector<u8>".to_string());
            name.value = Some(bcs::to_bytes(&root.to_vec()).unwrap().into());
            let mut value = rpc::Bcs::default();
            value.name = Some("anchor::Anchor".to_string());
            value.value = Some(bcs::to_bytes(&stored).unwrap().into());
            let mut field = rpc::DynamicField::default();
            field.parent = Some(hex_string(&TABLE));
            field.field_id = Some(hex_string(root));
            field.name = Some(name);
            field.value = Some(value);
            dynamic_fields.push(field);
        }
        let mut response = rpc::ListDynamicFieldsResponse::default();
        response.dynamic_fields = dynamic_fields;
        Ok(tonic::Response::new(response))
    }

    async fn list_owned_objects(
        &self,
        _request: tonic::Request<rpc::ListOwnedObjectsRequest>,
    ) -> Result<tonic::Response<rpc::ListOwnedObjectsResponse>, tonic::Status> {
        let mut coin = rpc::Object::default();
        coin.object_id = Some(hex_string(&COIN));
        coin.version = Some(1);
        coin.digest = Some(digest_string(&COIN_DIGEST));
        let mut response = rpc::ListOwnedObjectsResponse::default();
        response.objects = vec![coin];
        Ok(tonic::Response::new(response))
    }
}

#[tonic::async_trait]
impl rpc::transaction_execution_service_server::TransactionExecutionService for Execution {
    async fn execute_transaction(
        &self,
        request: tonic::Request<rpc::ExecuteTransactionRequest>,
    ) -> Result<tonic::Response<rpc::ExecuteTransactionResponse>, tonic::Status> {
        let request = request.into_inner();
        let Some(transaction) = request.transaction else {
            return Err(tonic::Status::invalid_argument("transaction missing"));
        };
        let Ok(transaction) = Transaction::try_from(&transaction) else {
            return Err(tonic::Status::invalid_argument("transaction invalid"));
        };
        let Some((root, prev, cur)) = anchor_call(&transaction) else {
            return Err(tonic::Status::invalid_argument("anchor call invalid"));
        };
        let mut state = self.state.lock().await;
        match state.get(&root).copied() {
            None => {
                if prev != [0u8; 32] {
                    return Ok(tonic::Response::new(failure()));
                }
                state.insert(root, (prev, cur));
                Ok(tonic::Response::new(success()))
            }
            Some(stored) => {
                if stored.0 == prev && stored.1 == cur {
                    return Ok(tonic::Response::new(success()));
                }
                if stored.1 == prev {
                    state.insert(root, (prev, cur));
                    return Ok(tonic::Response::new(success()));
                }
                Ok(tonic::Response::new(failure()))
            }
        }
    }
}

#[derive(serde::Serialize)]
struct AnchorValue {
    prev_hash: Vec<u8>,
    cur_hash: Vec<u8>,
}

fn anchor_call(transaction: &Transaction) -> Option<([u8; 32], [u8; 32], [u8; 32])> {
    let TransactionKind::ProgrammableTransaction(programmable) = &transaction.kind else {
        return None;
    };
    let mut pure = Vec::new();
    for input in &programmable.inputs {
        if let Input::Pure(bytes) = input {
            pure.push(bcs::from_bytes::<Vec<u8>>(bytes).ok()?);
        }
    }
    if pure.len() != 3 {
        return None;
    }
    let root = pure[0].as_slice().try_into().ok()?;
    let prev = pure[1].as_slice().try_into().ok()?;
    let cur = pure[2].as_slice().try_into().ok()?;
    Some((root, prev, cur))
}

fn success() -> rpc::ExecuteTransactionResponse {
    let mut status = rpc::ExecutionStatus::default();
    status.success = Some(true);
    response(status)
}

fn failure() -> rpc::ExecuteTransactionResponse {
    let mut abort = rpc::MoveAbort::default();
    abort.abort_code = Some(1);
    let mut error = rpc::ExecutionError::default();
    error.description = Some(
        "MoveAbort(MoveLocation { module: ModuleId { address: 0x0, name: \"registry\" }, function: 2, instruction: 10, function_name: Some(\"anchor\") }, 1) in command 0"
            .to_string(),
    );
    error.kind = Some(rpc::execution_error::ExecutionErrorKind::MoveAbort as i32);
    error.error_details = Some(rpc::execution_error::ErrorDetails::Abort(abort));
    let mut status = rpc::ExecutionStatus::default();
    status.success = Some(false);
    status.error = Some(error);
    response(status)
}

fn response(status: rpc::ExecutionStatus) -> rpc::ExecuteTransactionResponse {
    let mut effects = rpc::TransactionEffects::default();
    effects.status = Some(status);
    let mut executed = rpc::ExecutedTransaction::default();
    executed.effects = Some(effects);
    let mut response = rpc::ExecuteTransactionResponse::default();
    response.transaction = Some(executed);
    response
}

fn hex_string(bytes: &[u8; 32]) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn digest_string(bytes: &[u8; 32]) -> String {
    sui_sdk_types::Digest::from(*bytes).to_string()
}
