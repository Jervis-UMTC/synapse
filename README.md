# Synapse

**A local-first knowledge layer that lets unrelated tools share what they learn about the same machine.**

Synapse gives local CLIs, agents, scripts, IDE tools, and other programs a small common place to publish and retrieve durable machine knowledge without requiring them to share a model, prompt history, agent framework, or hosted memory service.

Synapse is an early-stage Rust project (`v0.1`) with a working knowledge model, durable local storage, stable logical knowledge addresses, bounded retrieval, append-only knowledge evolution, write authorization, and authenticated local IPC.

## Why Synapse?

Local tools repeatedly rediscover the same facts:

- where a compiler or SDK is installed;
- which service owns a port;
- which configuration was selected and why;
- which earlier fact has become stale or was superseded;
- what another tool already observed about the machine.

Most tools keep that information inside their own logs, caches, conversations, or framework-specific memory. The next tool starts from zero.

Synapse treats useful machine knowledge as a shared local primitive instead:

```text
Tool A discovers something
        │
        ▼
  KnowledgeRecord
  + provenance
  + confidence
  + lifecycle state
        │
        ▼
      Synapse
        │
        ├────────► Tool B retrieves it later
        ├────────► Tool C can supersede it without deleting history
        └────────► Tool D can read it without becoming a writer
```

The goal is not to collect every action a tool performs. Synapse favors **small, durable, attributable knowledge** over unbounded transcripts and activity logs.

## What works today

| Capability | Status | Notes |
| --- | --- | --- |
| Storage-independent knowledge model | ✅ | Records preserve source, confidence, state, and observed/inferred provenance. |
| Durable local persistence | ✅ | Write-once validated JSON records with path-safe IDs and bounded reads. |
| Cross-process reuse | ✅ | One process can persist knowledge and another can retrieve it later. |
| Stable logical addressing | ✅ | Optional paired `scope + key` lets unrelated tools name the same fact without sharing prose or record IDs. |
| Lexical discovery | ✅ | Bounded case-insensitive retrieval with metadata filters and deterministic ordering. |
| Durable retrieval index | ✅ | Derived bounded index accelerates discovery while authoritative records remain the source of truth. |
| Knowledge evolution | ✅ | Append-only `supersedes` and `conflicts_with` relations preserve history. |
| Atomic replacement | ✅ | A successor record and its `supersedes` assertion can publish as one crash-safe authoritative commit. |
| Local write authorization | ✅ | Separate capabilities for record writes and lifecycle-relation writes. |
| Authenticated local IPC | ✅ | Local-socket writes bind `source` to an OS-reported peer process and trusted executable fingerprint. |
| Semantic/vector retrieval | Not yet | Intentionally deferred until it is justified by the core use case. |
| MCP/provider/agent adapters | Not yet | Optional clients should sit on top of generic local interfaces, not define the core. |
| Service installation and hardened OS isolation | Not yet | IPC exists; production-style service ownership/ACL setup is still future work. |

## Design principles

**Local first.** Synapse should remain useful on one computer without a hosted control plane.

**Tool and model agnostic.** Coders, agents, shell scripts, MCP servers, editors, and future integrations are clients of Synapse—not owners of its architecture.

**Provenance over convenient certainty.** Observed and inferred knowledge stay distinct. Stale, conflicted, superseded, uncertain, and unknown information is represented explicitly instead of silently becoming “truth.”

**History over destructive mutation.** New knowledge can supersede or conflict with older knowledge without erasing the earlier evidence.

**Bounded and fail-closed.** Record sizes, query results, scans, relation graphs, indexes, IPC frames, trust entries, and authorization policy are bounded. Invalid or ambiguous security state rejects authoritative writes.

**No secret hoarding.** Synapse is for useful machine knowledge, not passwords, tokens, cookies, private keys, or arbitrary conversation capture.

## Architecture

```text
                        ┌─────────────────────┐
                        │     local tools     │
                        │ CLI / agent / script│
                        └──────────┬──────────┘
                                   │
                          local IPC│or trusted admin path
                                   ▼
┌─────────────────────────────────────────────────────────────┐
│                         synapse-ipc                         │
│ peer PID → executable fingerprint → client identity        │
└──────────────────────────────┬──────────────────────────────┘
                               │
                               ▼
┌─────────────────────────────────────────────────────────────┐
│                        synapse-store                        │
│ authorization · persistence · retrieval index · evolution  │
└──────────────────────────────┬──────────────────────────────┘
                               │
                               ▼
┌─────────────────────────────────────────────────────────────┐
│                         synapse-core                        │
│ KnowledgeRecord · KnowledgeRelation · provenance · state   │
└─────────────────────────────────────────────────────────────┘
```

