use std::{
    fs,
    fs::OpenOptions,
    path::{Path, PathBuf},
    process,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Barrier,
    },
    thread,
    time::Duration,
};

use fs2::FileExt;
use synapse_core::knowledge::{
    Confidence, KnowledgeRecord, KnowledgeRelation, KnowledgeRelationKind, KnowledgeState,
    Provenance, ProvenanceBasis,
};
use synapse_store::{
    AuthorizationPolicy, ClientAuthorization, FileStore, KnowledgeQuery, StoreError,
};

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let directory = Self::unconfigured();
        write_test_authorization(directory.path());
        directory
    }

    fn unconfigured() -> Self {
        let sequence = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("synapse-store-test-{}-{sequence}", process::id()));
        fs::create_dir_all(&path).expect("test directory should be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

fn write_test_authorization(root: &Path) {
    let clients = [
        "store-test",
        "evolution-test",
        "machine-inspector",
        "planner",
        "test",
        "observer",
        "inspector",
        "observer-a",
        "observer-b",
        "a",
        "b",
        "reviewer",
    ]
    .into_iter()
    .map(|id| {
        serde_json::json!({
            "id": id,
            "write_records": true,
            "write_relations": true
        })
    })
    .collect::<Vec<_>>();
    let policy = serde_json::json!({ "version": 1, "clients": clients });
    fs::write(
        root.join("authorization-v1.json"),
        serde_json::to_vec(&policy).expect("authorization fixture should serialize"),
    )
    .expect("authorization fixture should be written");
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn encoded_path(root: &Path, directory: &str, id: &str) -> PathBuf {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(id.len() * 2 + 5);
    for byte in id.as_bytes() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded.push_str(".json");
    root.join(directory).join(encoded)
}

fn record_path(root: &Path, id: &str) -> PathBuf {
    encoded_path(root, "records", id)
}

fn relation_path(root: &Path, id: &str) -> PathBuf {
    encoded_path(root, "relations", id)
}

fn index_entry_count(root: &Path) -> usize {
    fs::read_dir(root.join("index-v1"))
        .expect("index directory should exist")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("idx"))
        .count()
}

fn record(id: &str, content: &str) -> KnowledgeRecord {
    record_with(
        id,
        content,
        "fact",
        "store-test",
        1_788_707_200_000,
        KnowledgeState::Active,
    )
}

fn addressed_record(id: &str, content: &str, scope: &str, key: &str) -> KnowledgeRecord {
    record(id, content)
        .with_address(scope, key)
        .expect("addressed fixture should be valid")
}

fn relation(
    id: &str,
    subject_id: &str,
    kind: KnowledgeRelationKind,
    object_id: &str,
) -> KnowledgeRelation {
    relation_with_source(id, subject_id, kind, object_id, "evolution-test")
}

fn relation_with_source(
    id: &str,
    subject_id: &str,
    kind: KnowledgeRelationKind,
    object_id: &str,
    source: &str,
) -> KnowledgeRelation {
    KnowledgeRelation::new(
        id,
        subject_id,
        kind,
        object_id,
        source,
        1_788_707_300_000,
        Provenance {
            basis: ProvenanceBasis::Observed,
            detail: None,
        },
    )
    .expect("relation fixture should be valid")
}

fn record_with(
    id: &str,
    content: &str,
    kind: &str,
    source: &str,
    created_at_unix_ms: u64,
    state: KnowledgeState,
) -> KnowledgeRecord {
    KnowledgeRecord::new(
        id,
        content,
        kind,
        source,
        created_at_unix_ms,
        Confidence::High,
        state,
        Provenance {
            basis: ProvenanceBasis::Observed,
            detail: None,
        },
    )
    .expect("fixture should be valid")
}

#[test]
fn store_inspection_does_not_create_a_missing_store() {
    let directory = TestDir::unconfigured();
    let missing = directory.path().join("not-created-yet");
    let inspection = FileStore::new(&missing)
        .inspect()
        .expect("missing store inspection should succeed");

    assert!(!inspection.root_exists);
    assert_eq!(inspection.store_id, None);
    assert_eq!(inspection.authorization_clients, None);
    assert_eq!(inspection.record_count, 0);
    assert_eq!(inspection.relation_count, 0);
    assert!(!inspection.index_ready);
    assert_eq!(inspection.index_entry_count, None);
    assert!(!missing.exists());
}

#[test]
fn store_inspection_validates_records_relations_index_and_configuration() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("old", "old compiler path"))
        .expect("old record should persist");
    store
        .insert(&record("new", "new compiler path"))
        .expect("new record should persist");
    store
        .insert_relation(&relation(
            "new-over-old",
            "new",
            KnowledgeRelationKind::Supersedes,
            "old",
        ))
        .expect("relation should persist");
    let store_id = store.store_id().expect("identity should initialize");

    let inspection = store.inspect().expect("store should inspect cleanly");
    assert!(inspection.root_exists);
    assert_eq!(inspection.store_id.as_deref(), Some(store_id.as_str()));
    assert_eq!(inspection.authorization_clients, Some(12));
    assert_eq!(inspection.record_count, 2);
    assert_eq!(inspection.relation_count, 1);
    assert!(inspection.index_ready);
    assert_eq!(inspection.index_entry_count, Some(2));
}

#[test]
fn store_inspection_fails_closed_on_a_corrupt_ready_index() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("indexed", "indexed value"))
        .expect("record should persist");
    let index_entry = fs::read_dir(directory.path().join("index-v1"))
        .expect("index directory should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("idx"))
        .expect("index entry should exist");
    fs::write(index_entry, b"corrupt-index-entry").expect("index fixture should be corrupted");

    let error = store
        .inspect()
        .expect_err("corrupt ready index must fail store inspection");
    assert!(matches!(error, StoreError::CorruptIndex { .. }));
}

#[test]
fn store_identity_is_created_once_and_persisted() {
    let directory = TestDir::unconfigured();
    let store = FileStore::new(directory.path());

    let first = store.store_id().expect("store identity should initialize");
    let second = store.store_id().expect("store identity should reload");
    assert_eq!(first, second);
    assert_eq!(first.len(), 64);
    assert!(first
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));

    let persisted: serde_json::Value = serde_json::from_slice(
        &fs::read(directory.path().join("store-identity-v1.json"))
            .expect("store identity file should exist"),
    )
    .expect("store identity should be JSON");
    assert_eq!(persisted["version"], 1);
    assert_eq!(persisted["id"], first);
}

