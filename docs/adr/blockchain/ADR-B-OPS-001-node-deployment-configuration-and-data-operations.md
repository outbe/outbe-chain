# ADR-B-OPS-001: Node deployment, configuration and data operations use one verifiable profile

- **Status:** Proposed; current systemd, localnet and monitoring surfaces profiled
- **Date:** 2026-07-17
- **Owners/scope:** node packaging, host/container topology, configuration, secrets, ports, data ownership, backup and operational lifecycle
- **Depends on:** ADR-B-NOD-001, ADR-B-GEN-001, ADR-B-SUP-001, ADR-B-DEP-001, ADR-B-OCD-014, ADR-B-OCD-015

## Context

An Outbe node is not only the `outbe-chain` process. A validator deployment combines
Reth data, consensus state and keys, compressed-entity state, a finalized Mongo
projection, optional enclave state, network/RPC/metrics listeners and monitoring. The
repository currently exposes systemd units and environment examples, local testnet and
Docker orchestration, Mongo replica-set bootstrap, `mise` tasks and operator guidance.
Those surfaces collectively determine whether a node starts with one coherent identity,
fails visibly, preserves durable state and can be recovered safely.

Localnet convenience is useful verification evidence, but it is not a production
deployment contract. A process being alive is also weaker than a node being ready to
serve or vote.

## Decision

Every deployed node is described by one versioned `NodeDeploymentProfile`. The profile
binds release/binary digest, role, chain/genesis identity, protocol activation schedule,
contract deployment manifest, public and private listeners, peer/consensus identity,
all durable stores, Mongo logical database, key and enclave references, resource limits,
observability endpoints and backup policy. Startup validates the resolved profile before
opening mutation interfaces or joining consensus and emits a redacted effective-profile
digest for operations and incident evidence.

Configuration precedence is explicit and inspectable: command-line overrides a named
profile, which overrides environment-backed deployment values, which overrides documented
defaults. Unknown keys, incompatible duplicates and missing role-required values fail
closed. Secrets are references to permissioned files or an external secret provider; they
are not embedded in the profile digest, process arguments, logs or checked-in environment
files.

The supported operational actions are distinct typed plans:

- `start` validates identity and storage, starts dependencies, waits for readiness and
  only then advertises service or enables validator participation;
- `stop` drains voting/submission, requests bounded graceful shutdown and preserves every
  durable store;
- `restart` is `stop` plus `start` against the same verified profile;
- `backup`/`restore` follow authenticated snapshot and cross-store checkpoint rules; and
- `clean` is destructive, names every path/database/volume to remove and requires explicit
  confirmation or a disposable-environment marker.

## Deployment topology and ownership

The profile assigns one owner and recovery rule to each component:

| Component | Required identity/ownership |
|---|---|
| Reth execution database | chain/genesis and node data directory |
| Consensus store | chain, validator identity, committee/key epoch and monotonic progress |
| Compressed-entity store | schema/root/checkpoint governed by ADR-B-OCD-014 and ADR-B-OCD-015 |
| Mongo projection | chain and validator-specific logical database plus writer lease/fencing |
| Signing/EVM keys | validator identity and permissioned secret reference |
| Enclave sealed state | enclave identity, policy and key epoch |
| Logs/metrics/checkpoints | deployment-profile digest and redacted node identity |

One Mongo server or replica set may host several validators in local or controlled
environments, but validators never share one logical projection database. Each validator
uses a distinct database namespace and independent writer lease/fencing identity. Resource,
backup and failure-domain analysis must account for the shared physical service.

Full-node/follower and validator profiles are separate. A validator profile additionally
requires consensus listener/storage, signing and EVM identities, enclave/key readiness and
eligibility/voting gates. Sidecars such as Mongo, an enclave service or monitoring are
explicit dependencies with health contracts, not hidden assumptions.

## Network exposure and readiness

Every listener has an explicit address, port, protocol, authentication/TLS expectation and
exposure class. P2P and consensus ports are not conflated; RPC namespaces are allowlisted;
admin/debug RPC and metrics default to private interfaces. Multi-node hosts validate port,
directory, database and identity uniqueness before starting any process.

