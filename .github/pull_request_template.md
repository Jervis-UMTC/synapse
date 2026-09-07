## What problem does this solve?

Describe the concrete Synapse user or cross-tool interoperability problem.

## What changed?

Summarize the smallest coherent design and implementation.

## Verification

List the checks and runtime/integration evidence you ran.

## Checklist

- [ ] I kept the change focused and avoided unrelated refactors.
- [ ] I added or updated relevant tests.
- [ ] `cargo fmt --all --check` passes.
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` passes.
- [ ] `cargo test --workspace --locked` passes.
- [ ] I updated durable documentation for changed public behavior, storage, security, or operations.
- [ ] I did not add secrets, private machine data, or unrelated personal information.
- [ ] I considered backward compatibility, resource bounds, provenance, and failure behavior where relevant.
