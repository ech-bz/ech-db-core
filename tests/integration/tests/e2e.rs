use ed25519_dalek::SigningKey;
use ech_db_protocol::errors::{DbError, DbErrorCode, RetryClass};
use ech_db_protocol::ids::{Stream, StreamId};
use ech_db_protocol::keys::{LogKey, Prefix, StateKey, SysKey, WindowKey};
use ech_db_protocol::values::{AttestedIntent, IntentBody, Invocation, SignedIntent, WindowBlob};
use ech_db_test_fixtures::harness::{Harness, TestEnv};
use ech_db_test_fixtures::programs::Programs;
use foundationdb_tuple::{pack, Bytes};

fn env() -> TestEnv {
    TestEnv::from_env()
}

fn signing() -> SigningKey {
    SigningKey::from_bytes(&rand::random())
}

fn root_of(key: &SigningKey) -> [u8; 32] {
    key.verifying_key().to_bytes()
}

fn intent_for(
    root: [u8; 32],
    key: &SigningKey,
    seq: u64,
    program_hash: [u8; 32],
    args: Vec<u8>,
) -> SignedIntent {
    let body = IntentBody {
        version: IntentBody::FORMAT_VERSION,
        root,
        author: root_of(key),
        tweak: [0u8; 32],
        expected_seq: seq,
        call: Invocation {
            program_hash,
            function: "apply".to_string(),
            args,
        },
    };
    SignedIntent::sign(body, key).unwrap()
}

fn intent(key: &SigningKey, seq: u64, program_hash: [u8; 32], args: Vec<u8>) -> SignedIntent {
    intent_for(root_of(key), key, seq, program_hash, args)
}

fn stream_of(intent: &SignedIntent) -> StreamId {
    Stream::intent(intent.body.author).id()
}

async fn append(
    harness: &Harness,
    key: &SigningKey,
    seq: u64,
    program_hash: [u8; 32],
    args: Vec<u8>,
) -> Result<Vec<u8>, DbError> {
    harness
        .append(root_of(key), intent(key, seq, program_hash, args), key)
        .await
}

async fn window_record_count(harness: &Harness, root: [u8; 32], number: u64) -> usize {
    let (begin, end) = WindowKey::records_range(&root, number);
    let trx = harness.server.fdb.trx().unwrap();
    trx.range_raw(&begin, &end).await.unwrap().len()
}

async fn snapshot(harness: &Harness, root: [u8; 32]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let trx = harness.server.fdb.trx().unwrap();
    let mut rows = Vec::new();
    for namespace in ["log", "state"] {
        let prefix = pack(&(Bytes::from(root.as_slice()), namespace));
        let end = Prefix::successor(&prefix).unwrap();
        rows.extend(trx.range_raw(&prefix, &end).await.unwrap());
    }
    rows.sort();
    rows
}

