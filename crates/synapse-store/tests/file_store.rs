use std::{
    fs,
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

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
