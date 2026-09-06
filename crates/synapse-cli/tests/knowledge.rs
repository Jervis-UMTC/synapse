use std::{
    fs,
    path::{Path, PathBuf},
    process::{self, Child, Command},
    sync::atomic::{AtomicU64, Ordering},
};

use serde_json::Value;

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let directory = Self::unconfigured();
        let clients = [
            "writer-tool",
            "machine-inspector",
            "legacy-writer",
            "inspector",
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
            directory.path().join("authorization-v1.json"),
            serde_json::to_vec(&policy).expect("authorization fixture should serialize"),
        )
        .expect("authorization fixture should be written");
        directory
    }

    fn unconfigured() -> Self {
        let sequence = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("synapse-cli-test-{}-{sequence}", process::id()));
        fs::create_dir_all(&path).expect("test directory should be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn synapse(store: Option<&Path>) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_synapse"));
    if let Some(store) = store {
        command.env("SYNAPSE_STORE", store);
    }
    command
}

fn one_shot_ipc_server(store: &Path) -> Child {
    synapse(Some(store))
        .args(["ipc", "serve", "--once"])
        .spawn()
        .expect("one-shot IPC server should start")
}

#[test]
fn knowledge_create_emits_a_serialized_record() {
    let output = synapse(None)
        .args([
            "knowledge",
            "create",
            "record-1",
            "fact",
            "test-client",
            "observed",
            "high",
            "Rust is installed",
        ])
        .output()
        .expect("synapse binary should run");

    assert!(output.status.success());

    let record: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(record["id"], "record-1");
    assert_eq!(record["content"], "Rust is installed");
    assert_eq!(record["kind"], "fact");
    assert_eq!(record["source"], "test-client");
    assert_eq!(record["confidence"], "high");
    assert_eq!(record["state"], "active");
    assert_eq!(record["provenance"]["basis"], "observed");
    assert!(record["created_at_unix_ms"].as_u64().is_some());
}

#[test]
fn knowledge_create_rejects_unknown_provenance_basis() {
    let output = synapse(None)
        .args([
            "knowledge",
            "create",
            "record-1",
            "fact",
            "test-client",
            "guessed",
            "low",
            "Rust might be installed",
        ])
        .output()
        .expect("synapse binary should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid provenance basis"));
}

#[test]
fn knowledge_add_then_show_persists_across_processes() {
    let directory = TestDir::new();
    let added = synapse(Some(directory.path()))
        .args([
            "knowledge",
            "add",
            "shared-1",
            "fact",
            "writer-tool",
            "observed",
            "high",
            "cargo is installed",
        ])
        .output()
        .expect("writer process should run");
    assert!(
        added.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&added.stderr)
    );

    let shown = synapse(Some(directory.path()))
        .args(["knowledge", "show", "shared-1"])
        .output()
        .expect("reader process should run");
    assert!(
        shown.status.success(),
        "show failed: {}",
        String::from_utf8_lossy(&shown.stderr)
    );

    let added_record: Value =
        serde_json::from_slice(&added.stdout).expect("add output should be JSON");
    let shown_record: Value =
        serde_json::from_slice(&shown.stdout).expect("show output should be JSON");
    assert_eq!(shown_record, added_record);
    assert_eq!(shown_record["source"], "writer-tool");
    assert_eq!(shown_record["content"], "cargo is installed");
}

#[test]
fn knowledge_find_discovers_matching_records_without_an_id() {
    let directory = TestDir::new();
    for args in [
        [
            "knowledge",
            "add",
            "rust-toolchain",
            "fact",
            "machine-inspector",
            "observed",
            "high",
            "Rust compiler is installed at C:/Rust/bin/rustc.exe",
        ],
        [
            "knowledge",
            "add",
            "node-toolchain",
            "fact",
            "machine-inspector",
            "observed",
            "high",
            "Node.js is installed",
        ],
    ] {
        let output = synapse(Some(directory.path()))
            .args(args)
            .output()
            .expect("writer process should run");
        assert!(output.status.success());
    }

    let output = synapse(Some(directory.path()))
        .args([
            "knowledge",
            "find",
            "RUST compiler",
            "--source",
            "machine-inspector",
            "--limit",
            "3",
        ])
        .output()
        .expect("reader process should run");
    assert!(
        output.status.success(),
        "find failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records: Value =
        serde_json::from_slice(&output.stdout).expect("find output should be JSON");
    let records = records.as_array().expect("find output should be an array");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["id"], "rust-toolchain");
    assert_eq!(records[0]["source"], "machine-inspector");
}