#[test]
fn concurrent_store_identity_initializers_converge_on_one_id() {
    let directory = TestDir::unconfigured();
    let writers = 16usize;
    let barrier = Arc::new(Barrier::new(writers));
    let mut handles = Vec::with_capacity(writers);

    for writer in 0..writers {
        let root = if writer % 2 == 0 {
            directory.path().to_path_buf()
        } else {
            directory.path().join(".")
        };
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            FileStore::new(root)
                .store_id()
                .expect("concurrent store identity initialization should succeed")
        }));
    }

    let mut identities = handles
        .into_iter()
        .map(|handle| handle.join().expect("identity thread should not panic"))
        .collect::<Vec<_>>();
    identities.sort();
    identities.dedup();
    assert_eq!(identities.len(), 1);
}

#[test]
fn malformed_store_identity_fails_closed() {
    let directory = TestDir::unconfigured();
    fs::write(
        directory.path().join("store-identity-v1.json"),
        br#"{"version":1,"id":"not-a-valid-store-id"}"#,
    )
    .expect("invalid identity fixture should be written");

    let error = FileStore::new(directory.path())
        .store_id()
        .expect_err("invalid store identity must fail closed");
    assert!(matches!(
        error,
        StoreError::InvalidStoreIdentity { ref reason }
            if reason.contains("64 lowercase hexadecimal")
    ));
}

#[test]
fn future_store_identity_versions_fail_closed() {
    let directory = TestDir::unconfigured();
    fs::write(
        directory.path().join("store-identity-v1.json"),
        br#"{"version":2,"id":"0000000000000000000000000000000000000000000000000000000000000000"}"#,
    )
    .expect("future identity fixture should be written");

    let error = FileStore::new(directory.path())
        .store_id()
        .expect_err("unsupported identity version must fail closed");
    assert!(matches!(
        error,
        StoreError::InvalidStoreIdentity { ref reason }
            if reason.contains("unsupported store identity version 2")
    ));
}

#[test]
fn successor_commit_publishes_record_and_supersession_as_one_authoritative_file() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    let old = record_with(
        "compiler-old",
        "compiler path old",
        "fact",
        "store-test",
        1_788_707_200_000,
        KnowledgeState::Active,
    );
    store.insert(&old).expect("old record should persist");

    let new = record_with(
        "compiler-new",
        "compiler path new",
        "fact",
        "store-test",
        1_788_707_300_000,
        KnowledgeState::Active,
    );
    let supersedes = relation_with_source(
        "compiler-replaced",
        "compiler-new",
        KnowledgeRelationKind::Supersedes,
        "compiler-old",
        "store-test",
    );

    store
        .insert_successor(&new, &supersedes)
        .expect("successor commit should persist atomically");

    assert!(
        !record_path(directory.path(), "compiler-new").exists(),
        "atomic successors must not publish a standalone record file"
    );
    assert!(
        !relation_path(directory.path(), "compiler-replaced").exists(),
        "atomic successors must not publish a standalone relation file"
    );
    let commit_path = encoded_path(directory.path(), "successors-v1", "compiler-new");
    let persisted: serde_json::Value = serde_json::from_slice(
        &fs::read(&commit_path).expect("combined successor commit should exist"),
    )
    .expect("combined successor commit should be JSON");
    assert_eq!(persisted["schema_version"], 1);
    assert_eq!(persisted["record"]["id"], "compiler-new");
    assert_eq!(persisted["relation"]["id"], "compiler-replaced");
    assert_eq!(persisted["relation"]["kind"], "supersedes");

    assert_eq!(
        store
            .get("compiler-new")
            .expect("successor should be readable")
            .expect("successor should exist"),
        new
    );
    let old_status = store
        .status("compiler-old")
        .expect("status should resolve")
        .expect("old record should exist");
    assert_eq!(old_status.effective_state, KnowledgeState::Superseded);
    assert_eq!(old_status.superseded_by.len(), 1);
    assert_eq!(old_status.superseded_by[0].id, "compiler-replaced");

    let current = store
        .query(&KnowledgeQuery {
            text: Some("compiler path".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("current query should resolve the atomic successor");
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].id, "compiler-new");
    assert_eq!(current[0].effective_state, KnowledgeState::Active);
}

#[test]
fn addressed_successor_uses_schema_two_and_keeps_the_same_logical_address_current() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    let old = addressed_record(
        "address-old",
        "compiler path old",
        "machine",
        "toolchain.rust.compiler_path",
    );
    store
        .insert(&old)
        .expect("old addressed record should persist");

    let new = addressed_record(
        "address-new",
        "compiler path new",
        "machine",
        "toolchain.rust.compiler_path",
    );
    let relation = relation_with_source(
        "address-replaced",
        "address-new",
        KnowledgeRelationKind::Supersedes,
        "address-old",
        "store-test",
    );
    store
        .insert_successor(&new, &relation)
        .expect("addressed successor should publish");

    let commit: serde_json::Value = serde_json::from_slice(
        &fs::read(encoded_path(
            directory.path(),
            "successors-v1",
            "address-new",
        ))
        .expect("addressed successor commit should be readable"),
    )
    .expect("addressed successor commit should be JSON");
    assert_eq!(commit["schema_version"], 2);

    let current = store
        .query(&KnowledgeQuery {
            scope: Some("machine".to_owned()),
            key: Some("toolchain.rust.compiler_path".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("address query should resolve lifecycle state");
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].id, "address-new");
    assert_eq!(current[0].effective_state, KnowledgeState::Active);
}

#[test]
fn invalid_successor_shape_fails_before_authoritative_or_index_side_effects() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("old", "old value"))
        .expect("old record should persist");
    let entries_before = index_entry_count(directory.path());
    let new = record("new", "new value");
    let invalid = relation_with_source(
        "invalid-successor",
        "old",
        KnowledgeRelationKind::Supersedes,
        "new",
        "store-test",
    );

    let error = store
        .insert_successor(&new, &invalid)
        .expect_err("relation subject must be the new record");
    assert!(matches!(
        error,
        StoreError::InvalidSuccessor { ref reason }
            if reason.contains("relation subject")
    ));
    assert!(!encoded_path(directory.path(), "successors-v1", "new").exists());
    assert_eq!(index_entry_count(directory.path()), entries_before);
    assert!(store
        .get("new")
        .expect("new id should be readable")
        .is_none());
}

