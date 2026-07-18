# ADR-S-KEY-001: Validator key generation is an offline secret-custody boundary

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Consensus and validator operations maintainers
- **Scope:** `bin/outbe-keygen` and its `outbe_consensus::bls` storage seam
- **Depends on:** ADR-S-VAL-001
- **Related:** ADR-S-TEE-001, ADR-B-RPC-001 and validator lifecycle PFS-006

## Context

`outbe-keygen` creates the long-lived individual BLS identity used to attribute
consensus messages, optionally creates an EVM artifact-signing key, derives public
identity, signs ValidatorSet registration proof and verifies that a stored BLS key
can sign. Its output is security authority, not ordinary configuration.

Genesis construction, threshold DKG shares, enclave keys and runtime key loading
are separate owners. This ADR defines only offline individual-key creation,
custody formats and registration proof production.

## Decision

### Artifact identities

Every secret artifact has a typed purpose and metadata envelope: format version,
network/domain, algorithm and variant, public-key fingerprint, creation time,
backend identifier and stable key id. An individual MinPk consensus key and an EVM
secp256k1 signer are distinct artifacts and cannot be substituted for DKG shares,
threshold material, P2P identity or enclave keys.

Key generation uses an operating-system CSPRNG and validates the resulting scalar
with the target cryptographic library before any installation. Public material may
be printed; private bytes and passphrases may not reach stdout, logs, argv or an
environment inherited by unrelated processes.

### Storage and installation

Production defaults to a hardware/OS secret store or an authenticated encrypted
file. Plaintext is an explicit development/recovery opt-in with a prominent
confirmation. File artifacts are created in a private directory, written to a
unique temporary file with mode `0600`, synced, atomically installed without
overwriting an existing key, and followed by directory sync. Unsupported permission
semantics fail closed.

Encrypted envelopes use versioned KDF parameters, random salt and AEAD nonce,
authenticated metadata and memory-zeroized plaintext/derived keys. Passphrases are
read interactively or through a protected descriptor/secret agent. A backend can
rekey/migrate without changing the cryptographic identity.

OS keychain entries use a collision-resistant stable key id, not a filename. The
marker is an authenticated reference containing backend/version/key id/fingerprint;
loading verifies that the returned secret derives the expected fingerprint.

### Commands and transactionality

`generate` creates exactly one BLS artifact. `hybrid` is a two-artifact provisioning
transaction: either both BLS and EVM artifacts are durably installed or neither is.
Existing targets cause a typed refusal unless an explicit, separately confirmed
rotation workflow names the old fingerprint and backup/recovery outcome.

`show-pubkey` and `verify` load one typed artifact and verify metadata, scalar,
derived public key and backend integrity. Verification never performs an
unauthorized format conversion.

Registration proof signs a versioned domain-separated statement binding chain id,
ValidatorSet domain/address, validator EVM address, BLS public key and expiry or
activation context. The exact statement must match ADR-S-VAL-001 verification. A proof
for one network or key cannot register another.

## Authoritative interfaces

| Responsibility | Owner/entrypoint |
|---|---|
| Entropy and scalar creation | keygen command plus cryptographic library |
| Secret envelope and backend | typed BLS/key-custody storage interface |
| Registration statement verification | ValidatorSet, ADR-S-VAL-001 |
| Threshold shares and DKG continuity | consensus DKG boundary, not keygen |
| Operator transaction submission | CLI, ADR-B-RPC-001 |

## Invariants

- Installed secret bytes decode to the public fingerprint in their metadata.
- One stable key id resolves to exactly one algorithm/purpose and secret identity.
- No normal command overwrites an existing secret artifact.
- Hybrid generation exposes neither half-completed pair as successful output.
- Plaintext secret files are never group/world accessible.
- Registration signatures are domain-, network-, validator- and public-key-bound.
- Backend migration preserves identity or explicitly creates a rotation.
- Errors and diagnostics contain paths/fingerprints at most, never secret bytes.

## Atomicity, replay and failure

Generation is not replay-idempotent: rerunning against existing targets refuses
rather than silently rotating identity. Temporary files are unique and cleaned on
failure. Crash recovery identifies staged artifacts and either completes a manifest
commit or removes the whole staged set. Keychain secret creation and marker install
use compensating cleanup so neither orphan is treated as a valid installation.

Registration signing is pure and repeatable for the same versioned statement.
Rotation is a separate audited state machine coordinated with validator/DKG
lifecycle; deleting or replacing a live key is never an incidental file operation.

## Security and trust assumptions

