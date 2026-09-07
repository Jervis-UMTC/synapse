# Security Policy

Synapse handles durable local machine knowledge, so security boundaries and failure behavior are part of the product rather than optional hardening.

## Supported versions

Synapse is currently pre-1.0. Security fixes are developed on the `main` branch until versioned releases establish a broader support policy.

| Version | Supported |
| --- | --- |
| `main` / current 0.1 development line | Yes |
| Older snapshots and unmaintained forks | No guaranteed support |

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting / security advisory flow for this repository when available. Do not open a public issue containing exploit details, credentials, private machine data, or a proof of concept that would expose users before a fix is available.

If private vulnerability reporting is unavailable, contact the repository maintainer through GitHub first and wait for a private disclosure channel before sending sensitive details.

A useful report includes:

- the affected commit or version;
- operating system and relevant filesystem/IPC environment;
- the security boundary you expected;
- reproduction steps that do not include real credentials or unrelated private data;
- impact and any known mitigations.

## Current security boundary

Authenticated local IPC binds authoritative writes to an OS-reported peer process and a trusted executable fingerprint, then applies store-local capabilities. This is not a complete hostile-process sandbox. If an untrusted process can directly modify the Synapse store, authorization policy, or executable-trust files, it can bypass the IPC policy boundary.

A hardened deployment still requires operating-system ownership and ACLs that make the Synapse service authoritative over its private files and socket or named pipe. Reader authorization, service installation, process-image attestation, automatic secret filtering, and stronger OS isolation remain future work. See [docs/architecture/ipc.md](docs/architecture/ipc.md) and [docs/architecture/authorization.md](docs/architecture/authorization.md) for the exact current claims.

## Secrets

Synapse is not a secret store. Do not intentionally submit passwords, tokens, cookies, private keys, recovery codes, or similar credentials as knowledge records. Current storage does not automatically detect and redact secrets from caller-provided content.
