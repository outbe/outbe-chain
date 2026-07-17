# ADR-C-TOK-001: Native, wrapped and synthetic tokens have explicit issuance authorities

- **Status:** Proposed; current Solidity implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `contracts/tokens` token implementations excluding bridge custody
- **Depends on:** ADR-B-EVM-002, ADR-B-CRY-001
- **Related flows:** PFS-003, PFS-004

## Context

The contracts package includes wrapped native COEN, a development USDT, and
configurable ERC-7802 synthetic ERC-20s including reference-currency metadata. These
balances are settlement assets used by Vault, Intex and bridge protocols.

## Decision

WCOEN is a strict one-to-one wrapper: mint only by receiving native value and burn only
by returning the same native amount to the holder. Synthetic ERC-7802 supply changes
only through one configured bridge authority. Reference currency ISO code, decimals,
name and symbol are immutable identity. A development mintable token is never used as a
production canonical asset without an explicit issuance policy and deployment profile.

Bridge authority installation/rotation is governed, two-step and impossible while it
would create two concurrent minters. Tokens reject zero/invalid bridge configuration and
emit complete configuration changes.

## Authoritative interfaces

- WCOEN `deposit/receive`, `withdraw`, transfer and allowance methods own wrapped value.
- `ConfigurableERC7802` owns bridge authorization for cross-chain mint/burn.
- `BridgeableERC20Stable.isoCode` owns immutable reference-currency identity.
- Deployment manifests own canonical address, code hash, decimals and supply authority.

## Invariants

- WCOEN total supply equals native currency held by WCOEN at every committed boundary.
- Synthetic total supply changes only by authorized bridge burn/mint.
- Metadata/decimals/ISO identity never changes after deployment.
- Allowance arithmetic and transfers follow the declared ERC-20 compatibility profile.
- No production token exposes unrestricted public mint.

## Atomicity, replay and failure

Deposit and mint, burn and native withdrawal, transfer and balance/allowance updates are
atomic. Native withdrawal uses reentrancy-safe state ordering and rolls back on failed
delivery. Bridge replay protection belongs to ADR-C-TOK-002 before token mint/burn.

## Determinism and bounds

All amounts use checked arithmetic; decimals conversions occur outside token contracts
or through one checked library. Metadata sizes are bounded at deployment.

## Compatibility, trust and activation

Token bytecode, address, metadata, decimals, ISO code, owner and bridge form a signed
deployment manifest. Canonical/synthetic pairing and domain are externally auditable.

## Production-interface verification evidence

Inspected WCOEN deposit/withdraw/allowance/transfer, configurable bridge gate, synthetic
constructors and public dev-USDT mint. Tests exist under `contracts/tokens`, but release
evidence does not yet distinguish development and production token profiles.

## Consequences

Downstream protocols can reason about amount units and issuance authority explicitly.
Deployment configuration becomes part of the protocol contract.

## Rejected alternatives

- Inferring token identity only from symbol/name is rejected.
- Multiple active synthetic minters are rejected.
- Treating development faucet mint as production policy is rejected.

## Open questions and technical debt

- **Critical:** `contracts/tokens/src/native/USDT.sol` exposes unrestricted `mint`; make
  its development-only status impossible to confuse in production deployment tooling.
- Prove WCOEN native backing under reentrancy, forced native transfers and failed
  recipient callbacks; define handling of surplus forced balance.
- Replace immediate owner `setTokenBridge` with governed two-step rotation and ensure old
  authority is revoked atomically.
- Pin canonical token addresses/decimals/ISO codes and bytecode hashes per network.
- Add invariant tests for WCOEN backing and synthetic supply changes through the real
  paired bridge only.