The host RNG, cryptographic implementations and selected secret backend are trusted.
Filesystem mode does not protect against root, process compromise, swap, core dumps
or shell history. Encrypted files are only as strong as KDF parameters and
passphrase custody. Keychain availability and namespace isolation vary by OS and
must be tested on supported deployment platforms.

## Compatibility and migration

Legacy raw-hex files remain loadable only through an explicit legacy path and can
be migrated to a versioned envelope after fingerprint confirmation. Envelope/KDF
versions are self-describing. Changing BLS variant, registration DST/statement or
keychain namespace requires coordinated ValidatorSet/runtime activation and cannot
be inferred from filename.

## Production-interface verification evidence

Inspected all keygen commands, backend resolution, BLS encrypted/plaintext/keychain
save/load paths, EVM creation and tests/callers. Tests exercise CLI parsing, basic
round trips and Unix permissions. There is no crash matrix, overwrite/rotation
contract, cross-platform backend suite, memory-zeroization evidence or production
registration lifecycle test. Status remains Proposed.

## Consequences

Key generation is auditable independently from genesis assembly and consensus DKG.
Runbooks and PFS-006 may reference its artifacts, but cannot redefine their
identity, storage or rotation semantics.

## Rejected alternatives

- **Treat keys as ordinary config files:** accidental overwrite/copy becomes an
  authority change.
- **Default to plaintext for operator convenience:** an unsafe development policy
  becomes the production happy path.
- **Identify keychain secrets by basename:** validators with the same conventional
  filename collide.
- **Combine key generation with genesis/DKG in one ADR:** distinct authorities,
  atomicity and recovery models become unauditable.

## Open questions and technical debt

1. `plaintext` is the global default backend. Change production guidance/defaults
   and require an explicit insecure-development acknowledgement.
2. `--passphrase` exposes the encrypted-backend passphrase through process argv;
   `BLS_PASSPHRASE` exposes it through process environment. Add protected prompt,
   descriptor or secret-agent input and remove unsafe production paths.
3. Encrypted saves use `std::fs::write`: they inherit umask, overwrite existing
   files, are not atomic/durable and do not verify mode `0600`.
4. Keychain markers also use `std::fs::write` and overwrite. If marker creation
   fails after `set_password`, the secret is orphaned without rollback.
5. Keychain account is only `path.file_stem()`. Conventional
   `validator-N/signing-key.hex` paths all map to account `signing-key` and silently
   replace each other in the same OS account.
6. The keychain marker trusts service/account text from a writable file and does
   not bind an expected public fingerprint. Tampering can substitute another key.
7. Plaintext/EVM atomic rename replaces an existing destination on Unix. Add
   no-clobber installation; process-id temporary names also collide between stale
   files or multiple operations in one process.
8. Plaintext and EVM writes sync the file but not the containing directory, so a
   reported key may disappear after power loss.
9. `hybrid` installs BLS before generating/installing EVM. Any later failure leaves
   a partial identity, and it always writes EVM plaintext even when an encrypted or
   OS-level backend was requested.
10. EVM generation checks only all-zero random bytes rather than constructing and
    validating a secp256k1 signer before installation.
11. BLS encrypted plaintext, passphrase copies, derived AES key, decoded raw bytes
    and EVM byte arrays are not demonstrably zeroized from memory.
12. Argon2 uses library-default parameters without encoding them in the envelope.
    Pin reviewed memory/time/parallelism values and support versioned migration.
13. AES-GCM uses empty AAD, so algorithm, purpose, public fingerprint and network
    metadata are not authenticated or even stored.
14. Registration signs only the 20-byte validator address. It is not bound to chain
    id, ValidatorSet address, public key or expiry and may be replayable across
    compatible deployments; coordinate a versioned statement with ADR-S-VAL-001.
15. `verify` proves a local sign/verify round trip under a test namespace, not that
    the key matches configured validator/genesis identity or current on-chain
    registration.
16. Load auto-detects plaintext regardless of selected backend. Define whether a
    production encrypted policy must reject legacy plaintext instead of silently
    accepting it.
17. Output directories are created with ambient permissions and symlink/path race
    behavior is unspecified. Require private directories and safe path traversal.
18. Public-key and signature stdout formats are human text only. Add a stable
    machine-readable output schema without secret fields.
19. There is no backup, restore, escrow, compromise, revocation or coordinated
    rotation runbook tied to validator/DKG state.
20. Add fault-injection tests at create/write/sync/rename/keychain steps,
    multi-validator keychain collision tests, wrong-backend/substitution tests and
    an e2e registration proof accepted on exactly the intended chain.
