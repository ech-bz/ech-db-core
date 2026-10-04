use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use tonic::transport::Endpoint;

use ech_db_client::session::Exchange;
use ech_db_client::{Call, ReadKey};
use ech_db_protocol::errors::DbErrorCode;
use ech_db_protocol::ids::Stream;
use ech_db_protocol::values::{AttestedIntent, IntentBody, Invocation, SignedIntent};
use ech_db_protocol::wire::Auth;
use ech_db_test_fixtures::harness::{Harness, TestEnv};

fn env() -> TestEnv {
    TestEnv::from_env()
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Counter {
    value: u64,
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn grpc_client_roundtrip_with_counter_program() {
    let mut harness = Harness::start(env()).await;
    harness.start_grpc().await;
    let client = harness.client(rand::random());
    let bundle = harness.counter_bundle();
    let program_hash = client.upload_program(bundle.clone()).await.unwrap();
    let fetched = client.get_program(program_hash).await.unwrap();
    assert_eq!(fetched.wasm, bundle.wasm);

    let first_intent = client
        .sign_intent(1, [0u8; 32], &Call::new(program_hash, "apply").with_args(&5u64).unwrap())
        .unwrap();
    let first: Counter = client.append(&first_intent, Vec::new()).await.unwrap();
    assert_eq!(first.value, 5);
    let second_intent = client
        .sign_intent(2, [0u8; 32], &Call::new(program_hash, "apply").with_args(&2u64).unwrap())
        .unwrap();
    let second: Counter = client.append(&second_intent, Vec::new()).await.unwrap();
    assert_eq!(second.value, 7);

    let state: Vec<Option<Counter>> = client
        .read(|session| async move {
            session
                .get(&[ReadKey::state(b"counter".to_vec())])
                .await
        })
        .await
        .unwrap();
    assert_eq!(state[0].as_ref().unwrap().value, 7);

    let physical = Stream::intent(first_intent.body.author).id();
    let log: Vec<Option<AttestedIntent>> = client
        .read(|session| async move { session.get(&[ReadKey::log_data(&physical, 1)]).await })
        .await
        .unwrap();
    let stored = log[0].as_ref().unwrap();
    assert_eq!(stored.intent, first_intent, "intent is stored byte identical");
    assert!(stored.intent.verify());
    assert!(stored.verify_attestation());

    let counter: u64 = harness
        .server
        .fdb
        .trx()
        .unwrap()
        .get_bcs(&ech_db_protocol::keys::LogKey::seq(&client.root(), &physical))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(counter, 2);
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn grpc_rejects_forged_signature() {
    let mut harness = Harness::start(env()).await;
    let address = harness.start_grpc().await;
    let endpoint = Endpoint::from_shared(format!("http://localhost:{}", address.port())).unwrap();
    let channel = endpoint.connect().await.unwrap();

    let signing = SigningKey::from_bytes(&[5u8; 32]);
    let signature = signing.sign(b"not the pubkey");
    let auth = Auth {
        pubkey: signing.verifying_key().to_bytes(),
        signature: signature.to_bytes(),
    };
    match Exchange::connect(&channel, auth).await {
        Err(ech_db_client::Error::Db(error)) => {
            assert_eq!(error.code, DbErrorCode::Unauthorized);
        }
        Err(error) => panic!("expected unauthorized, got {error:?}"),
        Ok(_) => panic!("expected unauthorized, got opened session"),
    }

    let client = harness.client(rand::random());
    assert!(client.upload_program(harness.counter_bundle()).await.is_ok());
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn append_verifies_intent_signature_and_root() {
    let harness = Harness::start(env()).await;
    let signing = SigningKey::from_bytes(&rand::random());
    let root = signing.verifying_key().to_bytes();
    let program_hash = harness
        .server
        .upload_program(root, harness.counter_bundle())
        .await
        .unwrap();
    let call = Invocation {
        program_hash,
        function: "apply".to_string(),
        args: bcs::to_bytes(&1u64).unwrap(),
    };
    let body = IntentBody {
        version: IntentBody::FORMAT_VERSION,
        root,
        author: root,
        tweak: [0u8; 32],
        expected_seq: 1,
        call: call.clone(),
    };
    let stream = Stream::intent(body.author).id();

    let mut forged = SignedIntent::sign(body.clone(), &signing).unwrap();
    forged.signature[0] ^= 0xff;
    let error = harness.append(root, forged, &signing).await.unwrap_err();
    assert_eq!(error.code, DbErrorCode::Unauthorized);

    let foreign_body = IntentBody {
        root: [9u8; 32],
        ..body
    };
    let foreign = SignedIntent::sign(foreign_body, &signing).unwrap();
    let error = harness.append(root, foreign, &signing).await.unwrap_err();
    assert_eq!(error.code, DbErrorCode::Unauthorized);

    let counter: Option<u64> = harness
        .server
        .fdb
        .trx()
        .unwrap()
        .get_bcs(&ech_db_protocol::keys::LogKey::seq(&root, &stream))
        .await
        .unwrap();
    assert!(counter.is_none(), "rejected intents must leave no state");
}