#[test]
fn successor_commit_participates_in_index_rebuild_and_legacy_discovery() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("old", "shared migration value old"))
        .expect("old record should persist");
    let new = record_with(
        "new",
        "shared migration value new",
        "fact",
        "store-test",
        1_788_707_300_000,
        KnowledgeState::Active,
    );
    let relation = relation_with_source(
        "new-supersedes-old",
        "new",
        KnowledgeRelationKind::Supersedes,
        "old",
        "store-test",
    );
    store
        .insert_successor(&new, &relation)
        .expect("successor should persist");

    fs::remove_dir_all(directory.path().join("index-v1"))
        .expect("derived index should be removable");
    let legacy_hits = FileStore::new(directory.path())
        .query(&KnowledgeQuery {
            text: Some("shared migration value".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("legacy discovery should enumerate combined successors");
    assert_eq!(legacy_hits.len(), 1);
    assert_eq!(legacy_hits[0].id, "new");

    assert_eq!(
        FileStore::new(directory.path())
            .rebuild_index()
            .expect("rebuild should include standalone and successor records"),
        2
    );
    let indexed_hits = FileStore::new(directory.path())
        .query(&KnowledgeQuery {
            text: Some("shared migration value".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("indexed discovery should include the successor record");
    assert_eq!(indexed_hits.len(), 1);
    assert_eq!(indexed_hits[0].id, "new");
}

#[test]
fn successor_ids_share_record_and_relation_uniqueness_with_standalone_storage() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    for id in ["old", "other"] {
        store
            .insert(&record(id, &format!("record {id}")))
            .expect("fixture record should persist");
    }
    let new = record("new", "new value");
    let relation = relation_with_source(
        "successor-relation",
        "new",
        KnowledgeRelationKind::Supersedes,
        "old",
        "store-test",
    );
    store
        .insert_successor(&new, &relation)
        .expect("successor should persist");

    let duplicate_record = store
        .insert(&record("new", "standalone duplicate"))
        .expect_err("standalone records must share successor record IDs");
    assert!(matches!(duplicate_record, StoreError::AlreadyExists { .. }));

    let duplicate_relation = relation_with_source(
        "successor-relation",
        "other",
        KnowledgeRelationKind::ConflictsWith,
        "old",
        "store-test",
    );
    let error = store
        .insert_relation(&duplicate_relation)
        .expect_err("standalone relations must share successor relation IDs");
    assert!(matches!(error, StoreError::RelationAlreadyExists { .. }));
}

#[test]
fn future_successor_schema_versions_fail_closed() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("old", "old value"))
        .expect("old record should persist");
    let successors = directory.path().join("successors-v1");
    fs::create_dir_all(&successors).expect("successor directory should exist");
    let future = serde_json::json!({
        "schema_version": 3,
        "record": record("future", "future value"),
        "relation": relation_with_source(
            "future-over-old",
            "future",
            KnowledgeRelationKind::Supersedes,
            "old",
            "store-test",
        )
    });
    fs::write(
        encoded_path(directory.path(), "successors-v1", "future"),
        serde_json::to_vec(&future).expect("future successor fixture should serialize"),
    )
    .expect("future successor fixture should be written");

    let error = store
        .get("future")
        .expect_err("unsupported successor schema versions must fail closed");
    assert!(matches!(
        error,
        StoreError::CorruptSuccessor { ref reason, .. }
            if reason.contains("unsupported successor schema version 3")
    ));
}

#[test]
fn incomplete_successor_temp_files_never_become_authoritative() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("old", "stable old value"))
        .expect("old record should persist");
    let successors = directory.path().join("successors-v1");
    fs::create_dir_all(&successors).expect("successor directory should exist");
    fs::write(
        successors.join(".synapse-tmp-crash-fixture"),
        b"partial-json",
    )
    .expect("crash temp fixture should be written");

    assert!(store
        .get("not-published")
        .expect("missing successor should remain a normal miss")
        .is_none());
    let hits = store
        .query(&KnowledgeQuery {
            text: Some("stable old value".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("temporary successor files must be ignored by discovery");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "old");
    assert_eq!(hits[0].effective_state, KnowledgeState::Active);
}

#[test]
fn successor_components_keep_the_existing_record_size_limit() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("old", "old value"))
        .expect("old record should persist");
    let oversized = record("oversized-successor", &"x".repeat(1024 * 1024));
    let relation = relation_with_source(
        "oversized-over-old",
        "oversized-successor",
        KnowledgeRelationKind::Supersedes,
        "old",
        "store-test",
    );

    let error = store
        .insert_successor(&oversized, &relation)
        .expect_err("successor records must retain the standalone record size bound");
    assert!(matches!(
        error,
        StoreError::RecordTooLarge {
            max_bytes: 1_048_576
        }
    ));
    assert!(!encoded_path(directory.path(), "successors-v1", "oversized-successor").exists());
}

#[test]
fn query_waits_for_the_store_state_lock_before_reading_currentness() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("stable", "stable current value"))
        .expect("fixture record should persist");

    let state_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.path().join(".state.lock"))
        .expect("state lock file should exist after a write");
    FileExt::lock_exclusive(&state_file).expect("test should hold the exclusive state lock");

    let (sender, receiver) = mpsc::channel();
    let root = directory.path().to_path_buf();
    let reader = thread::spawn(move || {
        let result = FileStore::new(root).query(&KnowledgeQuery {
            text: Some("stable current value".to_owned()),
            ..KnowledgeQuery::default()
        });
        sender.send(result).expect("reader result should be sent");
    });

    thread::sleep(Duration::from_millis(50));
    assert!(
        receiver.try_recv().is_err(),
        "currentness reads must not pass an exclusive state mutation lock"
    );
    FileExt::unlock(&state_file).expect("test state lock should release");

    let hits = receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("reader should finish after the state lock releases")
        .expect("query should succeed");
    reader.join().expect("reader thread should not panic");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "stable");
}

#[test]
fn separate_store_instances_can_exchange_a_record() {
    let directory = TestDir::new();
    let writer = FileStore::new(directory.path());
    writer
        .insert(&record("record-1", "shared knowledge"))
        .expect("record should persist");

    let reader = FileStore::new(directory.path());
    let loaded = reader
        .get("record-1")
        .expect("record should be readable")
        .expect("record should exist");

    assert_eq!(loaded.content, "shared knowledge");
    assert_eq!(loaded.source, "store-test");
}

#[test]
fn new_record_files_include_an_explicit_schema_version() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("versioned-record", "versioned payload"))
        .expect("record should persist");

    let stored: serde_json::Value = serde_json::from_slice(
        &fs::read(record_path(directory.path(), "versioned-record"))
            .expect("record file should be readable"),
    )
    .expect("record file should contain JSON");
    assert_eq!(stored["schema_version"], 1);
    assert_eq!(stored["record"]["id"], "versioned-record");
    assert_eq!(stored["record"]["content"], "versioned payload");
}