async fn destroy_root(harness: &Harness, root: [u8; 32]) {
    let trx = harness.server.fdb.trx().unwrap();
    let prefix = pack(&(Bytes::from(root.as_slice()),));
    let end = Prefix::successor(&prefix).unwrap();
    trx.clear_range(&prefix, &end);
    trx.commit().await.unwrap();
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn smoke_upload_append_read_state() {
    let harness = Harness::start(env()).await;
    let key = signing();
    let root = root_of(&key);
    let payload = bcs::to_bytes(&(b"k".to_vec(), b"v".to_vec())).unwrap();
    let bundle = harness.bundle(&Programs::write_then_ok(&payload));
    let program_hash = harness.server.upload_program(root, bundle).await.unwrap();
    let signed = intent(&key, 1, program_hash, vec![]);
    let appended = harness.append(root, signed.clone(), &key).await.unwrap();
    assert!(appended.is_empty());
    let trx = harness.server.fdb.trx().unwrap();
    let value = trx
        .get_raw(&StateKey::physical(&root, b"k"))
        .await
        .unwrap();
    assert_eq!(value.unwrap(), b"v");
    let stream = stream_of(&signed);
    let counter: u64 = trx
        .get_bcs(&LogKey::seq(&root, &stream))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(counter, 1);
    let record = trx
        .get_raw(&LogKey::data(&root, &stream, 1))
        .await
        .unwrap()
        .unwrap();
    let stored: AttestedIntent = bcs::from_bytes(&record).unwrap();
    assert_eq!(stored.intent, signed);
    assert!(stored.intent.verify());
    assert!(stored.verify_attestation());
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn upload_is_idempotent() {
    let harness = Harness::start(env()).await;
    let root: [u8; 32] = rand::random();
    let bundle = harness.bundle(Programs::OK);
    let first = harness.server.upload_program(root, bundle.clone()).await.unwrap();
    let records_after_first = window_record_count(&harness, root, 0).await;
    let second = harness.server.upload_program(root, bundle).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(records_after_first, window_record_count(&harness, root, 0).await);
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn sequence_mismatch_is_terminal() {
    let harness = Harness::start(env()).await;
    let key = signing();
    let root = root_of(&key);
    let bundle = harness.bundle(Programs::OK);
    let program_hash = harness.server.upload_program(root, bundle).await.unwrap();

    let error = append(&harness, &key, 2, program_hash, vec![]).await.unwrap_err();
    assert_eq!(
        error.code,
        DbErrorCode::SequenceMismatch {
            current: 0,
            expected: 2
        }
    );
    assert_eq!(error.class, RetryClass::Never);

    append(&harness, &key, 1, program_hash, vec![]).await.unwrap();
    let error = append(&harness, &key, 1, program_hash, vec![]).await.unwrap_err();
    assert_eq!(
        error.code,
        DbErrorCode::SequenceMismatch {
            current: 1,
            expected: 1
        }
    );
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn counter_program_updates_state() {
    #[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug)]
    struct Counter {
        value: u64,
    }

    let harness = Harness::start(env()).await;
    let key = signing();
    let root = root_of(&key);
    let bundle = harness.counter_bundle();
    let program_hash = harness.server.upload_program(root, bundle).await.unwrap();

    let first = append(
        &harness,
        &key,
        1,
        program_hash,
        bcs::to_bytes(&5u64).unwrap(),
    )
    .await
    .unwrap();
    let first: Counter = bcs::from_bytes(&first).unwrap();
    assert_eq!(first.value, 5);

    let second = append(
        &harness,
        &key,
        2,
        program_hash,
        bcs::to_bytes(&2u64).unwrap(),
    )
    .await
    .unwrap();
    let second: Counter = bcs::from_bytes(&second).unwrap();
    assert_eq!(second.value, 7);
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn concurrent_append_has_single_winner() {
    let harness = Harness::start(env()).await;
    let key = signing();
    let root = root_of(&key);
    let bundle = harness.bundle(Programs::OK);
    let program_hash = harness.server.upload_program(root, bundle).await.unwrap();

    let first_intent = intent(&key, 1, program_hash, bcs::to_bytes(&5u64).unwrap());
    let second_intent = intent(&key, 1, program_hash, bcs::to_bytes(&6u64).unwrap());
    let stream = stream_of(&first_intent);
    let first = harness.append(root, first_intent, &key);
    let second = harness.append(root, second_intent, &key);
    let (first, second) = tokio::join!(first, second);
    let successes = [&first, &second]
        .iter()
        .filter(|result| result.is_ok())
        .count();
    assert_eq!(successes, 1);
    let loser = if first.is_err() { first } else { second }.unwrap_err();
    let loser = if loser.is_retryable() {
        harness
            .append(root, intent(&key, 1, program_hash, vec![]), &key)
            .await
            .unwrap_err()
    } else {
        loser
    };
    assert!(matches!(
        loser.code,
        DbErrorCode::SequenceMismatch { .. }
    ));

    let trx = harness.server.fdb.trx().unwrap();
    let counter: u64 = trx.get_bcs(&LogKey::seq(&root, &stream)).await.unwrap().unwrap();
    assert_eq!(counter, 1);
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn failures_leave_no_changes() {
    let harness = Harness::start(env()).await;
    let key = signing();
    let root = root_of(&key);
    let payload = bcs::to_bytes(&(b"trap-key".to_vec(), b"trap-value".to_vec())).unwrap();
    let cases: Vec<(&str, String, DbErrorCode)> = vec![
        ("trap", Programs::TRAP.to_string(), DbErrorCode::VmTrap),
        (
            "write_then_trap",
            Programs::write_then_trap(&payload),
            DbErrorCode::VmTrap,
        ),
        (
            "abort",
            Programs::write_then_abort(&payload),
            DbErrorCode::Application {
                bytes: b"k".to_vec(),
            },
        ),
        (
            "fuel",
            Programs::FUEL_BURNER.to_string(),
            DbErrorCode::OutOfFuel,
        ),
        (
            "memory",
            Programs::MEMORY_GROWER.to_string(),
            DbErrorCode::ResourceLimit,
        ),
    ];
    for (name, wat, expected) in cases {
        let bundle = harness.bundle(&wat);
        let program_hash = harness.server.upload_program(root, bundle).await.unwrap();
        let signed = intent(&key, 1, program_hash, vec![]);
        let stream = stream_of(&signed);
        let error = harness.append(root, signed, &key).await.unwrap_err();
        let matches = match (&error.code, &expected) {
            (DbErrorCode::Application { .. }, DbErrorCode::Application { .. }) => true,
            (actual, wanted) => actual == wanted,
        };
        assert!(matches, "{name}: unexpected error {error:?}");
        let trx = harness.server.fdb.trx().unwrap();
        let counter = trx.get_bcs::<u64>(&LogKey::seq(&root, &stream)).await.unwrap();
        assert!(counter.is_none(), "{name}: counter mutated");
        let state = trx
            .get_raw(&StateKey::physical(&root, b"trap-key"))
            .await
            .unwrap();
        assert!(state.is_none(), "{name}: state mutated");
    }
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn root_isolation() {
    let harness = Harness::start(env()).await;
    let key_a = signing();
    let key_b = signing();
    let root_a = root_of(&key_a);
    let root_b = root_of(&key_b);
    let bundle = harness.bundle(Programs::OK);
    let program_hash_a = harness.server.upload_program(root_a, bundle.clone()).await.unwrap();
    let program_hash_b = harness.server.upload_program(root_b, bundle).await.unwrap();
    assert_eq!(program_hash_a, program_hash_b, "program hash is content addressed");

    let a1 = intent(&key_a, 1, program_hash_a, vec![]);
    let b1 = intent(&key_b, 1, program_hash_b, vec![]);
    let stream_a = stream_of(&a1);
    let stream_b = stream_of(&b1);
    harness.append(root_a, a1, &key_a).await.unwrap();
    harness.append(root_b, b1, &key_b).await.unwrap();
    harness.append(root_a, intent(&key_a, 2, program_hash_a, vec![]), &key_a).await.unwrap();

    let foreign = intent(&key_b, 2, program_hash_b, vec![]);
    let error = harness.append(root_a, foreign, &key_a).await.unwrap_err();
    assert_eq!(
        error.code,
        DbErrorCode::Unauthorized,
        "intent root must match the authorized root"
    );

    let trx = harness.server.fdb.trx().unwrap();
    let counter_a: u64 = trx.get_bcs(&LogKey::seq(&root_a, &stream_a)).await.unwrap().unwrap();
    let counter_b: u64 = trx.get_bcs(&LogKey::seq(&root_b, &stream_b)).await.unwrap().unwrap();
    assert_eq!(counter_a, 2);
    assert_eq!(counter_b, 1);
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn stream_is_derived_from_author() {
    let harness = Harness::start(env()).await;
    let key = signing();
    let root = root_of(&key);
    let bundle = harness.bundle(Programs::OK);
    let program_hash = harness.server.upload_program(root, bundle).await.unwrap();

    let first = intent(&key, 1, program_hash, vec![]);
    let stream = Stream::intent(first.body.author).id();
    assert_ne!(
        stream.as_array(),
        &root,
        "stream is derived from the author, not the author itself"
    );
    harness.append(root, first, &key).await.unwrap();

    let trx = harness.server.fdb.trx().unwrap();
    let counter: Option<u64> = trx.get_bcs(&LogKey::seq(&root, &stream)).await.unwrap();
    assert_eq!(counter, Some(1));

    let second = intent(&key, 2, program_hash, vec![]);
    harness.append(root, second.clone(), &key).await.unwrap();
    let trx = harness.server.fdb.trx().unwrap();
    let record = trx.get_raw(&LogKey::data(&root, &stream, 2)).await.unwrap().unwrap();
    let stored: AttestedIntent = bcs::from_bytes(&record).unwrap();
    assert_eq!(
        stored.intent, second,
        "stored intent must be byte identical to the signed one"
    );
    assert!(stored.verify_attestation());
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn forbidden_programs_are_rejected() {
    let harness = Harness::start(env()).await;
    let root: [u8; 32] = rand::random();

    let saturating_wat = r#"
    (module
      (memory (export "memory") 1)
      (func (export "ech_alloc") (param i32) (result i32) (local.get 0))
      (func (export "ech_call") (param i32 i32) (result i64)
        (drop (i32.trunc_sat_f32_s (f32.const 1)))
        (i64.const 0))
    )"#;
    let error = harness
        .server
        .upload_program(root, harness.bundle(saturating_wat))
        .await
        .unwrap_err();
    assert_eq!(error.code, DbErrorCode::InvalidWasm);

    let start_wat = r#"(module (func $s) (start $s))"#;
    let error = harness
        .server
        .upload_program(root, harness.bundle(start_wat))
        .await
        .unwrap_err();
    assert_eq!(error.code, DbErrorCode::InvalidWasm);

    let foreign_wat = r#"
    (module
      (import "env" "x" (func (param i32)))
      (memory (export "memory") 1)
      (func (export "ech_alloc") (param i32) (result i32) (i32.const 0))
      (func (export "ech_call") (param i32 i32) (result i64) (i64.const 0))
    )
    "#;
    let error = harness
        .server
        .upload_program(root, harness.bundle(foreign_wat))
        .await
        .unwrap_err();
    assert_eq!(error.code, DbErrorCode::Abi);

    let mut bundle = harness.bundle(Programs::OK);
    bundle.execution_profile = 2;
    let error = harness.server.upload_program(root, bundle).await.unwrap_err();
    assert_eq!(error.code, DbErrorCode::UnsupportedProfile { profile: 2 });

    let mut bundle = harness.bundle(Programs::OK);
    bundle.format_version = 7;
    let error = harness.server.upload_program(root, bundle).await.unwrap_err();
    assert_eq!(error.code, DbErrorCode::InvalidWasm);
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn windows_export_and_anchoring() {
    let harness = Harness::start(env()).await;
    let key = signing();
    let root = root_of(&key);
    let bundle = harness.bundle(Programs::OK);
    let program_hash = harness.server.upload_program(root, bundle).await.unwrap();
    let stream = stream_of(&intent(&key, 1, program_hash, vec![]));

    for window in 0..3u64 {
        append(&harness, &key, window + 1, program_hash, vec![])
            .await
            .unwrap();
        harness.force_close_window(root).await;
    }
    harness.force_close_window(root).await;
    harness.force_close_window(root).await;

    let mut exported = 0usize;
    loop {
        match harness.server.export_once(root).await.unwrap() {
            ech_db_server::exporter::ExportOutcome::Exported => exported += 1,
            ech_db_server::exporter::ExportOutcome::Idle => break,
        }
    }
    assert_eq!(exported, 5, "three filled and two empty windows");

    let mut chain: Vec<[u8; 32]> = Vec::new();
    let mut expected_prev = [0u8; 32];
    for window in 0..5u64 {
        let key = harness.server.s3.key(&root, window);
        let bytes = harness.s3_get(&key).await.unwrap();
        let blob: WindowBlob = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(blob.window_number, window);
        assert_eq!(blob.prev_hash, expected_prev);
        let hash = blob.hash().unwrap();
        expected_prev = hash;
        chain.push(hash);
        if window == 0 {
            assert_eq!(blob.records.len(), 2, "program chunk and first event");
        } else if window < 3 {
            assert_eq!(blob.records.len(), 1);
        } else {
            assert!(blob.records.is_empty());
        }
    }

    harness.sui.wait_anchor(&root, chain[3], chain[4]).await;

    let trx = harness.server.fdb.trx().unwrap();
    for window in 0..5u64 {
        let (begin, end) = WindowKey::records_range(&root, window);
        assert!(trx.range_raw(&begin, &end).await.unwrap().is_empty());
        assert!(trx
            .get_raw(&SysKey::window_number(&root, window))
            .await
            .unwrap()
            .is_none());
    }
    let cursor: Option<ech_db_protocol::values::ExportCursor> =
        trx.get_bcs(&SysKey::export_cursor(&root)).await.unwrap();
    let cursor = cursor.unwrap();
    assert_eq!(cursor.number, 4);
    assert_eq!(cursor.hash, chain[4]);
    let counter: u64 = trx
        .get_bcs(&LogKey::seq(&root, &stream))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(counter, 3, "log data is preserved by the exporter");
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn export_is_idempotent_and_conflict_aware() {
    let harness = Harness::start(env()).await;
    let key = signing();
    let root = root_of(&key);
    let bundle = harness.bundle(Programs::OK);
    let program_hash = harness.server.upload_program(root, bundle).await.unwrap();
    append(&harness, &key, 1, program_hash, vec![]).await.unwrap();
    harness.force_close_window(root).await;

    let outcome = harness.server.export_once(root).await.unwrap();
    assert!(matches!(
        outcome,
        ech_db_server::exporter::ExportOutcome::Exported
    ));
    let blob_key = harness.server.s3.key(&root, 0);
    let blob_bytes = harness.s3_get(&blob_key).await.unwrap();
    let blob: WindowBlob = bcs::from_bytes(&blob_bytes).unwrap();
    let blob_hash = blob.hash().unwrap();
    harness.sui.wait_anchor(&root, [0u8; 32], blob_hash).await;

    let outcome = harness.server.export_once(root).await.unwrap();
    assert!(matches!(
        outcome,
        ech_db_server::exporter::ExportOutcome::Idle
    ));
}

#[tokio::test]
#[ignore = "requires foundationdb and s3 services"]
async fn destructive_recovery_restores_state() {
    use ech_db_recovery::recovery::Recovery;
    use ech_db_server::exporter::ExportOutcome;
    use ech_db_server::exec::Executor;
    use ech_db_server::wasm::Profile;

    #[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug)]
    struct Counter {
        value: u64,
    }

    let harness = Harness::start(env()).await;
    let key_a = signing();
    let key_b = signing();
    let root = root_of(&key_a);
    let counter_program_hash = harness
        .server
        .upload_program(root, harness.counter_bundle())
        .await
        .unwrap();
    let ok_program_hash = harness
        .server
        .upload_program(root, harness.bundle(Programs::OK))
        .await
        .unwrap();

    let mut sequence_a = 0u64;
    let mut sequence_b = 0u64;
    let mut expected = 0u64;
    for _window in 0..10u64 {
        sequence_a += 1;
        sequence_b += 1;
        expected += 1;
        let counter = harness.append(
            root,
            intent_for(
                root,
                &key_a,
                sequence_a,
                counter_program_hash,
                bcs::to_bytes(&1u64).unwrap(),
            ),
            &key_a,
        );
        let other = harness.append(
            root,
            intent_for(root, &key_b, sequence_b, ok_program_hash, vec![]),
            &key_a,
        );
        let (counter, other) = tokio::join!(counter, other);
        let counter: Counter = bcs::from_bytes(&counter.unwrap()).unwrap();
        assert_eq!(counter.value, expected);
        other.unwrap();
        harness.force_close_window(root).await;
    }
    while let ExportOutcome::Exported = harness.server.export_once(root).await.unwrap() {}

    let cursor: ech_db_protocol::values::ExportCursor = harness
        .server
        .fdb
        .trx()
        .unwrap()
        .get_bcs(&SysKey::export_cursor(&root))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.number, 9, "snapshot ends at the last archived boundary");

    let boundary = harness
        .s3_get(&harness.server.s3.key(&root, 8))
        .await
        .unwrap();
    let boundary: WindowBlob = bcs::from_bytes(&boundary).unwrap();
    harness
        .sui
        .wait_anchor(&root, boundary.hash().unwrap(), cursor.hash)
        .await;

    let reference = snapshot(&harness, root).await;
    assert!(!reference.is_empty());

    sequence_b += 1;
    harness
        .append(
            root,
            intent_for(root, &key_b, sequence_b, ok_program_hash, vec![]),
            &key_a,
        )
        .await
        .unwrap();

    destroy_root(&harness, root).await;
    assert!(snapshot(&harness, root).await.is_empty());

    let recovery = Recovery::new(&harness.config, Executor::new(Profile::engine())).unwrap();
    recovery.run(root).await.unwrap();

    let restored = snapshot(&harness, root).await;
    assert_eq!(reference, restored, "restored state must be byte identical");

    let counter_result = harness
        .append(
            root,
            intent_for(
                root,
                &key_a,
                sequence_a + 1,
                counter_program_hash,
                bcs::to_bytes(&3u64).unwrap(),
            ),
            &key_a,
        )
        .await
        .unwrap();
    let counter_result: Counter = bcs::from_bytes(&counter_result).unwrap();
    assert_eq!(counter_result.value, expected + 3);

    harness.force_close_window(root).await;
    let outcome = harness.server.export_once(root).await.unwrap();
    assert!(matches!(outcome, ExportOutcome::Exported));
    let blob = harness
        .s3_get(&harness.server.s3.key(&root, 10))
        .await
        .unwrap();
    let blob: WindowBlob = bcs::from_bytes(&blob).unwrap();
    harness
        .sui
        .wait_anchor(&root, blob.prev_hash, blob.hash().unwrap())
        .await;
}

