use ed25519_dalek::SigningKey;
use ech_db_protocol::errors::{DbErrorCode, RetryClass};
use ech_db_protocol::values::{ArchiveRecord, ExportCursor, IntentBody, Invocation, SignedIntent, WindowBlob};
use ech_db_protocol::keys::WindowKey;
use ech_db_server::exporter::ExportOutcome;
use ech_db_server::s3::PutOutcome;
use ech_db_test_fixtures::harness::{Harness, TestEnv};
use ech_db_test_fixtures::programs::Programs;

fn env() -> TestEnv {
    TestEnv::from_env()
}

async fn prepared(harness: &Harness) -> [u8; 32] {
    let signing = SigningKey::from_bytes(&rand::random());
    let root = signing.verifying_key().to_bytes();
    let program_hash = harness
        .server
        .upload_program(root, harness.bundle(Programs::OK))
        .await
        .unwrap();
    let body = IntentBody {
        version: IntentBody::FORMAT_VERSION,
        root,
        author: root,
        tweak: [0u8; 32],
        expected_seq: 1,
        call: Invocation {
            program_hash,
            function: "apply".to_string(),
            args: Vec::new(),
        },
    };
    let intent = SignedIntent::sign(body, &signing).unwrap();
    harness.append(root, intent, &signing).await.unwrap();
    harness.force_close_window(root).await;
    root
}

async fn blob_for(
    harness: &Harness,
    root: [u8; 32],
    number: u64,
    prev_hash: [u8; 32],
) -> (Vec<u8>, [u8; 32]) {
    let trx = harness.server.fdb.trx().unwrap();
    let (begin, end) = WindowKey::records_range(&root, number);
    let rows = trx.range_raw(&begin, &end).await.unwrap();
    drop(trx);
    let records = rows
        .iter()
        .map(|(_, value)| bcs::from_bytes::<ArchiveRecord>(value).unwrap())
        .collect();
    let blob = WindowBlob {
        format_version: WindowBlob::FORMAT_VERSION,
        root,
        window_number: number,
        prev_hash,
        records,
    };
    let bytes = bcs::to_bytes(&blob).unwrap();
    let hash = blob.hash().unwrap();
    (bytes, hash)
}

async fn cursor(harness: &Harness, root: [u8; 32]) -> Option<ExportCursor> {
    harness
        .server
        .fdb
        .trx()
        .unwrap()
        .get_bcs(&ech_db_protocol::keys::SysKey::export_cursor(&root))
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn export_completes_when_blob_already_uploaded() {
    let harness = Harness::start(env()).await;
    let root = prepared(&harness).await;
    let (bytes, hash) = blob_for(&harness, root, 0, [0u8; 32]).await;
    let key = harness.server.s3.key(&root, 0);
    let put = harness.server.s3.put_immutable(&key, &bytes).await.unwrap();
    assert!(matches!(put, PutOutcome::Stored));

    let outcome = harness.server.export_once(root).await.unwrap();
    assert!(matches!(outcome, ExportOutcome::Exported));
    let cursor = cursor(&harness, root).await.unwrap();
    assert_eq!(cursor.number, 0);
    assert_eq!(cursor.hash, hash);

    let trx = harness.server.fdb.trx().unwrap();
    let (begin, end) = WindowKey::records_range(&root, 0);
    assert!(trx.range_raw(&begin, &end).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn export_completes_when_anchor_already_written() {
    let harness = Harness::start(env()).await;
    let root = prepared(&harness).await;
    let (bytes, hash) = blob_for(&harness, root, 0, [0u8; 32]).await;
    let key = harness.server.s3.key(&root, 0);
    harness.server.s3.put_immutable(&key, &bytes).await.unwrap();
    harness.sui.set_anchor(root, [0u8; 32], hash).await;

    let outcome = harness.server.export_once(root).await.unwrap();
    assert!(matches!(outcome, ExportOutcome::Exported));
    let cursor = cursor(&harness, root).await.unwrap();
    assert_eq!(cursor.hash, hash);
    harness.sui.wait_anchor(&root, [0u8; 32], hash).await;
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn export_rejects_conflicting_anchor() {
    let harness = Harness::start(env()).await;
    let root = prepared(&harness).await;
    harness.sui.set_anchor(root, [0u8; 32], [9u8; 32]).await;

    let error = harness.server.export_once(root).await.unwrap_err();
    assert_eq!(error.class, RetryClass::Never);
    assert_eq!(error.code, DbErrorCode::Integrity);
    assert!(cursor(&harness, root).await.is_none());
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn export_rejects_conflicting_blob() {
    let harness = Harness::start(env()).await;
    let root = prepared(&harness).await;
    let key = harness.server.s3.key(&root, 0);
    harness
        .server
        .s3
        .put_immutable(&key, b"foreign window bytes")
        .await
        .unwrap();

    let error = harness.server.export_once(root).await.unwrap_err();
    assert_eq!(error.class, RetryClass::Never);
    assert_eq!(error.code, DbErrorCode::Integrity);
    assert!(cursor(&harness, root).await.is_none());
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn concurrent_exports_have_single_winner() {
    let harness = Harness::start(env()).await;
    let root = prepared(&harness).await;

    let first = harness.server.export_once(root);
    let second = harness.server.export_once(root);
    let (first, second) = tokio::join!(first, second);
    let winner = matches!(first, Ok(ExportOutcome::Exported)) as usize
        + matches!(second, Ok(ExportOutcome::Exported)) as usize;
    assert_eq!(winner, 1, "{first:?} {second:?}");
    let loser = if matches!(first, Ok(ExportOutcome::Exported)) {
        second
    } else {
        first
    };
    match loser {
        Ok(ExportOutcome::Idle) => {}
        Err(error) => assert!(error.is_retryable(), "{error:?}"),
        Ok(ExportOutcome::Exported) => panic!("two exports won"),
    }

    let cursor = cursor(&harness, root).await.unwrap();
    assert_eq!(cursor.number, 0);
    let stored = harness
        .server
        .s3
        .get(&harness.server.s3.key(&root, 0))
        .await
        .unwrap();
    let stored: WindowBlob = bcs::from_bytes(&stored).unwrap();
    assert_eq!(cursor.hash, stored.hash().unwrap());
}