#[test]
fn knowledge_index_rebuild_restores_discovery_for_a_legacy_store() {
    let directory = TestDir::new();
    let added = synapse(Some(directory.path()))
        .args([
            "knowledge",
            "add",
            "legacy-1",
            "fact",
            "legacy-writer",
            "observed",
            "high",
            "legacy searchable knowledge",
        ])
        .output()
        .expect("writer process should run");
    assert!(added.status.success());

    fs::remove_dir_all(directory.path().join("index-v1"))
        .expect("index should be removable to simulate a pre-index store");

    let rebuilt = synapse(Some(directory.path()))
        .args(["knowledge", "index", "rebuild"])
        .output()
        .expect("index rebuild process should run");
    assert!(
        rebuilt.status.success(),
        "rebuild failed: {}",
        String::from_utf8_lossy(&rebuilt.stderr)
    );
    let rebuild_result: Value =
        serde_json::from_slice(&rebuilt.stdout).expect("rebuild output should be JSON");
    assert_eq!(rebuild_result["indexed_records"], 1);

    let found = synapse(Some(directory.path()))
        .args(["knowledge", "find", "SEARCHABLE knowledge"])
        .output()
        .expect("reader process should run");
    assert!(found.status.success());
    let records: Value = serde_json::from_slice(&found.stdout).expect("find output should be JSON");
    assert_eq!(records.as_array().expect("array output").len(), 1);
    assert_eq!(records[0]["id"], "legacy-1");
}

#[test]
fn knowledge_relations_change_current_discovery_without_overwriting_history() {
    let directory = TestDir::new();
    for args in [
        [
            "knowledge",
            "add",
            "compiler-old",
            "fact",
            "inspector",
            "observed",
            "high",
            "compiler location is C:/old/rustc.exe",
        ],
        [
            "knowledge",
            "add",
            "compiler-new",
            "fact",
            "inspector",
            "observed",
            "high",
            "compiler location is C:/new/rustc.exe",
        ],
    ] {
        let output = synapse(Some(directory.path()))
            .args(args)
            .output()
            .expect("writer process should run");
        assert!(output.status.success());
    }

    let related = synapse(Some(directory.path()))
        .args([
            "knowledge",
            "relate",
            "compiler-supersession",
            "compiler-new",
            "supersedes",
            "compiler-old",
            "inspector",
            "observed",
        ])
        .output()
        .expect("relation process should run");
    assert!(
        related.status.success(),
        "relate failed: {}",
        String::from_utf8_lossy(&related.stderr)
    );

    let found = synapse(Some(directory.path()))
        .args(["knowledge", "find", "compiler location"])
        .output()
        .expect("reader process should run");
    assert!(found.status.success());
    let hits: Value = serde_json::from_slice(&found.stdout).expect("find output should be JSON");
    let hits = hits.as_array().expect("find should return an array");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["id"], "compiler-new");
    assert_eq!(hits[0]["effective_state"], "active");

    let status = synapse(Some(directory.path()))
        .args(["knowledge", "status", "compiler-old"])
        .output()
        .expect("status process should run");
    assert!(status.status.success());
    let status: Value = serde_json::from_slice(&status.stdout).expect("status should be JSON");
    assert_eq!(status["id"], "compiler-old");
    assert_eq!(status["state"], "active");
    assert_eq!(status["effective_state"], "superseded");
    assert_eq!(status["superseded_by"][0]["id"], "compiler-supersession");

    let shown = synapse(Some(directory.path()))
        .args(["knowledge", "show", "compiler-old"])
        .output()
        .expect("raw history process should run");
    assert!(shown.status.success());
    let shown: Value = serde_json::from_slice(&shown.stdout).expect("show should be JSON");
    assert_eq!(shown["state"], "active");
    assert!(shown.get("effective_state").is_none());
}

