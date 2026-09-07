# Local Write Authorization

## Purpose

Task 7 adds the first explicit least-privilege boundary for authoritative Synapse writes. A store-local `AuthorizationPolicy` decides which claimed local client identities may persist knowledge records and which may assert lifecycle relations.

The boundary is deliberately independent of agents, models, MCP, hosted identity providers, and network services. It protects cooperative local clients from accidentally exercising permissions they were not granted while preserving Task 1-6 local-first behavior.

## Identity model

For authoritative writes, `synapse-store` treats the existing `source` field as the claimed local client identity:

- `KnowledgeRecord.source` is checked for `write_records` before record persistence;
- `KnowledgeRelation.source` is checked for `write_relations` before relation persistence.

The same value is therefore both the identity used for authorization and durable evidence of which client claimed responsibility for the write. `synapse-core` remains policy-independent; this interpretation is imposed only at the local persistence boundary.

External evidence origin should not be impersonated through `source`. A writer can preserve additional evidence context in provenance detail while keeping `source` equal to its own client identity.

## Policy file

The authoritative local policy is:

```text
<store-root>/authorization-v1.json
```

Its current JSON shape is:

```json
{
  "version": 1,
  "clients": [
    {
      "id": "machine-inspector",
      "write_records": true,
      "write_relations": false
    },
    {
      "id": "knowledge-reviewer",
      "write_records": false,
      "write_relations": true
    }
  ]
}
```

Task 7 bounds and validates policy data:

- exact policy version: `1`;
- policy file: at most 64 KiB;
- clients: at least 1 and at most 64;
- client ID: non-whitespace, at most 100 UTF-8 bytes;
- client IDs: unique within one policy;
- every client must have at least one write capability.

Unknown versions, malformed JSON, duplicates, empty identities, useless zero-capability entries, and oversized policies fail closed for authoritative writes.

## Capabilities

Task 7 intentionally exposes only two capabilities:

- `write_records`: persist a new immutable `KnowledgeRecord`;
- `write_relations`: persist a new append-only `KnowledgeRelation`.

These permissions are separate because lifecycle assertions can change what unrelated tools consider current knowledge even though they do not rewrite historical records. A machine observation tool can therefore be allowed to contribute records without automatically receiving permission to supersede or conflict with another tool's knowledge.

Atomic successor publication intentionally requires **both** capabilities. The combined record and relation must use the same `source`; authorization is checked for `write_records` and `write_relations` before the combined authoritative file or its derived index entry is published. A record-only client therefore cannot bypass lifecycle authority by using the atomic replacement API.

Authorization is checked before index preparation, endpoint validation, or authoritative publication. A denied record write must not leave a record or derived index entry behind. A denied relation write must not publish relation data.

## Bootstrap and administration

A new or legacy store has no implicit writer grant. Until authorization is configured, reads remain available but authoritative record and relation writes return an explicit authorization-not-configured error.

The bootstrap command is:

```text
synapse authorization init <client-id>
```

It creates `authorization-v1.json` exactly once through the same private temporary-file, sync, and hard-link publication pattern used elsewhere in the file store. The bootstrap client receives both Task 7 capabilities so the store is immediately usable.

Synapse does not yet provide an in-band grant/revoke command. `initialize_authorization` refuses to replace an existing policy. Operators that need multiple clients or split permissions may manage the small JSON policy through their normal local configuration-management mechanism. A malformed externally edited policy denies later authoritative writes rather than falling back to allow-all behavior.

This avoids inventing an insecure mutable admin protocol before Synapse has authenticated local IPC.

## Read behavior and legacy stores

Authorization protects authoritative ingestion, not knowledge consumption. Exact reads, bounded discovery, lifecycle status inspection, and derived index rebuild remain available without consulting the policy.

This means an existing Task 1-6 store can be upgraded without rewriting historical records: old records remain readable, new authoritative writes are denied until `authorization-v1.json` is explicitly configured, and later authorization has no retroactive effect on the provenance of previously stored data.

Derived index rebuild is intentionally outside the capability model because it reconstructs non-authoritative data exclusively from validated authoritative records; it cannot create or change knowledge truth.

## Security boundary

Task 7 by itself remains **authorization without strong authentication**: direct `FileStore` callers present a claimed `source` string. Task 8 adds an optional local IPC boundary that authenticates write requests against an OS-reported peer process and a trusted executable fingerprint before this policy is evaluated. See [`ipc.md`](ipc.md).

Direct storage access is still an administrative/trusted path. A process that can modify the store root can bypass IPC, replace policy/trust/store-identity files, redirect the local endpoint identity, or write authoritative files directly. A hardened deployment must therefore give the service control of those files and restrict ordinary clients to the local endpoint.

The authorization policy contains identifiers and booleans only. Task 8 executable-trust entries add hashes, not passwords, API keys, tokens, cookies, or private-key material.

## Deferred work

Task 7/8 do not add OS service installation, dedicated service-account ownership, dynamic grant/revoke administration, per-reader ACLs, source-to-source trust weighting, automatic secret detection, destructive deletion, distributed authorization, or hosted identity. Those are separate security decisions and must not be implied by the current policy plus executable-trust boundary.
