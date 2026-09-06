# Bounded Knowledge Retrieval

## Purpose

Task 4 adds the first discovery path for persisted Synapse knowledge. A local client no longer needs to know an exact record ID: it can search a bounded local store by text and metadata and receive validated `KnowledgeRecord` values.

This is deliberately lexical retrieval. It does not add embeddings, a vector database, a model dependency, fuzzy matching, or a relevance index.

## Query contract

`synapse-store` exposes `KnowledgeQuery` with these fields:

- `text`: optional case-normalized substring text.
- `kind`: optional exact `kind` filter.
- `source`: optional exact `source` filter.
- `state`: optional lifecycle-state filter. The default is `active`.
- `provenance_basis`: optional exact `observed` / `inferred` filter.
- `limit`: maximum returned records. The default is 5 and the hard maximum is 10.

Text matching checks record ID, content, kind, source, and optional provenance detail. Text is normalized with Rust lowercase conversion before substring comparison. This is simple lexical matching, not tokenization or linguistic case folding.

Exact metadata filters remain case-sensitive because those values are caller-defined identifiers/categories rather than free-text search terms.

## Currentness and ordering

Task 6 makes the query `state` predicate an **effective-state** filter. Queries still default to `active`, but a stored-active record is excluded when append-only evolution relations make it effectively superseded or conflicted. Explicit `--state superseded`, `--state conflicted`, and `--state any` queries can retrieve those historical branches.

`FileStore::query` returns `KnowledgeHit`: the immutable stored record fields plus `effective_state`. The original `state` remains visible so callers can distinguish what was stored from what later lifecycle evidence resolved.

Matching hits are returned newest first by the immutable record's `created_at_unix_ms`, with record ID as a deterministic ascending tie-breaker. This ordering is recency, not semantic relevance. Because the timestamp remains caller supplied at the domain boundary, clients that provide inaccurate timestamps can affect ordering; Synapse does not silently rewrite provenance or time metadata during retrieval.

## Bounds and failure behavior

Retrieval is intentionally bounded. Task 5 now uses a compact durable index for normal discovery: index filtering may produce at most 256 unique candidate IDs, and at most 10 validated full records can be returned. Query text remains limited to 256 UTF-8 bytes and authoritative records retain the existing 1 MiB bound.

Pre-index stores keep the Task 4 compatibility path. They may scan at most 256 authoritative record files; a larger legacy store returns an explicit index-rebuild-required error instead of an arbitrary partial result. `synapse knowledge index rebuild` upgrades that derived retrieval state without rewriting authoritative records.

Indexed candidates are only hints. Every candidate is loaded through the same fail-closed `FileStore::get` path used by exact lookup, its relation-derived effective state is resolved, and it is then checked against the complete lexical and metadata predicates. Corrupt index or evolution data fails closed, while Bloom-filter/hash collisions can only create false-positive candidates that are removed by final validation.

The index-v1 stored-state byte is retained for format compatibility but is no longer used to prefilter the `state` query because effective state can differ after Task 6 relations. See [`index.md`](index.md), [`evolution.md`](evolution.md), and the bounded relation graph rules documented there.

## CLI

The current discovery command is:

```text
synapse knowledge find <text> [--kind <kind>] [--source <source>] [--state <active|stale|conflicted|superseded|unknown|any>] [--basis <observed|inferred|any>] [--limit <1..10>]
```

The command uses the same local data-directory behavior as `knowledge add` and `knowledge show`, including the `SYNAPSE_STORE` override. It emits a JSON array of `KnowledgeHit` objects: complete validated record fields plus `effective_state`, so an unrelated local process can consume currentness without scraping human-oriented output. Task 7 authorization applies only to authoritative writes; discovery remains readable without a writer grant.

`--state any` removes the default active-state filter. `--basis any` removes the provenance-basis filter.

## Deferred work

Task 4 itself did not define indexing; Task 5 adds the bounded derived index, Task 6 adds append-only supersession/conflict resolution, and Task 7 adds cooperative authorization for authoritative writes without changing read semantics. Relevance scoring, pagination, tokenization, fuzzy matching, semantic retrieval, embeddings, relation retraction, strong writer authentication, automatic secret detection, authenticated daemon IPC, and distributed synchronization remain deferred.

A future retrieval design should preserve the same core properties: bounded work and output, current-state awareness, provenance, explicit uncertainty/conflict, deterministic behavior where possible, and no dependency on a particular agent framework or model provider.