#[test]
fn addressed_record_files_use_schema_version_two() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&addressed_record(
            "addressed-versioned",
            "addressed payload",
            "machine",
            "toolchain.rust.compiler_path",
        ))
        .expect("addressed record should persist");

    let stored: serde_json::Value = serde_json::from_slice(
        &fs::read(record_path(directory.path(), "addressed-versioned"))
            .expect("addressed record file should be readable"),
    )
    .expect("addressed record file should contain JSON");
    assert_eq!(stored["schema_version"], 2);
    assert_eq!(stored["record"]["scope"], "machine");
    assert_eq!(stored["record"]["key"], "toolchain.rust.compiler_path");
}

#[test]
fn schema_one_records_cannot_smuggle_scope_key_fields() {
    let directory = TestDir::new();
    let records_dir = directory.path().join("records");
    fs::create_dir_all(&records_dir).expect("records directory should exist");
    let addressed = addressed_record(
        "bad-v1-address",
        "addressed payload",
        "machine",
        "toolchain.rust.compiler_path",
    );
    let fixture = serde_json::json!({
        "schema_version": 1,
        "record": addressed
    });
    fs::write(
        record_path(directory.path(), "bad-v1-address"),
        serde_json::to_vec(&fixture).expect("fixture should serialize"),
    )
    .expect("fixture should be written");

    let error = FileStore::new(directory.path())
        .get("bad-v1-address")
        .expect_err("v1 must not accept scope/key fields");
    assert!(matches!(
        error,
        StoreError::CorruptRecord { ref reason, .. }
            if reason.contains("schema version 1 records cannot contain scope/key")
    ));
}

#[test]
fn explicit_future_record_schema_versions_fail_closed() {
    let directory = TestDir::new();
    let records_dir = directory.path().join("records");
    fs::create_dir_all(&records_dir).expect("records directory should exist");
    let future = serde_json::json!({
        "schema_version": 3,
        "record": record("future-record", "future payload")
    });
    fs::write(
        record_path(directory.path(), "future-record"),
        serde_json::to_vec(&future).expect("future fixture should serialize"),
    )
    .expect("future fixture should be written");

    let error = FileStore::new(directory.path())
        .get("future-record")
        .expect_err("unsupported explicit schema versions must fail closed");
    assert!(matches!(
        error,
        StoreError::CorruptRecord { ref reason, .. }
            if reason.contains("unsupported record schema version 3")
    ));
}

#[test]
fn separate_store_instances_can_discover_inserted_knowledge_through_the_index() {
    let directory = TestDir::new();
    let writer = FileStore::new(directory.path());
    writer
        .insert(&record("indexed-1", "durable indexed discovery"))
        .expect("record should persist with its index entry");

    let reader = FileStore::new(directory.path());
    let results = reader
        .query(&KnowledgeQuery {
            text: Some("INDEXED discovery".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("separate reader should use the durable index");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "indexed-1");
}

#[test]
fn inserting_the_same_id_does_not_replace_existing_knowledge() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("record-1", "original"))
        .expect("first insert should succeed");

    let error = store
        .insert(&record("record-1", "replacement"))
        .expect_err("duplicate id should be rejected");

    assert!(matches!(error, StoreError::AlreadyExists { .. }));
    assert_eq!(
        store
            .get("record-1")
            .expect("record should remain readable")
            .expect("original should remain")
            .content,
        "original"
    );
}

#[test]
fn rejected_duplicate_inserts_do_not_accumulate_index_entries() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("record-1", "authoritative"))
        .expect("first insert should succeed");
    assert_eq!(index_entry_count(directory.path()), 1);

    for attempt in 0..20 {
        let error = store
            .insert(&record(
                "record-1",
                &format!("rejected replacement {attempt}"),
            ))
            .expect_err("duplicate id should stay write-once");
        assert!(matches!(error, StoreError::AlreadyExists { .. }));
    }

    assert_eq!(
        index_entry_count(directory.path()),
        1,
        "rejected duplicates must not consume durable index capacity"
    );
    let found = store
        .query(&KnowledgeQuery {
            text: Some("authoritative".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("the winning record should remain discoverable");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "record-1");
}

#[test]
fn concurrent_duplicate_inserts_keep_only_the_winning_index_entry() {
    let directory = TestDir::new();
    FileStore::new(directory.path())
        .rebuild_index()
        .expect("empty store should have a ready index before the race");

    let writers = 16usize;
    let barrier = Arc::new(Barrier::new(writers));
    let mut handles = Vec::with_capacity(writers);
    for writer in 0..writers {
        let root = directory.path().to_path_buf();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let store = FileStore::new(root);
            let candidate = record("raced-record", &format!("candidate {writer}"));
            barrier.wait();
            store.insert(&candidate).map(|()| candidate)
        }));
    }

    let mut winner = None;
    let mut duplicates = 0usize;
    for handle in handles {
        match handle.join().expect("writer thread should not panic") {
            Ok(record) => {
                assert!(
                    winner.replace(record).is_none(),
                    "exactly one writer may win"
                );
            }
            Err(StoreError::AlreadyExists { .. }) => duplicates += 1,
            Err(error) => panic!("concurrent duplicate write failed unexpectedly: {error}"),
        }
    }

    let winner = winner.expect("one writer should publish the authoritative record");
    assert_eq!(duplicates, writers - 1);
    assert_eq!(
        index_entry_count(directory.path()),
        1,
        "losing concurrent writers must reconcile their index entries"
    );
    assert_eq!(
        FileStore::new(directory.path())
            .get("raced-record")
            .expect("winning record should be readable")
            .expect("winning record should exist"),
        winner
    );
}

#[test]
fn path_like_ids_cannot_escape_the_store_directory() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("../../outside", "contained"))
        .expect("path-like id should be safely encoded");

    assert_eq!(
        store
            .get("../../outside")
            .expect("encoded record should be readable")
            .expect("record should exist")
            .content,
        "contained"
    );
    assert!(!directory.path().join("outside").exists());
}

#[test]
fn oversized_ids_are_rejected_before_filesystem_access() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    let id = "x".repeat(101);

    let error = store
        .insert(&record(&id, "too long"))
        .expect_err("oversized id should be rejected");

    assert!(matches!(error, StoreError::IdTooLong { max_bytes: 100 }));
}

#[test]
fn corrupted_files_are_rejected_on_read() {
    let directory = TestDir::new();
    let records_dir = directory.path().join("records");
    fs::create_dir_all(&records_dir).expect("records directory should be created");
    fs::write(record_path(directory.path(), "record-1"), b"not-json")
        .expect("corrupt fixture should be written");

    let error = FileStore::new(directory.path())
        .get("record-1")
        .expect_err("corrupt persisted data must not be trusted");

    assert!(matches!(error, StoreError::CorruptRecord { .. }));
}

