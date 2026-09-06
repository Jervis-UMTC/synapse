# Append-Only Knowledge Evolution

## Purpose

Task 6 adds explicit history without introducing destructive mutation. A `KnowledgeRecord` remains immutable and write-once, while a separate append-only `KnowledgeRelation` can state that one record supersedes another or that two records conflict. Retrieval derives an effective lifecycle state from those relation assertions instead of rewriting historical JSON.

This lets one local tool replace or challenge knowledge discovered by another tool while preserving the original content, source, time, confidence, and provenance for later inspection.

## Relation model

`synapse-core` defines `KnowledgeRelation` with:

- `id`: caller-assigned write-once relation identity;
- `subject_id`: the record making the lifecycle relationship;
- `kind`: `supersedes` or `conflicts_with`;
- `object_id`: the other record;
- `source`: the local client asserting the relationship;
- `created_at_unix_ms`: caller-supplied assertion time;
- `provenance`: observed/inferred basis plus optional detail.

Relation IDs, endpoints, and source must contain non-whitespace text. A relation cannot point from a record to itself. Relation deserialization goes through the same validation path as construction.

`supersedes` is directional: `new supersedes old` makes `old` effectively superseded. `conflicts_with` is symmetric for lifecycle resolution even though the stored assertion retains its original subject/object orientation and provenance.

## Effective state

The stored `KnowledgeRecord.state` remains part of the immutable historical record. Task 6 adds a separate relation-derived `effective_state` used by discovery.

Resolution precedence is:

1. a record targeted by any valid `supersedes` relation is effectively `superseded`;
2. otherwise a record participating in any valid `conflicts_with` relation is effectively `conflicted`;
3. otherwise its effective state is its stored `state`.

Supersession intentionally takes precedence over conflict. A resolution record can therefore supersede both sides of a prior conflict, leaving the resolution active while both historical branches remain available as superseded evidence.

Conflicts do not disappear merely because one side is newer. An unresolved conflict remains explicit until clients add sufficient supersession evidence. This favors `CONFLICTED` over silently selecting a winner.

## Storage

Relations are authoritative append-only JSON stored separately from records:

```text
<store-root>/
├── records/
│   └── <hex-record-id>.json
├── relations/
│   └── <hex-relation-id>.json
└── index-v1/
    └── ... derived retrieval data ...
```

Relation filenames use the same path-safe lowercase hexadecimal encoding as record IDs. A relation is limited to 16 KiB, relation IDs share the 100-byte ID limit, and one evolution graph load is bounded to 4,096 relation files.

Relation insertion requires both record endpoints to already exist and deserialize successfully. Relation IDs are write-once. Publication uses the same complete temporary-file plus same-directory hard-link pattern as records, so an existing relation is never silently overwritten.

## Supersession cycles

A new supersession assertion is rejected if it would create a directed cycle. The store also checks the complete loaded graph for cycles. This second check matters for corrupted data or concurrent writers that could otherwise publish opposing edges after each independently observed an acyclic graph.

A stored cycle fails closed with `SupersessionCycle`; Synapse does not attempt to choose a winner from cyclic replacement claims.

## Retrieval integration

`FileStore::query` now returns `KnowledgeHit`. It serializes the complete immutable record fields plus an additional `effective_state` field. The existing query `state` filter applies to effective state, not merely the stored record state.

For example, a record originally stored as:

```json
{"id":"old","state":"active"}
```

can later appear in a historical query with:

```json
{"id":"old","state":"active","effective_state":"superseded"}
```

This makes the distinction explicit instead of pretending the authoritative record file was edited.

The Task 5 index still prefilters text, kind, source, and provenance. It no longer treats the lifecycle byte stored in an index-v1 entry as authoritative for state filtering, because relation-derived effective state can differ from the record's stored state. Candidate records are reopened and effective state is resolved before the final state predicate is applied.

`FileStore::status` and `synapse knowledge status <id>` return the immutable record, its effective state, and the relation assertions that supersede or conflict with it. `synapse knowledge show <id>` deliberately remains the raw authoritative-record view.

## CLI

Add an evolution assertion with:

```text
synapse knowledge relate <relation-id> <subject-id> <supersedes|conflicts> <object-id> <source> <observed|inferred>
```

Inspect resolved currentness with:

```text
synapse knowledge status <record-id>
```

Normal `knowledge find` continues to default to `state = active`, which now means effectively active/current knowledge. Historical branches can be requested explicitly with `--state superseded`, `--state conflicted`, or `--state any`.

## Trust boundary and deferred work

Task 7 now requires `KnowledgeRelation.source` to have the store-local `write_relations` capability before a new relation can be published. This separates ordinary record contribution from the stronger ability to change effective currentness. The relation's `source` remains durable evidence of the claimed writer identity used for that check.

The Task 7 identity is not strongly authenticated: a hostile same-user process with direct store access can still impersonate a source or tamper with files. See [`authorization.md`](authorization.md) for the exact cooperative security boundary and the future IPC/OS-identity requirement.

Task 6/7 still do not add relation retraction, deletion, semantic conflict detection, automatic supersession, provenance trust scoring, transactions spanning record-plus-relation creation, distributed merge rules, authenticated daemon IPC, or synchronization. Those require separate designs rather than hidden mutation of this append-only model.