Readiness follows ADR-B-SUP-001: required stores are reconciled, projection policy is
satisfied, consensus health is known and the configured RPC view is safe. Service managers
must distinguish startup, ready, degraded, draining and fatal states. Restart policy uses
bounded exponential backoff and escalation for repeated invariant/configuration failures;
it must not turn deterministic corruption or incompatibility into an endless crash loop.

## Atomicity, failure and recovery

Deployment/startup is a resumable saga with observable dependency steps. A failed start
does not silently initialize a new chain, database, key or data directory. Cross-store
recovery selects a common finalized checkpoint; Mongo may be rebuilt only from authenticated
chain history according to ADR-B-OCD-014. Backup captures a compatible checkpoint manifest
and restore verifies it before service advertisement. Restore and disaster-recovery drills
are production-interface tests, not documentation-only procedures.

Graceful shutdown has one deadline and reports which participant failed to drain. Forced
termination is an explicit degraded outcome followed by reconciliation on restart. PID
files and container names are hints, not sufficient process identity: stop tooling verifies
the executable/deployment identity before signalling or removing resources.

## Determinism, compatibility and upgrades

The same profile plus pinned artifacts produces the same effective topology. Host-specific
paths and secret references may differ, but their semantic roles and permissions are part
of the profile. Node-binary upgrades declare database, wire, genesis/schema and contract
compatibility and coordinate activation with ADR-B-GEN-001 and ADR-B-DEP-001. Rollback is
allowed only when every opened store and activated protocol remains backward compatible.

Localnet is a named disposable verification profile. Its one Mongo container with
per-validator databases is valid for demonstrations and E2E tests, but does not establish
production availability, isolation, authentication, capacity or backup guarantees.

## Production-interface verification evidence

Inspected `deploy/systemd/outbe-chain.service`, `outbe-validator.service`, environment
examples, `deploy/monitoring`, `mise.toml`, `scripts/localnet-stack.sh`,
`scripts/run-testnet.sh` and validator operations documentation. Current localnet startup
checks Mongo reachability/primary election and uses distinct validator databases. Current
stop preserves chain data while clean removes the stack, but the contract is implemented
across shell conventions rather than one machine-checked profile.

## Consequences

Operators can identify exactly which binary, chain, keys, stores and listeners form a node,
and automation can reject partial or conflicting deployments before mutation. Shared local
infrastructure remains possible without confusing shared physical Mongo with shared logical
validator state.

## Rejected alternatives

- Environment files and shell defaults as the unversioned source of truth are rejected.
- “systemd/container says running” as readiness evidence is rejected.
- One logical Mongo database shared by several validators is rejected.
- Treating stop and destructive cleanup as synonyms is rejected.
- Automatic restart forever after deterministic fatal errors is rejected.

## Open questions and technical debt

- **Critical:** define and implement the versioned `NodeDeploymentProfile`; current
  systemd, environment, CLI, shell and documentation values can drift without one digest
  binding binary, genesis, stores, keys, database names and ports.
- **Critical:** verify systemd writable-path coverage for compressed-entity, projection,
  enclave, key and checkpoint state. The validator unit currently names only `DATADIR` and
  `CONSENSUS_STORAGE` under `ProtectSystem=strict`.
- **Critical:** replace raw signing/EVM-key environment values or ambiguous key-path
  conventions with permission-checked secret references; add startup redaction tests.
- Add service-manager readiness/watchdog integration and classify configuration/schema/
  corruption failures so `Restart=on-failure` does not loop indefinitely.
- Add a production Mongo topology with authentication/TLS, per-validator database/writer
  fencing, capacity limits, backup retention and restore drills. One local container proves
  functionality, not availability or isolation.
- Harden PID-file/container stop logic against stale PID reuse and wrong deployment
  identity; retain an auditable forced-shutdown outcome instead of suppressing all errors.
- Make destructive `clean` enumerate targets and require confirmation or a signed
  disposable-profile marker; test that ordinary stop preserves every store and key.
- Define disk/inode headroom, log rotation, metrics/alerts and escalation thresholds for
  Reth, consensus, compressed entities and Mongo lag/checkpoint divergence.
- Add upgrade/rollback drills binding node binary, wire/schema activation, contract
  deployment manifest and all persistent-store versions.
- Add authenticated multi-store backup/restore and disaster-recovery tests with measured
  recovery point and recovery time objectives.
