# Authenticated Local IPC

## Purpose

Task 8 adds the first local process boundary for Synapse. Authoritative record and relation writes can now be routed through a local IPC service instead of giving every client direct write access to the store. The service binds a write request to an operating-system-reported peer process and then applies the existing Task 7 authorization policy.

The design remains local-first and framework-agnostic. It uses local sockets rather than TCP and does not introduce a hosted identity provider, API token, model integration, or agent-specific protocol.

## Transport

`synapse-ipc` uses `interprocess` local sockets with a namespaced endpoint derived from the SHA-256 of the configured store-root path. On the current Windows development target this is a local named pipe. On Unix targets the same abstraction resolves to the platform local-socket implementation.

The protocol is versioned by the current implementation contract and uses one request per connection. Each frame is a four-byte big-endian length followed by JSON. Frames are bounded to 12 MiB. Accepted streams are placed in nonblocking mode and frame reads/writes must make progress within three seconds; a stalled peer therefore cannot hold one service request indefinitely.

Current request operations are:

- `ping`;
- insert a `KnowledgeRecord`;
- insert a `KnowledgeRelation`;
- exact record lookup;
- bounded `KnowledgeQuery` discovery;
- record lifecycle status.

The IPC layer reuses `synapse-core` and `synapse-store` data contracts rather than defining a second knowledge model.

## Peer identity

Authoritative IPC writes require an OS-reported peer process ID. If the local-socket backend cannot provide a process ID, writes fail closed.

For each write the service:

1. obtains the peer PID from the accepted local connection;
2. resolves that process through the operating system;
3. records its process start time and executable path;
4. SHA-256 hashes the executable, with a 512 MiB input bound;
5. refreshes the process and verifies that the PID still has the same start time and executable path;
6. maps the executable fingerprint to exactly one trusted Synapse client ID;
7. requires `KnowledgeRecord.source` or `KnowledgeRelation.source` to equal that authenticated client ID;
8. invokes `synapse-store`, which still enforces Task 7 `write_records` / `write_relations` capability checks.

This removes the Task 7 behavior where an IPC writer could select any permitted `source` string by claim alone. Source identity is still preserved in the authoritative record/relation, but the IPC service now checks that the connected process maps to it.

The start-time/path recheck reduces PID-reuse races. It is not process-image attestation: the implementation hashes the resolved executable file and does not prove the exact in-memory code pages of a running process.

## Executable trust

Trusted executable mappings live under:

```text
<store-root>/ipc-trust-v1/
└── <sha256-of-client-id>.json
```

A trust entry contains only:

- format version;
- Synapse client ID;
- lowercase SHA-256 executable fingerprint.

No passwords, bearer tokens, cookies, API keys, or private keys are introduced.

Bootstrap a trusted executable with:

```text
synapse ipc trust <client-id> <executable-path>
```

The client ID must already exist in `authorization-v1.json` with at least one write capability. Trust entries are write-once, at most 1 KiB each, and the trust directory is bounded to 64 entries. If one executable fingerprint maps to more than one client identity, write authentication fails as ambiguous rather than selecting one.

Rebuilding or replacing a trusted executable changes its fingerprint and therefore requires a new trust configuration. Task 8 intentionally does not invent automatic trust migration.

## CLI mode

Run the local service with:

```text
synapse ipc serve
```

`--once` serves one connection and exits; this exists primarily for deterministic tests and small process orchestration.

Check service reachability with:

```text
synapse ipc ping
```

When `SYNAPSE_IPC=1` is set, these existing CLI operations use the service:

- `knowledge add`;
- `knowledge show`;
- `knowledge find`;
- `knowledge relate`;
- `knowledge status`.

`knowledge create` remains non-persistent. Authorization bootstrap, executable-trust administration, and derived index rebuild remain direct administrative operations.

Reads over IPC do not require a writer identity. This preserves the core product behavior: an unrelated local tool can consume validated knowledge without receiving mutation authority.

## Security boundary

Task 8 provides **OS-bound peer-process authentication at the IPC write boundary**, but it is not a complete same-machine sandbox.

The strongest deployment requires the IPC service to own the store and policy files while ordinary clients can reach the local endpoint but cannot write the store root directly. If an untrusted process can modify `authorization-v1.json`, `ipc-trust-v1`, `records/`, or `relations/`, it can bypass the service entirely. Task 8 does not install an OS service, change directory ownership, or configure a dedicated Windows named-pipe ACL.

Executable fingerprints identify a trusted binary, not a human account or individual process instance. Multiple invocations of the same trusted binary authenticate as the same client ID. This is useful for ordinary tools with distinct executables; the generic `synapse` CLI should be treated as one client identity when used as an IPC writer.

The endpoint is local only, but local read access is intentionally broader than write access. Per-reader ACLs are not part of Task 8.

## Deferred work

Task 8 does not add service installation, dedicated service accounts, Windows SID / Unix UID policy rules, per-reader authorization, secure dynamic trust rotation, grant/revoke audit history, cryptographic process attestation, rate limiting across many simultaneous clients, concurrent request workers, remote networking, or distributed synchronization. Those require explicit security and operations designs rather than being implied by this first local IPC boundary.