#[test]
fn corrupted_index_entries_fail_closed() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("record-1", "indexed knowledge"))
        .expect("record should persist");

    let index_entry = fs::read_dir(directory.path().join("index-v1"))
        .expect("index directory should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("idx"))
        .expect("index entry should exist");
    fs::write(index_entry, b"corrupt-index-entry").expect("fixture should be corrupted");

    let error = FileStore::new(directory.path())
        .query(&KnowledgeQuery {
            text: Some("indexed knowledge".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect_err("corrupt index data must not be trusted");

    assert!(matches!(error, StoreError::CorruptIndex { .. }));
}

#[test]
fn query_discovers_active_knowledge_without_knowing_its_id() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record_with(
            "toolchain-rust",
            "Rust compiler is installed at C:/Rust/bin/rustc.exe",
            "fact",
            "machine-inspector",
            20,
            KnowledgeState::Active,
        ))
        .expect("matching record should persist");
    store
        .insert(&record_with(
            "toolchain-node",
            "Node.js is installed",
            "fact",
            "machine-inspector",
            10,
            KnowledgeState::Active,
        ))
        .expect("non-matching record should persist");

    let results = store
        .query(&KnowledgeQuery {
            text: Some("RUST compiler".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("query should succeed");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "toolchain-rust");
}

#[test]
fn query_addresses_knowledge_by_exact_scope_and_key() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&addressed_record(
            "machine-rust",
            "compiler lives at C:/Rust/bin/rustc.exe",
            "machine",
            "toolchain.rust.compiler_path",
        ))
        .expect("machine-addressed record should persist");
    store
        .insert(&addressed_record(
            "project-rust",
            "project overrides its compiler",
            "project:synapse",
            "toolchain.rust.compiler_path",
        ))
        .expect("project-addressed record should persist");

    let hits = store
        .query(&KnowledgeQuery {
            scope: Some("machine".to_owned()),
            key: Some("toolchain.rust.compiler_path".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("exact address query should succeed");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "machine-rust");

    let wrong_case = store
        .query(&KnowledgeQuery {
            scope: Some("Machine".to_owned()),
            key: Some("toolchain.rust.compiler_path".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("case-sensitive address query should succeed with no match");
    assert!(wrong_case.is_empty());
}

#[test]
fn multiple_active_records_can_share_an_address_without_silent_winner_selection() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    for (id, content) in [("path-a", "compiler path A"), ("path-b", "compiler path B")] {
        store
            .insert(&addressed_record(
                id,
                content,
                "machine",
                "toolchain.rust.compiler_path",
            ))
            .expect("same-address record should persist");
    }

    let hits = store
        .query(&KnowledgeQuery {
            scope: Some("machine".to_owned()),
            key: Some("toolchain.rust.compiler_path".to_owned()),
            limit: 10,
            ..KnowledgeQuery::default()
        })
        .expect("same-address query should succeed");
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits.iter().map(|hit| hit.id.as_str()).collect::<Vec<_>>(),
        vec!["path-a", "path-b"]
    );
}

#[test]
fn text_discovery_and_index_rebuild_include_scope_and_key() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&addressed_record(
            "addressed",
            "opaque value",
            "machine",
            "toolchain.rust.compiler_path",
        ))
        .expect("addressed record should persist");

    let by_key_text = store
        .query(&KnowledgeQuery {
            text: Some("RUST.COMPILER_PATH".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("key text should be searchable");
    assert_eq!(by_key_text.len(), 1);
    assert_eq!(by_key_text[0].id, "addressed");

    fs::remove_dir_all(directory.path().join("index-v1"))
        .expect("derived index should be removable");
    assert_eq!(store.rebuild_index().expect("rebuild should succeed"), 1);
    let by_address = store
        .query(&KnowledgeQuery {
            scope: Some("machine".to_owned()),
            key: Some("toolchain.rust.compiler_path".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("rebuilt index should retain address candidate bits");
    assert_eq!(by_address.len(), 1);
    assert_eq!(by_address[0].id, "addressed");
}

#[test]
fn indexed_query_preserves_unicode_provenance_detail_matching() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    let record = KnowledgeRecord::new(
        "unicode-detail",
        "toolchain observation",
        "fact",
        "observer",
        25,
        Confidence::High,
        KnowledgeState::Active,
        Provenance {
            basis: ProvenanceBasis::Observed,
            detail: Some("Observed at Café Central".to_owned()),
        },
    )
    .expect("fixture should be valid");
    store.insert(&record).expect("record should persist");

    let results = FileStore::new(directory.path())
        .query(&KnowledgeQuery {
            text: Some("CAFÉ CENTRAL".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("unicode provenance query should succeed");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "unicode-detail");
}

#[test]
fn query_filters_metadata_and_excludes_non_active_records_by_default() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    for record in [
        record_with(
            "active-decision",
            "Use stable Rust",
            "decision",
            "planner",
            30,
            KnowledgeState::Active,
        ),
        record_with(
            "stale-decision",
            "Use nightly Rust",
            "decision",
            "planner",
            40,
            KnowledgeState::Stale,
        ),
        record_with(
            "active-fact",
            "Rust is installed",
            "fact",
            "planner",
            50,
            KnowledgeState::Active,
        ),
    ] {
        store.insert(&record).expect("fixture should persist");
    }

    let results = store
        .query(&KnowledgeQuery {
            kind: Some("decision".to_owned()),
            source: Some("planner".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("query should succeed");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "active-decision");
}

#[test]
fn query_returns_newest_matches_first_and_honors_limit() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    for (id, created_at) in [("old", 10), ("new", 30), ("middle", 20)] {
        store
            .insert(&record_with(
                id,
                "shared query text",
                "fact",
                "test",
                created_at,
                KnowledgeState::Active,
            ))
            .expect("fixture should persist");
    }

    let results = store
        .query(&KnowledgeQuery {
            text: Some("shared".to_owned()),
            limit: 2,
            ..KnowledgeQuery::default()
        })
        .expect("query should succeed");

    assert_eq!(
        results
            .iter()
            .map(|record| record.id.as_str())
            .collect::<Vec<_>>(),
        vec!["new", "middle"]
    );
}

#[test]
fn query_rejects_invalid_limits() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());

    let error = store
        .query(&KnowledgeQuery {
            limit: 0,
            ..KnowledgeQuery::default()
        })
        .expect_err("zero-sized output should be rejected");

    assert!(matches!(error, StoreError::InvalidQueryLimit { max: 10 }));
}

#[test]
fn query_rejects_oversized_text_before_scanning() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());

    let error = store
        .query(&KnowledgeQuery {
            text: Some("x".repeat(257)),
            ..KnowledgeQuery::default()
        })
        .expect_err("oversized query text should be rejected");

    assert!(matches!(
        error,
        StoreError::QueryTextTooLong { max_bytes: 256 }
    ));
}

#[test]
fn query_rejects_oversized_address_filters_before_scanning() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    let error = store
        .query(&KnowledgeQuery {
            scope: Some("x".repeat(257)),
            ..KnowledgeQuery::default()
        })
        .expect_err("oversized scope should be rejected");

    assert!(matches!(
        error,
        StoreError::QueryAddressTooLong {
            field: "scope",
            max_bytes: 256
        }
    ));
}

#[test]
fn legacy_store_over_scan_ceiling_requires_then_accepts_index_rebuild() {
    let directory = TestDir::new();
    let records_dir = directory.path().join("records");
    fs::create_dir_all(&records_dir).expect("records directory should be created");

    for index in 0..257 {
        let id = format!("record-{index:03}");
        let content = if index == 256 {
            "unique indexed retrieval needle"
        } else {
            "legacy fixture"
        };
        let record = record_with(
            &id,
            content,
            "fact",
            "store-test",
            index,
            KnowledgeState::Active,
        );
        let serialized = serde_json::to_vec(&record).expect("fixture should serialize");
        fs::write(record_path(directory.path(), &id), serialized)
            .expect("fixture record should be written");
    }

    let store = FileStore::new(directory.path());
    let error = store
        .query(&KnowledgeQuery {
            text: Some("unique indexed retrieval needle".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect_err("legacy query must not silently exceed its scan ceiling");
    assert!(matches!(
        error,
        StoreError::IndexRebuildRequired {
            legacy_scan_limit: 256
        }
    ));

    assert_eq!(store.rebuild_index().expect("rebuild should succeed"), 257);

    let results = FileStore::new(directory.path())
        .query(&KnowledgeQuery {
            text: Some("UNIQUE indexed retrieval needle".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("rebuilt index should support discovery past the legacy ceiling");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "record-256");
}

#[test]
fn index_rebuild_handles_mixed_legacy_and_versioned_record_files() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("versioned", "versioned migration needle"))
        .expect("versioned record should persist");

    fs::remove_dir_all(directory.path().join("index-v1"))
        .expect("fixture should remove the derived index");
    let legacy = record("legacy", "legacy migration needle");
    fs::write(
        record_path(directory.path(), "legacy"),
        serde_json::to_vec(&legacy).expect("legacy record should serialize"),
    )
    .expect("legacy record should be written");

    assert_eq!(
        FileStore::new(directory.path())
            .rebuild_index()
            .expect("mixed-format rebuild should succeed"),
        2
    );

    for (needle, expected_id) in [
        ("versioned migration needle", "versioned"),
        ("legacy migration needle", "legacy"),
    ] {
        let hits = FileStore::new(directory.path())
            .query(&KnowledgeQuery {
                text: Some(needle.to_owned()),
                ..KnowledgeQuery::default()
            })
            .expect("mixed-format indexed query should succeed");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, expected_id);
    }
}

#[test]
fn indexed_query_keeps_broad_candidate_work_bounded() {
    let directory = TestDir::new();
    let records_dir = directory.path().join("records");
    fs::create_dir_all(&records_dir).expect("records directory should be created");

    for index in 0..257 {
        let id = format!("record-{index:03}");
        let record = record_with(
            &id,
            "common indexed phrase",
            "fact",
            "store-test",
            index,
            KnowledgeState::Active,
        );
        let serialized = serde_json::to_vec(&record).expect("fixture should serialize");
        fs::write(record_path(directory.path(), &id), serialized)
            .expect("fixture record should be written");
    }

    let store = FileStore::new(directory.path());
    store.rebuild_index().expect("rebuild should succeed");

    let error = store
        .query(&KnowledgeQuery {
            text: Some("common indexed phrase".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect_err("broad indexed retrieval must remain bounded");

    assert!(matches!(
        error,
        StoreError::QueryCandidateLimitExceeded {
            max_candidates: 256
        }
    ));
}

#[test]
fn supersession_derives_current_state_without_rewriting_history() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record_with(
            "old-toolchain",
            "Rust compiler path is C:/old/rustc.exe",
            "fact",
            "inspector",
            10,
            KnowledgeState::Active,
        ))
        .expect("old record should persist");
    store
        .insert(&record_with(
            "new-toolchain",
            "Rust compiler path is C:/new/rustc.exe",
            "fact",
            "inspector",
            20,
            KnowledgeState::Active,
        ))
        .expect("new record should persist");
    store
        .insert_relation(&relation(
            "supersession-1",
            "new-toolchain",
            KnowledgeRelationKind::Supersedes,
            "old-toolchain",
        ))
        .expect("supersession should persist");

    let current = FileStore::new(directory.path())
        .query(&KnowledgeQuery {
            text: Some("Rust compiler path".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("current query should resolve supersession");
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].id, "new-toolchain");
    assert_eq!(current[0].effective_state, KnowledgeState::Active);

    let superseded = store
        .query(&KnowledgeQuery {
            text: Some("Rust compiler path".to_owned()),
            state: Some(KnowledgeState::Superseded),
            ..KnowledgeQuery::default()
        })
        .expect("historical state should be queryable");
    assert_eq!(superseded.len(), 1);
    assert_eq!(superseded[0].id, "old-toolchain");
    assert_eq!(superseded[0].state, KnowledgeState::Active);
    assert_eq!(superseded[0].effective_state, KnowledgeState::Superseded);

    let status = FileStore::new(directory.path())
        .status("old-toolchain")
        .expect("status should resolve")
        .expect("record should exist");
    assert_eq!(status.effective_state, KnowledgeState::Superseded);
    assert_eq!(status.superseded_by.len(), 1);
    assert_eq!(status.superseded_by[0].id, "supersession-1");
}

#[test]
fn conflict_relations_remove_both_branches_from_default_current_results() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    for record in [
        record_with(
            "branch-a",
            "Configured compiler is stable Rust",
            "fact",
            "observer-a",
            10,
            KnowledgeState::Active,
        ),
        record_with(
            "branch-b",
            "Configured compiler is nightly Rust",
            "fact",
            "observer-b",
            20,
            KnowledgeState::Active,
        ),
    ] {
        store.insert(&record).expect("branch should persist");
    }
    store
        .insert_relation(&relation(
            "conflict-1",
            "branch-a",
            KnowledgeRelationKind::ConflictsWith,
            "branch-b",
        ))
        .expect("conflict should persist");

    let current = store
        .query(&KnowledgeQuery {
            text: Some("Configured compiler".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("current query should succeed");
    assert!(current.is_empty());

    let conflicted = store
        .query(&KnowledgeQuery {
            text: Some("Configured compiler".to_owned()),
            state: Some(KnowledgeState::Conflicted),
            ..KnowledgeQuery::default()
        })
        .expect("conflicted history should be queryable");
    assert_eq!(conflicted.len(), 2);
    assert!(conflicted
        .iter()
        .all(|hit| hit.effective_state == KnowledgeState::Conflicted));
}

#[test]
fn a_resolution_record_can_supersede_both_conflict_branches() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    for record in [
        record_with(
            "branch-a",
            "compiler decision A",
            "fact",
            "a",
            10,
            KnowledgeState::Active,
        ),
        record_with(
            "branch-b",
            "compiler decision B",
            "fact",
            "b",
            20,
            KnowledgeState::Active,
        ),
        record_with(
            "resolution",
            "compiler decision resolved to stable",
            "fact",
            "reviewer",
            30,
            KnowledgeState::Active,
        ),
    ] {
        store.insert(&record).expect("fixture should persist");
    }
    for relation in [
        relation(
            "conflict-1",
            "branch-a",
            KnowledgeRelationKind::ConflictsWith,
            "branch-b",
        ),
        relation(
            "resolve-a",
            "resolution",
            KnowledgeRelationKind::Supersedes,
            "branch-a",
        ),
        relation(
            "resolve-b",
            "resolution",
            KnowledgeRelationKind::Supersedes,
            "branch-b",
        ),
    ] {
        store
            .insert_relation(&relation)
            .expect("evolution relation should persist");
    }

    let current = store
        .query(&KnowledgeQuery {
            text: Some("compiler decision".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("resolved query should succeed");
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].id, "resolution");
}

#[test]
fn new_relation_files_include_an_explicit_schema_version() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    for id in ["a", "b"] {
        store
            .insert(&record(id, &format!("record {id}")))
            .expect("endpoint should persist");
    }
    store
        .insert_relation(&relation(
            "versioned-relation",
            "a",
            KnowledgeRelationKind::ConflictsWith,
            "b",
        ))
        .expect("relation should persist");

    let stored: serde_json::Value = serde_json::from_slice(
        &fs::read(relation_path(directory.path(), "versioned-relation"))
            .expect("relation file should be readable"),
    )
    .expect("relation file should contain JSON");
    assert_eq!(stored["schema_version"], 1);
    assert_eq!(stored["relation"]["id"], "versioned-relation");
}

#[test]
fn explicit_future_relation_schema_versions_fail_closed() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    for id in ["a", "b"] {
        store
            .insert(&record(id, &format!("record {id}")))
            .expect("endpoint should persist");
    }
    let relations_dir = directory.path().join("relations");
    fs::create_dir_all(&relations_dir).expect("relations directory should exist");
    let future = serde_json::json!({
        "schema_version": 2,
        "relation": relation(
            "future-relation",
            "a",
            KnowledgeRelationKind::ConflictsWith,
            "b",
        )
    });
    fs::write(
        relation_path(directory.path(), "future-relation"),
        serde_json::to_vec(&future).expect("future relation fixture should serialize"),
    )
    .expect("future relation fixture should be written");

    let error = store
        .query(&KnowledgeQuery::default())
        .expect_err("unsupported relation schema versions must fail closed");
    assert!(matches!(
        error,
        StoreError::CorruptRelation { ref reason, .. }
            if reason.contains("unsupported relation schema version 2")
    ));
}

#[test]
fn relation_endpoints_must_already_exist() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("existing", "known record"))
        .expect("fixture should persist");

    let error = store
        .insert_relation(&relation(
            "missing-endpoint",
            "missing",
            KnowledgeRelationKind::Supersedes,
            "existing",
        ))
        .expect_err("dangling relation should be rejected");

    assert!(matches!(
        error,
        StoreError::RelationEndpointMissing { ref id } if id == "missing"
    ));
}

#[test]
fn corrupt_relation_data_fails_closed() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("a", "record a"))
        .expect("a should persist");
    store
        .insert(&record("b", "record b"))
        .expect("b should persist");
    store
        .insert_relation(&relation(
            "relation-1",
            "a",
            KnowledgeRelationKind::ConflictsWith,
            "b",
        ))
        .expect("relation should persist");
    fs::write(relation_path(directory.path(), "relation-1"), b"not-json")
        .expect("relation fixture should be corrupted");

    let error = FileStore::new(directory.path())
        .query(&KnowledgeQuery::default())
        .expect_err("corrupt evolution data must not affect currentness silently");
    assert!(matches!(error, StoreError::CorruptRelation { .. }));
}

#[test]
fn stored_supersession_cycles_fail_closed() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("a", "record a"))
        .expect("a should persist");
    store
        .insert(&record("b", "record b"))
        .expect("b should persist");
    let relations_dir = directory.path().join("relations");
    fs::create_dir_all(&relations_dir).expect("relations directory should exist");
    for edge in [
        relation("a-over-b", "a", KnowledgeRelationKind::Supersedes, "b"),
        relation("b-over-a", "b", KnowledgeRelationKind::Supersedes, "a"),
    ] {
        fs::write(
            relation_path(directory.path(), &edge.id),
            serde_json::to_vec(&edge).expect("relation should serialize"),
        )
        .expect("cycle fixture should be written");
    }

    let error = FileStore::new(directory.path())
        .query(&KnowledgeQuery::default())
        .expect_err("stored supersession cycles must fail closed");
    assert!(matches!(error, StoreError::SupersessionCycle));
}

#[test]
fn supersession_cycles_are_rejected() {
    let directory = TestDir::new();
    let store = FileStore::new(directory.path());
    store
        .insert(&record("a", "record a"))
        .expect("a should persist");
    store
        .insert(&record("b", "record b"))
        .expect("b should persist");
    store
        .insert_relation(&relation(
            "a-over-b",
            "a",
            KnowledgeRelationKind::Supersedes,
            "b",
        ))
        .expect("first edge should persist");

    let error = store
        .insert_relation(&relation(
            "b-over-a",
            "b",
            KnowledgeRelationKind::Supersedes,
            "a",
        ))
        .expect_err("cycle should be rejected");

    assert!(matches!(error, StoreError::SupersessionCycle));
}

#[test]
fn concurrent_supersession_writers_cannot_publish_a_cycle() {
    let directory = TestDir::new();
    let writers = 16usize;
    let store = FileStore::new(directory.path());
    for index in 0..writers {
        let id = format!("cycle-{index}");
        store
            .insert(&record(&id, &format!("cycle record {index}")))
            .expect("cycle fixture record should persist");
    }

    let barrier = Arc::new(Barrier::new(writers));
    let mut handles = Vec::with_capacity(writers);
    for index in 0..writers {
        let root = directory.path().to_path_buf();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let subject = format!("cycle-{index}");
            let object = format!("cycle-{}", (index + 1) % writers);
            let edge = relation(
                &format!("cycle-edge-{index}"),
                &subject,
                KnowledgeRelationKind::Supersedes,
                &object,
            );
            barrier.wait();
            FileStore::new(root).insert_relation(&edge)
        }));
    }

    let mut inserted = 0usize;
    let mut cycle_rejections = 0usize;
    for handle in handles {
        match handle
            .join()
            .expect("relation writer thread should not panic")
        {
            Ok(()) => inserted += 1,
            Err(StoreError::SupersessionCycle) => cycle_rejections += 1,
            Err(error) => panic!("concurrent relation write failed unexpectedly: {error}"),
        }
    }

    assert_eq!(inserted, writers - 1);
    assert_eq!(cycle_rejections, 1);
    FileStore::new(directory.path())
        .query(&KnowledgeQuery::default())
        .expect("serialized lifecycle writes must leave an acyclic readable graph");
}

