# ADR-C-PRM-001: Promis is a non-transferable future-value ledger

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Promis protocol maintainers
- **Scope:** `crates/core/promis`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004
- **Related:** ADR-C-FID-001, ADR-C-PRM-002, ADR-C-PRM-003
- **Supersedes:** Promis ledger portions of former broad pre-space Gratis/economic aggregate (previously numbered 030)

## Context

Promis represents protocol value that can later be converted through
PromisFactory. Users can inspect balances but cannot transfer, mint or burn it
through the Promis ABI. This ledger must be auditable independently from issuance
sources and conversion policy.

## Decision

Promis owns only `total_supply` and `balances[address]`, using 18 decimals. Its ABI
exposes metadata, total supply, balances and ERC-165 support. Mutation is an internal
API with two primitives:

```text
mint(account, amount): balance += amount; total_supply += amount
burn(account, amount): balance -= amount; total_supply -= amount
```

Mint rejects zero amount, zero account and overflow. Burn rejects zero amount and
insufficient balance. Both update balance and supply and emit the ledger event in
one EVM rollback domain. User transfer and allowance do not exist.

## Authority and module boundary

PromisFactory is the intended mint/burn orchestrator. IntexFactory and GemFactory
must mint through PromisFactory so Fidelity is updated; any direct `Promis::mint` or
`burn` caller is a potential policy bypass and requires explicit ADR authorization.

The ledger does not own issuance caps, PromisLimit, Fidelity, PoW, native COEN or
Gratis conversion.

## Persistent state and invariants

```text
total_supply == sum(all account balances)
```

No balance or supply operation may underflow, overflow, saturate or clamp. A
successful event describes the committed post-state. If the map is not enumerable,
production invariant tooling must obtain closure from a separate authenticated
index rather than infer it from sampled accounts.

## Atomicity, replay and failure

Primitive mutation is atomic locally and rolls back with the calling factory's
Fidelity/native/Gratis effects. Promis supplies no replay protection; issuance and
conversion commands own their sequence, identity or consumed-asset guard.

Business validation errors revert. A state where supply is below a requested burn
despite sufficient account balance is corruption; current subtraction must not be
allowed to panic or wrap.

## Compatibility and evidence

Storage slots, decimals, metadata, events and non-transferability are protocol API.
Inspected schema, runtime, read-only dispatch and tests for metadata, mint, burn,
multiple users, overflow and insufficient balance. Caller authority and exhaustive
supply closure are not proven.

## Consequences

Promis is a small accounting kernel. Conversion workflows and caps can evolve only
through their own ADRs without exposing token-like transfer semantics.

## Rejected alternatives

- **Expose ERC-20 transfer/allowance:** it bypasses Fidelity provenance and factory
  conversion semantics.
- **Let every producer mint directly:** Fidelity and issuance controls become
  optional.
- **Fold PromisLimit into supply:** allocated balances and unallocated capacity are
  different facts with different owners.

## Open questions and technical debt

1. Add a structural caller test for every `Promis::mint` and `burn` production call;
   require normal producers to use PromisFactory.
2. Burn computes `supply - amount` without an explicit supply sufficiency check.
   Validate supply independently so corrupt state returns an invariant error rather
   than underflow behavior.
3. Add generated multi-account supply-closure tests with injected event/write and
   downstream factory failures.
4. Define how full supply closure is verified if the balance map is not enumerable.
5. Decide whether zero-address burn should be explicitly rejected for symmetry and
   corruption diagnostics.
6. Document maximum supply/account bounds and storage-query strategy.
7. Prove every Promis mint has an authorized source asset/outcome and cannot bypass
   PromisLimit where that limit is economically intended to apply.
8. Add ABI tests proving transfer/approve selectors are unavailable and cannot
   collide with precompile dispatch.
9. Define migration procedure that reconciles all balances before storage layout or
   scale changes.
10. Human acceptance is required for the non-transferable ledger contract.
