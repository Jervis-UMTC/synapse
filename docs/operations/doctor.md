# Store diagnostics (`synapse doctor`)

`synapse doctor` provides a bounded structural inspection of the currently selected local store. It is intended for users and maintainers to distinguish an uninitialized store from corrupt or inconsistent authoritative state without running a repair operation.

The command uses the same `SYNAPSE_STORE` selection and per-user defaults as other CLI commands.

```text
synapse doctor
```

A healthy configured store reports the selected root, authorization client count, authoritative record/relation counts, derived-index status, durable store identity when one already exists, and an overall successful structural result.

## What is validated

For an existing store, inspection uses the store state lock to keep authoritative record/relation mutations and index rebuild from racing the bounded structural snapshot, then:

- enumerates standalone and atomic-successor records up to the same 16,384-record index capacity;
- reopens and validates every authoritative record through its normal persistence decoder;
- loads the complete bounded relation graph, revalidates endpoints, ID uniqueness, relation limits, and supersession-cycle invariants;
- reads and validates the authorization policy when configured;
- reads and validates an existing store identity without creating one;
- validates the index readiness marker when present;
- when the index is ready, decodes every bounded index entry and verifies that each authoritative record has the exact content-addressed derived entry expected from its stored bytes.

A malformed record, relation, identity, authorization policy, readiness marker, or index entry makes `doctor` fail instead of reporting a partially healthy store.

## Informational states

These are not treated as corruption:

- the selected store root does not exist yet;
- authorization has not been initialized (reads can still be meaningful, but authoritative writes will be denied);
- no durable store identity exists yet (identity is created when IPC endpoint identity is first required);
- the derived index is not built yet (small legacy stores can still use bounded fallback discovery; larger legacy stores require `synapse knowledge index rebuild`).

Inspecting a missing store does not create it. Inspecting an existing store may create/use the `.state.lock` coordination file so concurrent authoritative mutations cannot produce a torn diagnostic snapshot; it does not create a store identity, rebuild the index, rewrite records, add relations, or change authorization.

## What `doctor` does not do

`doctor` does not repair corruption, delete stale derived index entries, change filesystem ACLs, start the IPC service, authenticate readers, scan record content for secrets, or prove that an executable currently running in memory matches its trusted on-disk image. Those are separate operational or security boundaries.

If `doctor` reports index corruption and authoritative records are otherwise known to be intact, `synapse knowledge index rebuild` is the explicit derived-data repair path. Do not modify authoritative record or relation files manually as a repair mechanism.