The core domain model does not know about a database, agent framework, embedding model, IPC mechanism, or identity provider. Storage and transport are deliberately separate layers.

## Quick start

Synapse currently targets **Rust 1.82+**. The quickest way to try it is to clone the repository, install the CLI locally, and use a disposable store.

### 1. Build and install the CLI

```bash
git clone https://github.com/Jervis-UMTC/synapse.git
cd synapse
cargo install --path crates/synapse-cli
synapse --version
```

If you do not want to install the binary, replace `synapse` in the examples below with `cargo run -p synapse-cli --`.

### 2. Use an isolated store

For a first run, point Synapse at a directory inside the checkout so the experiment is easy to inspect or remove.

**Linux/macOS:**

```bash
export SYNAPSE_STORE="$PWD/.synapse-demo"
```

**PowerShell:**

```powershell
$env:SYNAPSE_STORE = "$PWD/.synapse-demo"
```

Without `SYNAPSE_STORE`, Synapse uses the normal per-user data directory: `%LOCALAPPDATA%\\Synapse` on Windows, `$HOME/Library/Application Support/Synapse` on macOS, and `$XDG_DATA_HOME/synapse` or `$HOME/.local/share/synapse` on other Unix systems.

### 3. Bootstrap a writer

A new store needs an authorized local writer before it accepts authoritative records. This command initializes the store policy and grants `inspector` permission to write records and lifecycle relations:

```bash
synapse authorization init inspector
```

Use the same client ID as the `source` in the write commands that follow.

### 4. Publish useful knowledge

Store an observation with both an immutable record ID and a stable logical address:

```bash
synapse knowledge add rust-toolchain fact inspector observed high "Rust is installed" --scope machine --key toolchain.rust.compiler_path
```

The record ID identifies this historical assertion. The optional `scope + key` pair identifies what the assertion is about, so another unrelated tool can look up the same fact without knowing the record ID or matching the prose.

### 5. Read it from another process

Read by immutable ID:

```bash
synapse knowledge show rust-toolchain
```

Or discover the current fact by logical address:

```bash
synapse knowledge find --scope machine --key toolchain.rust.compiler_path
```

Free-text discovery is also available:

```bash
synapse knowledge find "Rust"
```

`scope + key` is not a uniqueness constraint. If several active records share an address, Synapse returns the matching records instead of silently choosing a winner.

### 6. Replace a fact without deleting history

When the fact changes, publish the successor and its `supersedes` relation atomically:

```bash
synapse knowledge replace rust-toolchain-v2 fact inspector observed high "Rust moved to a new toolchain path" rust-toolchain-moved rust-toolchain --scope machine --key toolchain.rust.compiler_path
```

Normal current discovery now returns the successor. The old record remains readable as history, and its resolved lifecycle state can be inspected with:

```bash
synapse knowledge status rust-toolchain
```

### Common commands

| Command | Purpose |
| --- | --- |
| `synapse authorization init <client-id>` | Initialize one store and bootstrap its first writer. |
| `synapse knowledge add ...` | Persist a new immutable knowledge record. |
| `synapse knowledge find [<text>] [filters]` | Discover current knowledge by text or metadata such as `scope` and `key`. |
| `synapse knowledge show <id>` | Read one immutable record exactly by ID. |
| `synapse knowledge status <id>` | Read a record plus its effective lifecycle state and relation evidence. |
| `synapse knowledge replace ...` | Atomically publish a successor and supersede one older record. |
| `synapse knowledge relate ...` | Append an explicit `supersedes` or `conflicts` relation. |
| `synapse knowledge index rebuild` | Rebuild the bounded derived retrieval index. |
| `synapse ipc serve` | Run the authenticated local IPC service. |
| `synapse ipc trust <client-id> <executable>` | Trust one executable fingerprint for IPC writes as a client. |
| `synapse ipc ping` | Check that the local IPC service is reachable. |

Run `synapse` with no arguments to print the complete command grammar.

### What gets stored

A new addressed standalone record is persisted in an explicit versioned JSON envelope, for example:

```json
{
  "schema_version": 2,
  "record": {
    "id": "rust-toolchain",
    "content": "Rust is installed",
    "kind": "fact",
    "source": "inspector",
    "scope": "machine",
    "key": "toolchain.rust.compiler_path",
    "created_at_unix_ms": 0,
    "confidence": "high",
    "state": "active",
    "provenance": {
      "basis": "observed",
      "detail": null
    }
  }
}
```

Legacy flat v0.1 record JSON remains readable. Addressless records retain schema version 1; records with paired `scope + key` addressing use schema version 2 so older formats cannot silently discard the address. `created_at_unix_ms` is supplied at runtime; `0` above is only a shortened documentation example.

## Authenticated local IPC

Direct `FileStore` access is an administrative/trusted path. For ordinary writers, Synapse can put a local process boundary in front of the store.

