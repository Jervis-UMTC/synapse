# Synapse Architecture Overview

## Purpose

Synapse exists to make useful knowledge discovered by one local tool safely reusable by another unrelated local tool.

## Validated constraints

- **Local-first:** v0.1 must be useful on one machine without requiring a hosted control plane.
- **Agent-agnostic:** core behavior cannot depend on one model, agent framework, or integration ecosystem.
- **Generic interfaces:** named tools and integrations are clients or providers of Synapse, not owners of its architecture.
- **Durable knowledge over activity logs:** retain useful facts, decisions, constraints, findings, procedures, provenance, and state rather than indiscriminately accumulating transcripts or tool-call history.
- **Provenance matters:** future knowledge records must distinguish observation from inference and preserve stale, conflicted, superseded, uncertain, or unknown state rather than silently promoting it to truth.
- **Security by default:** future interfaces must validate inputs, bound outputs, protect filesystem boundaries, avoid secret capture, and prefer local IPC over exposed network services when appropriate.

## Task 1 boundary

The initial repository foundation has two Rust packages:

- `synapse-core`: model- and transport-independent domain code.
- `synapse-cli`: the `synapse` command-line executable and a first ordinary local client of the core.

Task 1 deliberately does not choose a database schema, daemon protocol, semantic-retrieval stack, MCP design, provider API, GUI, distributed synchronization model, or Lean integration. Those choices require their own validated task boundaries and tests.

## Task 2 boundary

Task 2 adds the first transport- and storage-independent knowledge domain model. `KnowledgeRecord` preserves source, confidence, lifecycle state, whether provenance is observed or inferred, and an optional paired `scope + key` logical address. Required text fields and paired address shape are validated on construction and deserialization. The CLI can construct a record and emit JSON as a proof exchange path, while persistence and retrieval remain outside the core domain crate.

See [`knowledge-model.md`](knowledge-model.md) for the current record contract and deferred decisions.

## Task 3 boundary

Task 3 introduces `synapse-store`, a storage implementation kept separate from the domain crate. The first `FileStore` persists one validated JSON record per path-safe encoded ID, rejects duplicate IDs rather than silently overwriting knowledge, bounds stored records, and revalidates data on read. The CLI now proves cross-process reuse with `knowledge add` followed by `knowledge show`.

See [`storage.md`](storage.md) for persistence guarantees, data-directory behavior, safety boundaries, and deferred work.

## Task 4 boundary

Task 4 adds bounded lexical discovery without introducing an AI or semantic-search dependency. `KnowledgeQuery` supports text plus exact metadata filters, including optional exact `scope` and `key` filters, defaults to active knowledge, returns at most 10 validated records in deterministic newest-first order, and refuses to scan beyond bounded candidate/legacy-scan limits rather than returning an arbitrary partial view. The CLI exposes this as `synapse knowledge find` with JSON-array output for unrelated local tools.

See [`retrieval.md`](retrieval.md) for matching semantics, bounds, ordering, and the scale trigger for a future durable index.

## Task 5 boundary

Task 5 adds a durable derived lexical index in `synapse-store` so discovery is no longer limited to scanning 256 authoritative record files. Compact Bloom-filter entries plus exact metadata fingerprints identify a bounded candidate set; every candidate is still reopened and fully validated against the authoritative `KnowledgeRecord`. Legacy stores retain bounded compatibility behavior and can be upgraded with `synapse knowledge index rebuild`. The index remains model-agnostic and introduces no semantic-ranking dependency.

See [`index.md`](index.md) for durability, rebuild, corruption, and scale boundaries.

## Task 6 boundary

Task 6 adds append-only `KnowledgeRelation` assertions for directional supersession and symmetric conflict without rewriting historical records. `synapse-store` persists relations separately, rejects missing endpoints and supersession cycles, and derives an `effective_state` used by normal discovery. Query output now preserves both the immutable stored `state` and the resolved `effective_state`; `knowledge status` exposes the relation evidence, while `knowledge show` remains the raw record view.

See [`evolution.md`](evolution.md) for relation semantics, conflict resolution, bounds, and trust limitations.

## Task 7 boundary

Task 7 adds a versioned store-local authorization policy for authoritative writes. The durable `source` on a record or relation is treated as the claimed client identity and must have `write_records` or `write_relations` respectively. Missing or invalid policy denies new authoritative writes before side effects, while existing reads, discovery, status inspection, and derived index rebuild remain available. The policy is bounded to 64 clients / 64 KiB and can be bootstrapped once with `synapse authorization init <client-id>`.

