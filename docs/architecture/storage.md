# Durable Local Storage

## Purpose

Task 3 adds the first durable local storage layer for Synapse. Its acceptance criterion is intentionally small: one local process can persist a validated `KnowledgeRecord`, terminate, and another unrelated process can retrieve the same record by ID.

Storage lives in the separate `synapse-store` crate. `synapse-core` remains transport- and storage-independent.

## File store layout

`FileStore` stores authoritative record and evolution JSON under a caller-selected root, with the retrieval index kept as derived data:

```text
<store-root>/
├── authorization-v1.json
├── ipc-trust-v1/
│   └── <sha256-of-client-id>.json
├── records/
│   └── <hex-encoded-record-id>.json
├── relations/
│   └── <hex-encoded-relation-id>.json
└── index-v1/
    └── ... derived entries ...
```

The filename is the lowercase hexadecimal encoding of the UTF-8 ID bytes. Caller-controlled IDs therefore never become path components containing `/`, `\\`, `..`, drive prefixes, or other filesystem syntax.

The file store currently limits IDs to 100 UTF-8 bytes and serialized records to 1 MiB. Reads enforce the same record-size bound before deserialization.

## Write semantics

Task 7 checks store-local authorization before authoritative write work begins. `KnowledgeRecord.source` must have `write_records`; `KnowledgeRelation.source` must have `write_relations`. Missing, malformed, unsupported, or denying policy fails closed before record index publication or relation publication. See [`authorization.md`](authorization.md) for policy format and the cooperative identity limitation.

Record insertion remains write-once by record ID. A second insert using an existing ID returns an error and does not replace the original record. Task 6 adds separate write-once relation assertions for supersession and conflict; it still never edits the original record JSON. See [`evolution.md`](evolution.md) for lifecycle resolution, relation bounds, and cycle behavior.

Before publication, the store serializes the already-validated domain object to a temporary file in the same `records` directory, writes the complete payload, and synchronizes the file. For stores using the Task 5 retrieval index, the complete derived index entry is published first. The store then publishes the authoritative record with a same-filesystem hard link. Because link creation fails when the destination already exists, concurrent writers cannot silently overwrite an existing record, and a newly visible authoritative record is never intentionally published before its retrieval entry is prepared.

A crash before publication can leave an ignored temporary file. A crash after publication can leave both names pointing to the same completed file until the temporary name is cleaned up. Readers only address the final encoded record path. Filesystems that cannot create the required hard link return an I/O error instead of falling back to an overwrite-prone publication path.

## Read validation

Retrieval is fail-closed:

- missing files return `None` at the store API and `not found` from the CLI;
- files over 1 MiB are rejected before JSON parsing;
- JSON must deserialize through the `KnowledgeRecord` validation path;
- the ID inside the record must exactly match the requested ID.

Corrupted or mismatched persisted files therefore do not become trusted knowledge objects.

## Local data directory

The CLI uses `SYNAPSE_STORE` when it is set. This is the supported override for tests, automation, isolated clients, and future local integrations.

Without the override, the current defaults are:

- Windows: `%LOCALAPPDATA%\\Synapse`
- macOS: `$HOME/Library/Application Support/Synapse`
- other Unix platforms: `$XDG_DATA_HOME/synapse`, falling back to `$HOME/.local/share/synapse`

On Unix, directories newly created by the store or IPC trust layer request mode `0700` and temporary files used to publish records, relations, index data, the authorization policy, or executable-trust entries request mode `0600`. Existing parent permissions are not rewritten. On Windows, files and directories inherit the local filesystem ACL policy. Task 8 does not rewrite those ACLs or install a dedicated service account.

## CLI boundary

Before the first authoritative write, a store can bootstrap one cooperative writer identity with:

```text
synapse authorization init <client-id>
```

The generated Task 7 policy grants that client both record and relation write capabilities. Reads remain available before authorization is configured; later writes require their `source` to be present in the policy with the appropriate permission.

Task 2's non-persistent proof command remains available:

```text
synapse knowledge create <id> <kind> <source> <observed|inferred> <unknown|low|medium|high> <content>
```

Task 3 adds durable insertion and retrieval:

```text
synapse knowledge add <id> <kind> <source> <observed|inferred> <unknown|low|medium|high> <content>
synapse knowledge show <id>
```

`knowledge add` constructs the same validated domain record as `knowledge create`, persists it, then prints the persisted value as JSON. `knowledge show` opens the local store in a separate process and prints the validated persisted record.

Task 8 adds an IPC execution path. After `synapse ipc trust <client-id> <executable-path>` and `synapse ipc serve`, setting `SYNAPSE_IPC=1` routes add/show/find/relate/status through the local service. IPC writes authenticate the peer executable before the same `FileStore` authorization and persistence rules run. See [`ipc.md`](ipc.md).

## Deferred work

This file store is a bootstrap persistence mechanism, not the final security or retrieval architecture. Task 4 added bounded lexical discovery, Task 5 a durable derived index, Task 6 append-only conflict/supersession history, Task 7 write authorization, and Task 8 an OS-bound local IPC write-authentication path. See [`retrieval.md`](retrieval.md), [`index.md`](index.md), [`evolution.md`](evolution.md), [`authorization.md`](authorization.md), and [`ipc.md`](ipc.md). Semantic ranking, destructive mutation/deletion, relation retraction, transactions spanning record-plus-relation creation, OS service/account isolation, process-image attestation, embeddings, and synchronization remain deferred.

The store also does not attempt to detect secrets inside caller-provided content. Synapse clients should not submit passwords, tokens, private keys, cookies, or other credentials. A future ingestion/security boundary must define explicit secret filtering and authorization before broader automated capture is enabled.
