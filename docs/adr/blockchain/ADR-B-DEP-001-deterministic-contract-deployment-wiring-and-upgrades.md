# ADR-B-DEP-001: Contract deployment, wiring and upgrades use one verifiable manifest

- **Status:** Proposed; current Solidity and script implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** deterministic factories, Solidity deploy/wire/upgrade scripts and deployment artifacts
- **Depends on:** ADR-B-GEN-001, ADR-B-CRY-001, ADR-B-CLI-001

## Context

Outbe deploys protocol contracts across Outbe and external chains using CREATE2/CREATE3,
ERC-1967/UUPS proxies, role grants, remote-peer wiring and constructor immutables. Address
stability alone does not prove the expected implementation, initialization or authority.
Scripts are production mutation interfaces because they can install or replace protocol
code and trust roots.

## Decision

Every environment has one signed, versioned deployment manifest containing chain/genesis
identity, factory/deployer/salt derivation, predicted address, creation/runtime code hash,
proxy implementation/admin, constructor immutables, initializer calldata, storage-layout
hash, roles, downstream addresses, remote domains/peers, token metadata and activation
transaction/height. Deployment first computes and displays this manifest, then applies an
idempotent plan, verifies on-chain state/code after each step and emits an immutable result.

CREATE3 salts are namespaced and stable, but an occupied predicted address is accepted
only if code/configuration exactly match the manifest. UUPS upgrade validates UUID,
implementation code, immutable constructor profile, storage compatibility and migration
postconditions before activation. Wiring/role changes are typed two-step plans with
old/new values and outstanding-liability guards.

## Authoritative interfaces

- Canonical CREATE2 deployer plus `Create3Factory/CreateX` own address derivation.
- Package deploy scripts own proxy/implementation creation and initialization plan.
- Wire/configuration tasks own dependency/peer/role installation.
- Upgrade scripts own implementation change and optional reinitializer/migration.
- The checked-in/release manifest, not console output or `.env`, is deployment authority.

## Invariants

- The same manifest deterministically derives the same intended address/configuration.
- Re-running a completed plan is a verified no-op; mismatched occupied addresses fail.
- Proxy implementation and all immutables/storage/roles match one compatible release.
- Initialization occurs exactly once and no uninitialized proxy is externally claimable.
- No upgrade or rewiring loses pending messages, custody, locks, supply or replay state.
- Secrets never enter manifest, logs, artifacts or command history.

## Atomicity, replay and failure

Multi-transaction deployment is an explicit saga with per-step state and resume rules.
Later steps require verified receipts/postconditions of prerequisites. Failure never causes
the tool to redeploy at a different address silently. Upgrade rollback is a new governed
upgrade with compatibility evidence, not an unchecked pointer flip.

## Determinism and bounds

Salt/hash encodings are canonical and independently reproducible. Plans bound RPC waits,
fees and retries. Array configuration is canonicalized or order-committed. Artifact/build
metadata that changes bytecode is pinned.

## Security, compatibility and activation

Deployer, proxy admin/default admin, timelock/governance and emergency authority are
explicit trust roots with transfer/revocation procedure. Cross-chain activation orders
peers and versions so no side accepts incompatible messages. Storage layout and migration
are checked for every UUPS implementation.

## Production-interface verification evidence

Inspected ownerless namespaced `Create3Factory`, intent `CreateX`, Intex deploy/wire and
upgrade scripts, deterministic proxy tests and package-specific deployment tasks. Current
upgrade script explicitly uses empty migration calldata and assumes preserved storage;
deployment evidence is spread across console output and environment files.

## Consequences

Operators can prove what code and authority exists at every advertised address. Package
ADRs can reference one activation artifact instead of restating script behavior.

## Rejected alternatives

- “Code exists at predicted address” as sufficient idempotency is rejected.
- Empty upgrade calldata by convention is rejected when schema changes.
- Mutable `.env` files as authoritative deployment registry are rejected.

## Open questions and technical debt

- **Critical:** current deploy reuse paths often check only `code.length != 0`; verify
  runtime hash, proxy implementation, initializer state, immutables, roles and wiring.
- **Critical:** UUPS upgrade scripts use empty calldata and no machine-checked storage
  layout/migration postconditions. Add layout diff and upgrade simulation gates.
- Define the canonical checked-in/signed manifest format and remove conflicting address
  registries across Rust constants, MCP, scripts and package deployment files.
- Move immediate deployer-held admin roles to governed/timelocked authorities with tested
  two-step transfer and emergency policy.
- Add crash/resume tests at every deployment/wiring step and adversarial occupied-address,
  wrong-chain, wrong-factory/salt and partial-role scenarios.
- Produce reproducible bytecode builds and record compiler, optimizer, libraries and
  dependency hashes.
- Define cross-chain activation/rollback order for in-flight messages and custody before
  peer or implementation rotation.