This is deliberately cooperative authorization rather than strong authentication: a hostile same-user process with direct filesystem access can impersonate another source or tamper with policy/data. A future authenticated local IPC boundary must provide OS-backed identity before Synapse can claim protection against such processes.

See [`authorization.md`](authorization.md) for policy format, capability semantics, migration behavior, and the exact security claim.

## Task 8 boundary

Task 8 adds `synapse-ipc`, a local-socket service that can mediate ordinary knowledge reads and authoritative writes without exposing a TCP listener. Write requests require an OS-reported peer process ID, a trusted executable SHA-256 mapping to exactly one Synapse client ID, an exact match between that authenticated identity and the record/relation `source`, and the existing Task 7 capability grant. Frames, executable hashing, trust entries, and frame I/O time are explicitly bounded. Reads remain available without writer authority.

The current Windows runtime proof uses local named pipes and rejects source spoofing from a connected process. This improves Task 7's claimed-string boundary, but safe deployment still requires OS permissions that prevent ordinary clients from modifying the store, authorization policy, or trust directory directly. Task 8 does not install a privileged service or claim process-image attestation.

See [`ipc.md`](ipc.md) for transport, trust bootstrap, peer authentication, bounds, CLI routing, and remaining OS-isolation requirements.

## Schema compatibility boundary

Authoritative addressless records and relations retain explicit `schema_version = 1` JSON envelopes. Records (and atomic successors) carrying paired `scope + key` addressing use record schema version `2`; schema-v1 records are forbidden from carrying address fields. Existing flat v0.1 addressless record/relation files remain readable as implicit schema 1, while an explicit unsupported version fails closed before Synapse interprets the payload. Derived index identity is computed from the exact authoritative bytes that were read, allowing mixed legacy/current stores to rebuild without rewriting history.

IPC request and response JSON now uses protocol v2 for the address-aware contract while retaining safe v1 compatibility for addressless operations. Missing protocol version is treated as the original v1 shape. Address-aware writes/queries use v2-only operation names so a pre-v2 server rejects them instead of silently discarding `scope` / `key`; a current server also refuses to return addressed data to a v1 peer. Protocol, executable-trust-entry, and endpoint-namespace versions remain independent.

## Stable knowledge address boundary

`KnowledgeRecord.scope + key` gives unrelated tools a shared logical name for the fact a record describes. For example, different clients can refer to `machine / toolchain.rust.compiler_path` even when their human-readable `content` strings differ. The address is optional for backward compatibility and is not a unique primary key: multiple active records may share it, allowing uncertainty or disagreement to remain visible. Exact address retrieval returns all current matches within normal bounds; append-only `supersedes` / `conflicts_with` relations remain responsible for explicit lifecycle resolution.

The existing index-v1 binary layout is retained. `scope` and `key` contribute to the candidate-only Bloom filter, while the authoritative record is reopened for exact case-sensitive address comparison. This preserves the established rule that derived index data cannot become knowledge truth.

## Stable store identity boundary

`store-identity-v1.json` gives each initialized store a durable 64-hex-character identity used for local IPC naming. Bootstrap intentionally seeds the ID with the old path-derived SHA-256 value, preserving the previous endpoint name for an existing store first opened at the same path. After publication, the identity file is authoritative, so path aliases and later directory moves no longer rename the endpoint. The ID is bounded, validated, write-once through the API, and explicitly not a credential. Copying it clones endpoint identity; simultaneous cloned stores therefore require operator separation until a future explicit fork workflow exists.

## Atomic replacement boundary

A new `FileStore::insert_successor` / `synapse knowledge replace` path publishes one successor record and its single `supersedes` assertion as one versioned authoritative file. A synchronized hard-link publication means readers never intentionally observe the new record as current without the paired supersession assertion. A root shared/exclusive OS file lock coordinates multi-file currentness reads with all authoritative writes, while record/relation IDs and existing size/cycle/authorization bounds remain shared across standalone and combined storage. The IPC protocol exposes the same generic operation and authenticates both nested sources to the same peer identity.

This is deliberately a focused one-predecessor replacement primitive, not a general transaction engine or multi-record commit system.

## Continuity note

The repository contained only `LICENSE` when Task 1 began on September 5, 2026; the previously approved v0.1 plan was not present in Git or GitHub. This document records only constraints that are validated by the current project direction. Future implementation tasks should be preserved under `docs/` once their current design is revalidated, so repository state—not chat history—remains authoritative.
