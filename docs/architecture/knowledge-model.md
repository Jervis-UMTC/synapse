# Knowledge Record Model

## Purpose

Task 2 introduces Synapse's first durable knowledge domain primitive: `KnowledgeRecord`. It is the smallest transport- and storage-independent representation that lets one local client describe useful machine knowledge in a form another unrelated client can understand.

This task defines the record shape and validation rules only. It does not introduce persistence, retrieval, a daemon, IPC, embeddings, model integration, or provider-specific behavior.

## Record fields

A knowledge record contains:

- `id`: caller-assigned record identity. Core does not choose an ID-generation policy.
- `content`: the useful knowledge being shared.
- `kind`: an extensible caller-supplied category such as `fact`, `decision`, `constraint`, or `procedure`.
- `source`: the local client or source that supplied the record. For durable Task 7 writes, `synapse-store` treats this value as the claimed client identity checked by the local authorization policy.
- `created_at_unix_ms`: caller-supplied creation time represented as Unix milliseconds.
- `confidence`: `unknown`, `low`, `medium`, or `high`.
- `state`: `active`, `stale`, `conflicted`, `superseded`, or `unknown`.
- `provenance`: whether the knowledge was `observed` or `inferred`, plus optional provenance detail.

`id`, `content`, `kind`, and `source` must contain non-whitespace text. Validation applies both when records are constructed in Rust and when records are deserialized, so serialized input cannot bypass those invariants.

## Design decisions

### Observation and inference are distinct

An observation is information a source directly obtained from the machine or another authoritative input. An inference is a conclusion derived from other information. Synapse preserves that distinction instead of silently promoting inference into observation.

### Confidence and lifecycle state are separate

Confidence describes certainty in a record. Lifecycle state describes whether the record is currently usable, stale, conflicted, superseded, or not yet classified. Keeping them separate allows future retrieval logic to handle uncertainty without deleting provenance or history.

### Identity and time policy stay outside the core

`synapse-core` accepts an ID and timestamp instead of generating them. This avoids prematurely coupling the domain model to a database, UUID scheme, process clock policy, daemon, or distributed system.

### JSON is a proof transport, not storage

The record implements Serde serialization. Task 2 introduced `synapse knowledge create`, which prints JSON to demonstrate that an ordinary local client can construct and exchange the domain object without persisting it. Task 3 keeps `knowledge create` non-persistent and adds separate storage-backed `knowledge add` and `knowledge show` commands.

## CLI proof path

The current proof command is:

```text
synapse knowledge create <id> <kind> <source> <observed|inferred> <unknown|low|medium|high> <content>
```

The CLI supplies the current Unix-millisecond timestamp and initially marks newly created records as `active`. The output is a serialized `KnowledgeRecord` suitable for inspection or piping to another local process.

## Evolution relationship

Task 6 adds a second storage-independent domain primitive, `KnowledgeRelation`, without changing record immutability. A relation has its own caller-assigned ID, source, timestamp, and provenance and can assert directional `supersedes` or symmetric `conflicts_with` semantics between two different record IDs. The core validates relation shape but does not perform storage lookup or lifecycle resolution.

Persistence, effective-state resolution, and local write authorization remain outside `synapse-core` in `synapse-store`. The core does not authenticate or authorize `source`; Task 7's file-store boundary gives it the additional meaning of a claimed writer identity for authoritative persistence. See [`storage.md`](storage.md), [`evolution.md`](evolution.md), and [`authorization.md`](authorization.md).

## Deferred work

Task 7 adds cooperative local write authorization outside this domain model. Strong writer authentication, bounded ingestion beyond existing file/query limits, secret handling, trust weighting, semantic conflict detection, relation retraction, and retrieval ranking remain separate tasks. Semantic search, MCP, provider integrations, GUI surfaces, distributed synchronization, and Lean remain optional future layers rather than dependencies of this model.
