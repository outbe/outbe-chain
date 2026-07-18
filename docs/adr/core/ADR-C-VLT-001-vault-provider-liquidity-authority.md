# ADR-C-VLT-001: VaultProvider owns reserve routing and liquidity authority

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Treasury and protocol execution maintainers
- **Scope:** `crates/core/vaultprovider` and its external ERC-20/vault/token-bundle seams
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-B-OCD-006,
  ADR-S-GOV-001 through ADR-S-GOV-003
- **Related:** ADR-C-GRT-002, ADR-C-CRD-002, ADR-C-INX-002, ADR-C-GEM-002
- **Supersedes:** VaultProvider portions of former broad pre-space Gratis/economic aggregate (previously numbered 030)

## Context

VaultProvider is the protocol's reserve gateway. It maps assets to external vaults,
classifies which module addresses may deposit or withdraw, holds vault shares and
routes withdrawn assets into token-bundle receivers. A registry error can redirect
all settlement or credit liquidity, so it is an independently privileged stateful
module.

## Decision

### Administrative authority

A genesis-seeded owner exclusively adds/removes vaults and liquidity source/target
accounts. The owner is protocol administration governed by ADR-S-GOV-001 through ADR-S-GOV-003, not an
ordinary operator convenience key.

Adding a vault staticcalls its `asset()`, inserts the asset and vault into enumerable
sets, and approves the vault for maximum asset allowance. Removing it deletes set
membership, removes an empty asset and revokes allowance. Duplicate/missing entries
revert.

Adding a source or target stores both enumerable membership and its declared enum
type. Removal clears both. Only declared non-Unknown enum variants are valid.

### Liquidity commands

ABI dispatch derives caller classification from registry state:

- `depositLiquidity(asset, amount)` is available only to a registered source. It
  selects the configured vault, pulls asset from caller into VaultProvider, deposits
  on behalf of VaultProvider and returns minted shares.
- `withdrawLiquidity(asset, amount, receiver)` is available only to a registered
  target. It previews required shares, verifies VaultProvider's vault-share balance,
  withdraws asset to itself, approves the receiver bundle and calls its top-up,
  returning burned shares.

V1 deterministically selects the first vault in the asset's enumerable set. This is
observed behavior but not an accepted long-term routing policy.

## Persistent state and invariants

- `asset in assets` iff its vault set is nonempty.
- Every registered vault reports the same asset under whose set it appears.
- Vault membership is unique; removal revokes its allowance.
- Source/target set membership iff its type mapping is a declared non-Unknown value.
- ABI caller classification is loaded from storage, never supplied by caller.
- Provider-held vault shares and emitted deposit/withdraw movement reconcile with
  external token/vault balance deltas.
- A withdrawal cannot burn more shares than Provider holds and tops up exactly the
  requested receiver/asset under supported-token semantics.

## Atomicity, external calls and failure

Each registry or liquidity command is one EVM frame including all nested calls and
events. Failed transfer, deposit, preview, withdrawal, approval or bundle top-up
reverts registry/accounting and external state under EVM call semantics.

External ERC-20, vault and bundle contracts are trust boundaries. Return data must
decode; false/missing ERC-20 returns, fee-on-transfer, rebasing, malicious callbacks
and vault share-price changes require explicit supported-contract policy and tests.

Configuration corruption, invalid enum values and asset mismatch are invariant
failures. Insufficient allowance/balance/shares or unauthorized caller are business
reverts.

## Replay, compatibility and activation

Liquidity calls are not intrinsically idempotent; owning factories consume their
position/Nod/Intex state in the same transaction. Registry mutations use set
membership as duplicate guards.

Owner address, enum discriminants, set layout, vault ABI, selected routing rule and
registered module identities are activation-critical. Upgrades/reconfiguration need
timelock/governance, preflight asset/share reconciliation and rollback plan.

## Production-interface verification evidence

Inspected storage schema, owner gates, set enumeration, vault add/remove allowances,
source/target registration, ABI-derived authorization, deposit/withdraw nested calls
and unit tests. Tests use generic subcall stubs and do not fully exercise real
ERC-20/vault/bundle adversarial behavior, multi-vault migration or governance.

## Consequences

Factories import a narrow reserve interface instead of owning vault selection and
shares. Treasury authority and external-contract risk become visible in one architecture
audit.

## Rejected alternatives

- **Let factories call arbitrary vaults:** reserve policy and audit surface fragment.
- **Accept source/target type as calldata:** any caller could impersonate a role.
- **Use `assetAt(0)` as economic identity:** enumeration order is configuration, not
  currency binding.
- **Remove vault without revoking allowance:** removed code retains asset authority.

## Open questions and technical debt

1. V1 always chooses the first vault. Define allocation/routing, failover, share
   migration and behavior when the first vault is removed or reordered.
2. Source/target registration currently rejects only `Unknown`; ensure arbitrary
   undeclared nonzero `u8` values cannot be stored and decoded back to Unknown.
3. Define governance/timelock/emergency and key-rotation policy for the seeded owner.
4. Add real-contract conformance tests for standard, false-return, no-return,
   fee-on-transfer, rebasing and callback-capable tokens; explicitly allow/reject each.
5. Verify actual asset and share balance deltas rather than trusting reported vault
   return values where protocol conservation depends on them.
6. Maximum approval on add expands external trust. Evaluate exact/per-call allowance
   or formally accepted vault trust and ensure removal/upgrade revokes old approvals.
7. Token-bundle receiver is arbitrary calldata from a registered target. Define
   receiver contract/interface validation and prevent approval residue/theft.
8. Add reentrancy analysis/tests across token, vault and bundle callbacks.
9. Add structural deployment tests proving every factory has exactly the intended
   source/target enum and no obsolete address remains authorized.
10. Define asset/vault health, pause, loss/socialization and emergency withdrawal
    FSM; current registry has no operational safety states.
11. Multi-vault shares lack an internal obligation ledger. Define how reserves are
    attributed/reconciled to outstanding Credis/Intex/Gem obligations.
12. Add pagination/capacity bounds for enumerable assets, vaults and role accounts.
13. Ensure add/remove state changes roll back when allowance calls return false or
    malformed data; current helpers need strict safe-call semantics.
14. Add production ABI e2e covering deposit/withdraw, failure rollback, registry
    rotation and restart with real contracts.
