# Durable Lexical Retrieval Index

## Purpose

Task 5 replaces Task 4's 256-record full-record scan as the normal discovery path. Authoritative records come from validated standalone files under `records/` and, for atomic replacements, validated combined files under `successors-v1/`; the index is derived data used only to find a bounded candidate set efficiently enough for unrelated local tools to reuse knowledge without knowing record IDs.

Task 5 itself does not change `KnowledgeRecord`, lexical matching, lifecycle semantics, or provenance. Task 6 subsequently adds append-only relation-derived effective state while keeping index-v1 as candidate-only derived data. The index does not introduce embeddings, semantic ranking, a model dependency, or a database.

## Layout

The current derived index lives beside the record store:

```text
<store-root>/
├── records/
│   └── <hex-record-id>.json
├── successors-v1/
│   └── <hex-new-record-id>.json
└── index-v1/
    ├── READY
    └── <sha256-of-serialized-record>.idx
```

`READY` is a versioned completeness marker. When it is absent, Synapse treats the store as a legacy/pre-index store and retains Task 4's bounded fallback behavior. The SHA-256 filename identifies the exact bytes of the authoritative persisted file that supplies the record; it is not a signature, trust proof, or authentication mechanism. This remains true for legacy flat v0.1 JSON, addressless standalone `schema_version = 1` envelopes, addressed standalone `schema_version = 2` envelopes, and the corresponding atomic-successor envelopes.

## Index entry contents

Each compact binary entry contains:

- the record ID;
- stable hashes of exact `kind` and `source` values;
- encoded stored lifecycle state and observed/inferred provenance basis;
- a fixed 2048-bit Bloom filter over lowercased searchable fields.

The Bloom filter indexes one-, two-, and three-byte windows from the lowercased record ID, content, kind, source, optional `scope`, optional `key`, and optional provenance detail. Text and address queries use the same lowercase transformation for candidate rejection before any full record is opened. Final `scope` / `key` equality remains case-sensitive against the reopened authoritative record, so the Bloom filter cannot silently change address semantics.

Bloom filters and metadata hashes are only candidate filters. Collisions and false positives are allowed. They cannot directly produce knowledge output because every candidate ID is reopened through `FileStore::get` and then evaluated by the full lexical and metadata predicates. Since Task 6, lifecycle `state` is checked only after relation-derived effective-state resolution; the state byte retained in index-v1 is not used as an authoritative state prefilter. This final validation preserves records plus append-only relation evidence as the source of truth.

## Publication ordering

For stores with a ready index, insertion first rejects an already-existing final record path before doing derived work. A genuinely new candidate then publishes and synchronizes the complete index entry before publishing the final authoritative record path. Therefore a successfully visible new record has already had its candidate entry prepared, while ordinary sequential duplicate attempts create no new index files. If a process crashes after publishing an index entry but before publishing its record, later queries may still encounter a stale candidate; the candidate is ignored because the authoritative record path does not exist.

The preflight does not replace atomic publication. Concurrent writers can both observe an unused ID and prepare different content-addressed entries before one final record hard link wins. A loser that receives `AlreadyExists` reloads the winning authoritative record, ensures its correct entry exists, and removes the losing entry when the serialized values differ. If both writers attempted the same serialized value, the shared content-addressed entry is retained. This preserves index-before-record crash behavior without allowing rejected duplicate writes to accumulate durable index capacity.

Existing index-entry files are byte-verified before they are reused. Corrupt index entries or an invalid readiness marker fail closed with an index error rather than being silently accepted.

## Legacy stores and rebuild

A pre-Task-5 store has record files but no `index-v1/READY` marker. Such a store remains readable by exact ID. Discovery keeps the Task 4 fallback scan while the store has at most 256 records. Above that boundary, discovery returns an explicit index-rebuild-required error rather than scanning an arbitrary subset.

The maintenance command is:

```text
synapse knowledge index rebuild
```

`FileStore::rebuild_index` enumerates standalone and atomic-successor record sources, reads each authoritative record through normal validation while retaining the exact bytes that supplied it, publishes the derived entry using those bytes for content-addressed identity, and writes `READY` only after the bounded rebuild completes. This permits one store to contain legacy flat records, addressless schema-v1 records, addressed schema-v2 records, and atomic-successor envelopes without rewriting any authoritative form. The public rebuild holds the shared store-state lock so it cannot straddle an authoritative mutation. An interrupted rebuild leaves the marker absent, so readers continue treating the index as incomplete.

A normal insert or atomic-successor insert also ensures that an index exists. For an atomic successor, its derived entry is prepared from the exact combined successor-file bytes before the single authoritative successor publication point. A crash before that final hard link can therefore leave only a harmless stale candidate, which cannot become query output because the authoritative successor file is absent. If the store is legacy, insertion performs the same bounded rebuild before publication.

## Bounds

Task 5 keeps explicit work limits:

- legacy discovery fallback: at most 256 full record files;
- query candidate records after index filtering: at most 256 unique IDs;
- query result output: default 5, hard maximum 10 full records;
- query text, `scope`, and `key` filters: each at most 256 UTF-8 bytes before candidate filtering;
- rebuild: at most 16,384 authoritative records;
- indexed query enumeration: at most 32,768 compact index-entry files;
- each authoritative record: existing 1 MiB limit;
- each compact index entry: at most 512 bytes.

A broad lexical query can still exceed the 256-candidate bound, especially when many large records share the same short text. Synapse then asks the caller to narrow the query instead of opening an unbounded number of full records.

These are v0.1 safety boundaries, not claims of final scale. If real workloads approach the index-entry or rebuild ceilings, the next design should introduce sharding or another bounded retrieval structure rather than merely removing limits.

## Security and trust boundary

The index is not authoritative and does not weaken record validation. It contains derived searchable fingerprints and identifiers, so it should receive the same local filesystem protection as the record store. On Unix, newly created index directories and temporary files use the same private-mode helpers as record storage; Windows inherits local filesystem ACL policy.

Task 7 now authorizes authoritative record/relation ingestion before any index publication. Index rebuild itself remains available without a writer grant because the index is derived exclusively from validated authoritative data and cannot create knowledge. Task 7 still does not add automatic secret detection; clients must not intentionally submit passwords, tokens, private keys, cookies, or other credentials as knowledge.

## Deferred work

Task 5 does not add relevance scoring, pagination, fuzzy search, semantic retrieval, embeddings, deletion, authenticated IPC, distributed synchronization, or online index compaction. Task 6 adds append-only lifecycle history and Task 7 adds write authorization outside the index; the index itself remains intentionally derived, local, bounded, and model-agnostic.