#[test]
fn authoritative_writes_require_configured_authorization() {
    let directory = TestDir::unconfigured();
    let store = FileStore::new(directory.path());

    let error = store
        .insert(&record("record-1", "blocked write"))
        .expect_err("missing authorization must deny authoritative writes");

    assert!(matches!(error, StoreError::AuthorizationNotConfigured));
    assert!(!record_path(directory.path(), "record-1").exists());
    assert!(!directory.path().join("index-v1").exists());
}

#[test]
fn malformed_authorization_policy_fails_closed_before_write_side_effects() {
    let directory = TestDir::unconfigured();
    fs::write(directory.path().join("authorization-v1.json"), b"not-json")
        .expect("malformed policy fixture should be written");

    let error = FileStore::new(directory.path())
        .insert(&record("record-1", "blocked by invalid policy"))
        .expect_err("invalid authorization policy must deny authoritative writes");

    assert!(matches!(
        error,
        StoreError::InvalidAuthorizationPolicy { .. }
    ));
    assert!(!record_path(directory.path(), "record-1").exists());
    assert!(!directory.path().join("index-v1").exists());
}

#[test]
fn authorization_separates_record_and_relation_permissions() {
    let directory = TestDir::unconfigured();
    let store = FileStore::new(directory.path());
    let policy = AuthorizationPolicy::new(vec![
        ClientAuthorization::new("record-writer", true, false)
            .expect("record writer grant should be valid"),
        ClientAuthorization::new("relation-writer", false, true)
            .expect("relation writer grant should be valid"),
    ])
    .expect("authorization policy should be valid");
    store
        .initialize_authorization(&policy)
        .expect("authorization should initialize");

    for id in ["a", "b"] {
        store
            .insert(&record_with(
                id,
                "authorized record",
                "fact",
                "record-writer",
                10,
                KnowledgeState::Active,
            ))
            .expect("record writer should be authorized");
    }

    let error = store
        .insert_relation(&relation_with_source(
            "blocked-relation",
            "a",
            KnowledgeRelationKind::ConflictsWith,
            "b",
            "record-writer",
        ))
        .expect_err("record-only writer must not assert lifecycle relations");
    assert!(matches!(
        error,
        StoreError::AuthorizationDenied {
            ref client_id,
            capability: "write_relations"
        } if client_id == "record-writer"
    ));

    store
        .insert_relation(&relation_with_source(
            "allowed-relation",
            "a",
            KnowledgeRelationKind::ConflictsWith,
            "b",
            "relation-writer",
        ))
        .expect("relation writer should be authorized");

    let successor = record_with(
        "successor",
        "replacement value",
        "fact",
        "record-writer",
        20,
        KnowledgeState::Active,
    );
    let supersedes = relation_with_source(
        "successor-edge",
        "successor",
        KnowledgeRelationKind::Supersedes,
        "a",
        "record-writer",
    );
    let error = store
        .insert_successor(&successor, &supersedes)
        .expect_err("record-only writer must not publish an atomic successor");
    assert!(matches!(
        error,
        StoreError::AuthorizationDenied {
            ref client_id,
            capability: "write_relations"
        } if client_id == "record-writer"
    ));
    assert!(!encoded_path(directory.path(), "successors-v1", "successor").exists());
}