Build the CLI, authorize a client ID, and trust the executable that will write as that client:

```bash
cargo build -p synapse-cli
cargo run -p synapse-cli -- ipc trust inspector target/debug/synapse
cargo run -p synapse-cli -- ipc serve
```

On Windows, use `target/debug/synapse.exe` for the trusted executable path.

Clients can then opt into IPC routing:

**Linux/macOS:**

```bash
SYNAPSE_IPC=1 cargo run -p synapse-cli -- knowledge find "Rust"
```

**PowerShell:**

```powershell
$env:SYNAPSE_IPC = "1"
cargo run -p synapse-cli -- knowledge find "Rust"
```

For authoritative IPC writes, the service obtains the peer process ID from the operating system, resolves and fingerprints the executable, maps that fingerprint to exactly one configured Synapse client ID, requires the record/relation `source` to match it, and then applies the store's capability policy.

### Security boundary

Authenticated IPC materially improves the earlier claimed-string authorization model, but it is **not a complete same-machine sandbox**. If an untrusted process can directly modify the Synapse store, authorization policy, or executable-trust files, it can bypass the service. A hardened deployment still needs OS ownership/ACLs that make the service the authority over those files.

Executable fingerprints authenticate a trusted binary identity, not a human identity or in-memory process attestation. See [`docs/architecture/ipc.md`](docs/architecture/ipc.md) for the exact security claim and remaining limitations.

## Knowledge evolution

Synapse does not overwrite history to make old knowledge disappear. Instead, clients can append relationships between immutable records.

```bash
cargo run -p synapse-cli -- knowledge relate \
  compiler-update \
  compiler-new \
  supersedes \
  compiler-old \
  inspector \
  observed
```

Normal discovery uses the relation-derived `effective_state`, while `knowledge show` still exposes the original immutable record and `knowledge status` exposes the relation evidence.

This lets a future tool answer both:

- “What is current?”
- “How did we get here?”

## Repository layout

```text
crates/
├── synapse-core/   # storage/transport-independent domain model
├── synapse-store/  # durable file store, retrieval, index, evolution, auth
├── synapse-ipc/    # authenticated local-socket boundary
└── synapse-cli/    # ordinary local client and integration proof surface

docs/architecture/
├── overview.md
├── knowledge-model.md
├── storage.md
├── retrieval.md
├── index.md
├── evolution.md
├── authorization.md
└── ipc.md
```

## Documentation

Architecture and operational contracts are documented under `docs/` and kept in sync with the implementation.

- [Architecture overview](docs/architecture/overview.md)
- [Knowledge model](docs/architecture/knowledge-model.md)
- [Durable storage](docs/architecture/storage.md)
- [Bounded retrieval](docs/architecture/retrieval.md)
- [Durable retrieval index](docs/architecture/index.md)
- [Knowledge evolution](docs/architecture/evolution.md)
- [Local write authorization](docs/architecture/authorization.md)
- [Authenticated local IPC](docs/architecture/ipc.md)

## Development

Run the full repository gates before submitting a change:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The workspace forbids unsafe Rust and treats Clippy's `all` lint group as warnings; CI-equivalent verification promotes warnings to errors with `-D warnings`.

## Contributing

Synapse is intentionally small and early enough for contributors to shape it. Focused issues and pull requests are welcome, especially around:

- cross-platform local IPC behavior and hardening;
- storage correctness and recovery testing;
- retrieval quality without forcing a model dependency into the core;
- safe client/provider adapters built on generic interfaces;
- documentation, threat modeling, and reproducible integration tests.

Before introducing a new subsystem, read the [architecture overview](docs/architecture/overview.md) and ask whether an existing boundary already solves the problem. New model providers, agent frameworks, MCP integrations, GUIs, semantic indexes, or distributed systems should remain optional unless the core product need clearly requires them.

## What Synapse is not

Synapse is **not** trying to be:

- a hosted “memory API”;
- a replacement for every agent framework;
- an unbounded conversation or tool-call logger;
- a vector database wrapper with a local-agent brand;
- a model-specific plugin disguised as infrastructure.

The core idea is narrower: **useful knowledge discovered locally should be safely reusable locally, even when the next tool is unrelated to the first one.**

## Project status

Synapse is pre-1.0 and under active development. The current implementation is suitable for experimentation and architecture validation, not yet for treating hostile local processes as fully isolated tenants. Service installation, hardened OS filesystem ownership, reader ACLs, relation retraction, semantic ranking, distributed synchronization, and higher-level integrations remain open work.

The roadmap is deliberately evidence-driven: optional technologies are added when they improve the core cross-tool knowledge-sharing problem, not simply because they are popular in agent infrastructure.

## License

Synapse is available under the [MIT License](LICENSE).
