# Contributing to Synapse

Thanks for helping improve Synapse. The project is intentionally focused: useful knowledge discovered by one local tool should be safely reusable by another unrelated local tool without requiring a shared model, framework, or hosted memory service.

## Before you start

Read the [architecture overview](docs/architecture/overview.md) and the documentation for the subsystem you plan to change. The current source, tests, and repository documentation are the project source of truth.

For substantial changes, open an issue first when that would help establish the user problem and the smallest coherent design. New model providers, agent frameworks, MCP adapters, semantic indexes, GUIs, or distributed systems should remain optional unless the core local knowledge-sharing problem requires them.

## Development setup

Synapse requires Rust 1.82 or newer.

```bash
git clone https://github.com/Jervis-UMTC/synapse.git
cd synapse
cargo build --workspace --locked
```

Run the release gates before submitting a pull request:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

CI runs tests on Linux, macOS, and Windows and separately checks the declared Rust 1.82 minimum version.

## Change guidelines

- Keep changes focused and preserve existing boundaries unless there is evidence they should change.
- Add or update tests for behavioral changes, including failure and recovery paths where relevant.
- Update durable documentation when public behavior, storage formats, security boundaries, or operational procedures change.
- Preserve provenance and uncertainty; do not turn inferred or conflicted information into silent truth.
- Keep I/O bounded and fail closed on corrupt or ambiguous authoritative state.
- Do not add passwords, API keys, tokens, private keys, cookies, private datasets, or other secrets to tests, fixtures, logs, issues, or commits.
- Avoid unrelated refactors in the same pull request.

## Pull requests

A good pull request explains the user or interoperability problem, the chosen design, important security or compatibility effects, and the verification that ran. The PR template contains the expected checklist.

Do not rewrite public history to tidy a pull request. Maintainers may squash or otherwise choose the merge strategy during review.

## Reporting security issues

Do not report exploitable security vulnerabilities in a public issue. Follow [SECURITY.md](SECURITY.md) instead.

## Documentation

Durable project documentation belongs under `docs/`, except conventional root project files such as this guide, `README.md`, `SECURITY.md`, and `CHANGELOG.md`.