#[test]
fn authorization_initialization_is_write_once() {
    let directory = TestDir::unconfigured();
    let store = FileStore::new(directory.path());
    let first = AuthorizationPolicy::new(vec![
        ClientAuthorization::new("owner", true, true).expect("owner grant should be valid")
    ])
    .expect("first policy should be valid");
    store
        .initialize_authorization(&first)
        .expect("first policy should initialize");

    let replacement =
        AuthorizationPolicy::new(vec![ClientAuthorization::new("replacement", true, true)
            .expect("replacement grant should be valid")])
        .expect("replacement policy should be valid");
    let error = store
        .initialize_authorization(&replacement)
        .expect_err("Synapse must not silently replace local authorization policy");

    assert!(matches!(error, StoreError::AuthorizationAlreadyConfigured));
    let loaded = store
        .authorization_policy()
        .expect("configured policy should be readable");
    assert_eq!(loaded, first);
}

#[test]
fn reads_remain_available_for_legacy_data_before_authorization_is_configured() {
    let directory = TestDir::unconfigured();
    let fixture = record("legacy", "legacy readable knowledge");
    let records_dir = directory.path().join("records");
    fs::create_dir_all(&records_dir).expect("records directory should exist");
    fs::write(
        record_path(directory.path(), "legacy"),
        serde_json::to_vec(&fixture).expect("fixture should serialize"),
    )
    .expect("legacy fixture should be written");

    let store = FileStore::new(directory.path());
    let loaded = store
        .get("legacy")
        .expect("legacy exact read should not require authorization")
        .expect("legacy record should exist");
    assert_eq!(loaded.content, "legacy readable knowledge");

    let found = store
        .query(&KnowledgeQuery {
            text: Some("readable knowledge".to_owned()),
            ..KnowledgeQuery::default()
        })
        .expect("legacy discovery should remain readable");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "legacy");
}

#[test]
fn oversized_files_are_rejected_before_deserialization() {
    let directory = TestDir::new();
    let records_dir = directory.path().join("records");
    fs::create_dir_all(&records_dir).expect("records directory should be created");
    fs::write(
        record_path(directory.path(), "record-1"),
        vec![b'x'; 1024 * 1024 + 1],
    )
    .expect("oversized fixture should be written");

    let error = FileStore::new(directory.path())
        .get("record-1")
        .expect_err("oversized persisted data must be rejected");

    assert!(matches!(
        error,
        StoreError::RecordTooLarge {
            max_bytes: 1_048_576
        }
    ));
}
