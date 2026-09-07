# Changelog

All notable user-visible changes to Synapse will be documented in this file. Synapse is currently pre-1.0; the `Unreleased` section describes the development line on `main` until the first tagged release.

## [Unreleased]

### Added

- Local-first Rust workspace with storage-independent knowledge records, provenance, confidence, and lifecycle state.
- Durable write-once file storage with bounded reads and versioned persistence envelopes.
- Stable optional `scope + key` logical knowledge addresses for cross-tool lookup.
- Bounded lexical discovery and a durable derived retrieval index.
- Append-only `supersedes` and `conflicts_with` knowledge evolution.
- Atomic successor-plus-supersession publication.
- Store-local record/relation write authorization.
- Authenticated local IPC using OS peer process information and trusted executable fingerprints.
- Stable store identity for IPC endpoint naming.
- `synapse doctor` bounded structural store inspection.
- Cross-platform CI for Linux, macOS, and Windows plus a Rust 1.82 MSRV check.

### Security

- Authoritative writes fail closed on invalid authorization, ambiguous executable trust, corrupt persistence, relation cycles, and bounded-capacity violations.
- Direct filesystem access remains an administrative/trusted path and is not yet isolated by a dedicated service account or enforced store ACLs.

### Fixed

- Normalize OS-reported IPC peer process IDs with checked conversion so authenticated IPC compiles safely on Unix as well as Windows.