#[test]
fn authorization_init_bootstraps_a_single_local_writer_and_denies_other_sources() {
    let directory = TestDir::unconfigured();

    let blocked = synapse(Some(directory.path()))
        .args([
            "knowledge",
            "add",
            "before-auth",
            "fact",
            "owner-tool",
            "observed",
            "high",
            "must be blocked",
        ])
        .output()
        .expect("unconfigured writer process should run");
    assert_eq!(blocked.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("authorization is not configured"));

    let initialized = synapse(Some(directory.path()))
        .args(["authorization", "init", "owner-tool"])
        .output()
        .expect("authorization bootstrap should run");
    assert!(
        initialized.status.success(),
        "authorization init failed: {}",
        String::from_utf8_lossy(&initialized.stderr)
    );

    let allowed = synapse(Some(directory.path()))
        .args([
            "knowledge",
            "add",
            "after-auth",
            "fact",
            "owner-tool",
            "observed",
            "high",
            "authorized knowledge",
        ])
        .output()
        .expect("authorized writer process should run");
    assert!(allowed.status.success());

    let denied = synapse(Some(directory.path()))
        .args([
            "knowledge",
            "add",
            "intruder-write",
            "fact",
            "intruder-tool",
            "observed",
            "high",
            "unauthorized knowledge",
        ])
        .output()
        .expect("unauthorized writer process should run");
    assert_eq!(denied.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&denied.stderr).contains("not authorized"));

    let shown = synapse(Some(directory.path()))
        .args(["knowledge", "show", "after-auth"])
        .output()
        .expect("reader process should run");
    assert!(shown.status.success());
}

#[test]
fn ipc_authenticates_trusted_executable_and_rejects_source_spoofing() {
    let directory = TestDir::unconfigured();

    let initialized = synapse(Some(directory.path()))
        .args(["authorization", "init", "ipc-client"])
        .output()
        .expect("authorization bootstrap should run");
    assert!(initialized.status.success());

    let trusted = synapse(Some(directory.path()))
        .args(["ipc", "trust", "ipc-client"])
        .arg(env!("CARGO_BIN_EXE_synapse"))
        .output()
        .expect("IPC trust bootstrap should run");
    assert!(
        trusted.status.success(),
        "IPC trust failed: {}",
        String::from_utf8_lossy(&trusted.stderr)
    );

    let mut writer_server = one_shot_ipc_server(directory.path());
    let written = synapse(Some(directory.path()))
        .env("SYNAPSE_IPC", "1")
        .args([
            "knowledge",
            "add",
            "ipc-record",
            "fact",
            "ipc-client",
            "observed",
            "high",
            "authenticated IPC knowledge",
        ])
        .output()
        .expect("IPC writer should run");
    assert!(
        written.status.success(),
        "authenticated IPC write failed: {}",
        String::from_utf8_lossy(&written.stderr)
    );
    assert!(writer_server
        .wait()
        .expect("IPC server should exit")
        .success());

    let mut reader_server = one_shot_ipc_server(directory.path());
    let found = synapse(Some(directory.path()))
        .env("SYNAPSE_IPC", "1")
        .args(["knowledge", "find", "authenticated IPC"])
        .output()
        .expect("IPC reader should run");
    assert!(
        found.status.success(),
        "IPC read failed: {}",
        String::from_utf8_lossy(&found.stderr)
    );
    assert!(reader_server
        .wait()
        .expect("IPC server should exit")
        .success());
    let hits: Value = serde_json::from_slice(&found.stdout).expect("IPC find should emit JSON");
    assert_eq!(
        hits.as_array()
            .expect("find output should be an array")
            .len(),
        1
    );
    assert_eq!(hits[0]["id"], "ipc-record");

    let mut spoof_server = one_shot_ipc_server(directory.path());
    let spoofed = synapse(Some(directory.path()))
        .env("SYNAPSE_IPC", "1")
        .args([
            "knowledge",
            "add",
            "spoofed-record",
            "fact",
            "another-client",
            "observed",
            "high",
            "must not persist",
        ])
        .output()
        .expect("spoofed IPC writer should run");
    assert_eq!(spoofed.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&spoofed.stderr)
        .contains("does not match authenticated IPC client"));
    assert!(spoof_server
        .wait()
        .expect("IPC server should exit")
        .success());

    let absent = synapse(Some(directory.path()))
        .args(["knowledge", "show", "spoofed-record"])
        .output()
        .expect("direct verification reader should run");
    assert_eq!(absent.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&absent.stderr).contains("not found"));
}

#[test]
fn knowledge_show_reports_missing_records() {
    let directory = TestDir::new();
    let output = synapse(Some(directory.path()))
        .args(["knowledge", "show", "missing"])
        .output()
        .expect("synapse binary should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not found"));
}
