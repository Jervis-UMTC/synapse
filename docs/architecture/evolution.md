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

Relations are authoritative append-only JSON. Ordinary assertions remain separate from records, while an atomic successor stores one new record and its `supersedes` assertion together:

```text
<store-root>/
├── .state.lock
├── records/
│   └── <hex-record-id>.json
├── relations/
│   ├── .mutation.lock
│   └── <hex-relation-id>.json
├── successors-v1/
│   └── <hex-new-record-id>.json
└── index-v1/
    └── ... derived retrieval data ...
```

Relation filenames use the same path-safe lowercase hexadecimal encoding as record IDs. A complete standalone relation file is limited to 16 KiB, relation IDs share the 100-byte ID limit, and the loaded evolution graph is bounded to 4,096 total assertions across standalone relation files and atomic-successor files.

New relation files use the same explicit schema-version boundary as records:

```json
{
  "schema_version": 1,
  "relation": {
    "id": "...",
    "kind": "supersedes"
  }
}
```

Legacy flat v0.1 `KnowledgeRelation` JSON remains readable as implicit schema 1. An explicitly unsupported relation schema version fails closed before the future payload is interpreted. The 16 KiB size bound applies to the complete versioned envelope.

Relation insertion requires both record endpoints to already exist and deserialize successfully. Relation IDs are write-once. Publication uses the same complete temporary-file plus same-directory hard-link pattern as records, so an existing relation is never silently overwritten.

Relation validation and publication are serialized across local processes with an OS-backed exclusive lock on `relations/.mutation.lock`. Lock acquisition retries for at most five seconds and then returns a timed-out storage I/O error rather than waiting without bound. The persistent lock file itself is not authoritative relation data and is ignored by graph enumeration. On Unix it is created with mode `0600`; on Windows it inherits the store directory ACL. OS file locks are released when the holding file descriptor/process exits, so a crashed writer does not leave a stale logical lockfile that must be manually deleted.

## Atomic successor publication

`FileStore::insert_successor` publishes a new `KnowledgeRecord` and the `Supersedes` relation from that new record to an existing predecessor as one authoritative file under `successors-v1/`. The relation subject must equal the new record ID, the record and relation must use the same `source`, and that source must have both `write_records` and `write_relations`. Record IDs and relation IDs share their normal global namespaces: an ID already present in standalone storage cannot be reused by an atomic successor and vice versa.

The combined file has an explicit schema version tied to the successor record shape: addressless successors use `schema_version = 1`, while successors carrying paired `scope + key` addressing use `schema_version = 2`. Publication uses one synchronized temporary file plus one same-directory hard link. Before the final link, neither the successor record nor its supersession assertion is authoritative; after it, both are. An interrupted temporary file is ignored. Unsupported or malformed successor schema fails closed. The nested record still obeys the existing 1 MiB record bound, the nested relation still obeys the 16 KiB relation bound, and the complete combined file is separately bounded.

A root OS-backed `.state.lock` gives multi-file readers a consistent visibility boundary. Ordinary authoritative record writes, relation writes, and atomic-successor writes take it exclusively. `query`, `status`, and index rebuild take it shared while they combine record/index and relation state. Exact `get` remains a raw read: because an atomic successor itself has one publication point, it observes either no combined file or the complete combined object. Lock acquisition is bounded to five seconds and locks are released by the OS when their file descriptor/process exits.

This fixes the earlier two-step replacement window where a client had to add a new record and then separately assert `new supersedes old`; between those calls both records could appear current. It does not provide a general multi-record transaction or atomically supersede several predecessors at once.

## Supersession cycles

A new supersession assertion is rejected if it would create a directed cycle. The relation mutation lock covers the complete graph load, cycle check, and final publication, so concurrent writers cannot each validate against the same pre-write graph and then jointly create a cycle. For example, simultaneous `A supersedes B` and `B supersedes A` attempts are serialized: one may publish, while the other reloads the now-current graph and is rejected with `SupersessionCycle`.

The store also checks every loaded graph for an already-persisted cycle. This remains a corruption/direct-tampering defense for data that bypassed normal insertion. A stored cycle fails closed with `SupersessionCycle`; Synapse does not attempt to choose a winner from cyclic replacement claims.

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

Publish a new record and its supersession of one existing record atomically with:

```text
synapse knowledge replace <new-id> <kind> <source> <observed|inferred> <unknown|low|medium|high> <content> <relation-id> <old-id>
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
