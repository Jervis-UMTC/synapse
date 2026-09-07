# Durable Local Storage

## Purpose

Task 3 adds the first durable local storage layer for Synapse. Its acceptance criterion is intentionally small: one local process can persist a validated `KnowledgeRecord`, terminate, and another unrelated process can retrieve the same record by ID.

Storage lives in the separate `synapse-store` crate. `synapse-core` remains transport- and storage-independent.

## File store layout

`FileStore` stores authoritative record and evolution JSON under a caller-selected root, with the retrieval index kept as derived data:

```text
<store-root>/
├── .state.lock
├── store-identity-v1.json
├── authorization-v1.json
├── ipc-trust-v1/
│   └── <sha256-of-client-id>.json
├── records/
│   └── <hex-encoded-record-id>.json
├── relations/
│   ├── .mutation.lock
│   └── <hex-encoded-relation-id>.json
├── successors-v1/
│   └── <hex-encoded-new-record-id>.json
└── index-v1/
    └── ... derived entries ...
```

The filename is the lowercase hexadecimal encoding of the UTF-8 ID bytes. Caller-controlled IDs therefore never become path components containing `/`, `\\`, `..`, drive prefixes, or other filesystem syntax.

## Stable store identity

`FileStore::store_id` lazily creates and then reads `store-identity-v1.json`. The file is bounded to 256 bytes and contains version `1` plus a 64-character lowercase hexadecimal store ID. It is store metadata, not a credential, authorization token, or source of knowledge truth.

Initialization uses the same private temporary-file, file sync, and same-directory hard-link publication pattern as other write-once metadata. Concurrent initializers may compute different candidates when they spell the same path differently, but only one final identity file can win; losing initializers reload and use that winner. Malformed, oversized, or unsupported identity files fail closed instead of being replaced automatically.

For migration compatibility, a store with no identity file seeds its first ID from the full SHA-256 digest used by the original path-derived IPC endpoint algorithm. The current endpoint uses the first 16 digest bytes from that stored ID, so an existing store first opened at the same configured path keeps its previous `synapse-v1-...` endpoint name. After the file exists, it—not the path spelling—is authoritative: aliases such as `.` or symlink/case variants that reach the same file converge on the same identity, and moving the complete store directory preserves the endpoint. A running service still needs to reopen/restart after a physical move because its `FileStore` path does not change automatically.

Copying `store-identity-v1.json` copies the store's endpoint identity as well. Two independently active clones with the same identity can therefore contend for the same local endpoint; clone/fork identity management is deferred rather than silently rewriting a copied store.

The file store currently limits IDs to 100 UTF-8 bytes and each complete serialized record file to 1 MiB. New authoritative record files use an explicit schema envelope:

```json
{
  "schema_version": 1,
  "record": {
    "id": "...",
    "content": "..."
  }
}
```

The 1 MiB bound applies to the complete envelope, not only the nested domain record. Addressless records continue to write schema version `1`. A record with the paired `scope + key` logical address writes schema version `2`; schema-v1 records are explicitly forbidden from carrying those fields so an older format cannot silently acquire semantics it did not define. Pre-versioning v0.1 files that contain a flat addressless `KnowledgeRecord` object remain readable as implicit schema 1 so existing local stores do not require destructive migration. If a file explicitly declares an unsupported `schema_version`, Synapse fails closed instead of guessing how to interpret a future format.

## Write semantics

Task 7 checks store-local authorization before authoritative write work begins. `KnowledgeRecord.source` must have `write_records`; `KnowledgeRelation.source` must have `write_relations`. Missing, malformed, unsupported, or denying policy fails closed before record index publication or relation publication. See [`authorization.md`](authorization.md) for policy format and the cooperative identity limitation.

Record insertion remains write-once by record ID. A second insert using an existing ID returns an error and does not replace the original record. Task 6 adds separate write-once relation assertions for supersession and conflict; it still never edits the original record JSON. Relation validation plus publication is serialized with the OS-backed `relations/.mutation.lock`, preventing concurrent lifecycle writers from jointly publishing a supersession cycle after validating the same earlier graph. Lock acquisition is bounded to five seconds. See [`evolution.md`](evolution.md) for lifecycle resolution, relation bounds, locking, and cycle behavior.

All authoritative record/relation mutations also take the root `.state.lock` exclusively. `query`, `status`, and public index rebuild take the same OS-backed lock in shared mode, so a reader that combines record/index data with the lifecycle graph cannot straddle an authoritative publication boundary. The lock is coordination metadata only and is not knowledge.

`FileStore::insert_successor` handles the common replacement operation without a two-step visibility window. It writes one versioned object under `successors-v1/<hex-new-record-id>.json` containing the new record and exactly one `supersedes` relation from that record to an existing predecessor. Addressless successors use schema version `1`; addressed successors use schema version `2`. The complete file is synchronized and then hard-linked to its final path once; before that link neither nested object is authoritative, and after it both are. Temporary crash remnants are ignored. The new record and embedded relation still obey their existing 1 MiB and 16 KiB component bounds and share the normal record/relation ID namespaces.

Before publication, the store serializes the already-validated domain object and checks whether the write-once final record path already exists. An ordinary duplicate is rejected before any derived index work, so repeated rejected replacements cannot consume index capacity. For a new ID, the complete derived Task 5 index entry is published first, then the store writes the authoritative payload to a temporary file in the same `records` directory, synchronizes it, and publishes the final record with a same-filesystem hard link.

The preflight is not the concurrency authority: two writers can both observe an unused ID. The final hard link remains the atomic winner. If a writer loses that race after preparing a different content-addressed index entry, it reloads the winning authoritative record, ensures the winner's index entry exists, and removes the losing entry. A same-content loser retains the shared entry. Thus concurrent duplicate attempts cannot silently overwrite an existing record or accumulate losing index entries, while a newly visible authoritative record is still never intentionally published before its retrieval entry is prepared.

A crash before publication can leave an ignored temporary file or a stale derived index candidate prepared before the authoritative hard link. Such candidates cannot directly become knowledge because query results are reopened through the authoritative record path. A crash after publication can leave both record names pointing to the same completed file until the temporary name is cleaned up. Readers only address the final encoded record path. Filesystems that cannot create the required hard link return an I/O error instead of falling back to an overwrite-prone publication path.

## Read validation

Retrieval is fail-closed:

- missing files return `None` at the store API and `not found` from the CLI;
- files over 1 MiB are rejected before JSON parsing;
- standalone JSON may be an addressless `schema_version = 1` record envelope, an addressed `schema_version = 2` record envelope, or the legacy flat addressless v0.1 record shape;
- atomic-successor JSON may use schema version `1` for an addressless successor or schema version `2` for an addressed successor;
- an explicit unsupported schema version is rejected before its payload is interpreted;
- nested or legacy records/relations must deserialize through their validated domain paths;
- the ID inside the record must exactly match the requested ID.

Corrupted or mismatched persisted files therefore do not become trusted knowledge objects.

## Local data directory

The CLI uses `SYNAPSE_STORE` when it is set. This is the supported override for tests, automation, isolated clients, and future local integrations.

Without the override, the current defaults are:

- Windows: `%LOCALAPPDATA%\\Synapse`
- macOS: `$HOME/Library/Application Support/Synapse`
- other Unix platforms: `$XDG_DATA_HOME/synapse`, falling back to `$HOME/.local/share/synapse`

On Unix, directories newly created by the store or IPC trust layer request mode `0700` and temporary files used to publish records, relations, index data, the store identity, the authorization policy, or executable-trust entries request mode `0600`. Existing parent permissions are not rewritten. On Windows, files and directories inherit the local filesystem ACL policy. Task 8 does not rewrite those ACLs or install a dedicated service account.

## CLI boundary

Before the first authoritative write, a store can bootstrap one cooperative writer identity with:

```text
synapse authorization init <client-id>
```

The generated Task 7 policy grants that client both record and relation write capabilities. Reads remain available before authorization is configured; later writes require their `source` to be present in the policy with the appropriate permission.

Task 2's non-persistent proof command remains available:

```text
synapse knowledge create <id> <kind> <source> <observed|inferred> <unknown|low|medium|high> <content> [--scope <scope> --key <key>]
```

Task 3 adds durable insertion and retrieval, while the evolution layer also provides atomic replacement:

```text
synapse knowledge add <id> <kind> <source> <observed|inferred> <unknown|low|medium|high> <content> [--scope <scope> --key <key>]
synapse knowledge replace <new-id> <kind> <source> <observed|inferred> <unknown|low|medium|high> <content> <relation-id> <old-id> [--scope <scope> --key <key>]
synapse knowledge show <id>
```

`knowledge add` constructs the same validated domain record as `knowledge create`, persists it, then prints the persisted value as JSON. `knowledge replace` publishes the new record and its one `supersedes` assertion as a single authoritative commit and requires both write capabilities. `knowledge show` opens the local store in a separate process and prints the validated persisted record.

Task 8 adds an IPC execution path. After `synapse ipc trust <client-id> <executable-path>` and `synapse ipc serve`, setting `SYNAPSE_IPC=1` routes add/replace/show/find/relate/status through the local service. IPC writes authenticate the peer executable before the same `FileStore` authorization and persistence rules run. See [`ipc.md`](ipc.md).

## Deferred work

This file store is a bootstrap persistence mechanism, not the final security or retrieval architecture. Task 4 added bounded lexical discovery, Task 5 a durable derived index, Task 6 append-only conflict/supersession history, Task 7 write authorization, and Task 8 an OS-bound local IPC write-authentication path. See [`retrieval.md`](retrieval.md), [`index.md`](index.md), [`evolution.md`](evolution.md), [`authorization.md`](authorization.md), and [`ipc.md`](ipc.md). Semantic ranking, destructive mutation/deletion, relation retraction, general multi-record/multi-relation transactions beyond the focused atomic successor operation, OS service/account isolation, process-image attestation, embeddings, and synchronization remain deferred.

The store also does not attempt to detect secrets inside caller-provided content. Synapse clients should not submit passwords, tokens, private keys, cookies, or other credentials. A future ingestion/security boundary must define explicit secret filtering and authorization before broader automated capture is enabled.
