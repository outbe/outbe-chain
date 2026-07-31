# ADR-C-GRT-001: Gratis is the non-transferable earned-value ledger

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Gratis protocol maintainers
- **Scope:** `crates/core/gratis`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004
- **Related:** ADR-C-GRT-002, ADR-C-GRT-003, ADR-C-FID-001, ADR-C-PRM-001
- **Supersedes:** Gratis ledger portions of former `030-assets-credit-promises-and-factory-authority.md`

## Context

Gratis records earned protocol value before conversion to native COEN or use as
Credis collateral. It is intentionally not a user-transferable ERC-20. Policy and
proof validation live in Gratisfactory and GratisPool; this ADR covers only the
ledger and its conservation rules.

## Decision

Gratis owns `total_supply`, per-account liquid balances, and per-account pledged
balances. Its public ABI exposes reads; mutation is available only through a small
enumerated internal API to authorized protocol modules.

The ledger supports four primitive transitions:

```text
mint(account, amount):       liquid += amount; supply += amount
burn(account, amount):       liquid -= amount; supply -= amount
pledge(account, amount):     liquid(account) -= amount;
                             liquid(CREDIS_ADDRESS) += amount;
                             pledged(account) += amount
unpledge(account, amount):   inverse of pledge
```

Pledge and unpledge do not change supply. There is no arbitrary account-to-account
user transfer. A zero amount is either a documented no-op or rejected consistently;
it must never create index membership or misleading events.

## Authoritative interfaces and authority

`GratisContract` storage methods and `gratis::api` are the production ledger
boundary. Gratisfactory is the normal mint/burn/pledge workflow owner. Any other
internal caller must be explicitly enumerated by a structural test; possession of a
Rust `StorageHandle` is not authorization policy.

The ledger validates sufficient balances, checked supply arithmetic and exact
escrow movement. It does not decide Fidelity eligibility, denomination, shielded
proof validity, COEN conversion rate, or Credis terms.

## Persistent state and invariants

Required structural closure is:

```text
total_supply == sum(all liquid balances)
sum(all pledged balances) == liquid_balance(CREDIS_ADDRESS)
```

Pledged amounts are attributed to their original accounts even though the liquid
backing is held at the shared escrow address. No pledged balance may exist without
matching escrow. Supply and balances may not underflow, saturate or silently clamp.

If zero balances remain indexed, index semantics and bounded enumeration must be
specified; otherwise mutation removes them atomically.

## Atomicity, replay and failure

Each primitive mutation and its event is one EVM rollback domain. Cross-module
commands such as mining COEN or shielded pledge are atomic at their orchestrator;
Gratis must not persist when a later Fidelity, proof, native-balance or factory
effect fails.

Insufficient balance, zero/invalid address and overflow are transaction errors.
Supply/escrow disagreement or an impossible stored amount is consensus-state
corruption and must surface as an invariant failure rather than a plausible zero.

The ledger itself has no replay key. Calling modules must ensure that a proof,
mining command or system allocation cannot invoke the primitive twice.

## Security, compatibility and activation

Internal mutation APIs are privileged. The address of shared Credis escrow, storage
slot order, amount scale and event ABI are consensus formats. Migration must prove
both supply and escrow closure before and after activation.

Gratis is 18-decimal protocol value but is not guaranteed economically equivalent
to an arbitrary external token. Callers may not infer transferability from ERC-like
method names.

## Production-interface verification evidence

The current runtime and API paths for mint, burn, internal transfer, pledge and
unpledge were inspected together with their Gratisfactory callers. Existing tests
exercise normal balance changes and failure paths, but there is no exhaustive
caller-closure test or generated supply/escrow state machine. Status remains
Proposed.

## Consequences

Gratis remains a deliberately shallow accounting kernel. Business eligibility and
cryptography cannot leak into it, while every orchestrator imports two clear
conservation equations.

## Rejected alternatives

- **Expose ERC-20 transfer:** it bypasses Fidelity and collateral semantics.
- **Burn pledged Gratis on pledge:** it would destroy explicit collateral backing.
- **Store only aggregate escrow:** per-account unpledge authority would be lost.
- **Silently repair underflow:** deterministic corruption becomes invisible.

## Open questions and technical debt

1. Add a structural production-caller test covering every `gratis::api` mutator and
   fail CI when a new caller is introduced without an ADR authority update.
2. Add a generated model proving supply and escrow equations over arbitrary
   mint/burn/pledge/unpledge sequences and injected downstream rollback.
3. Define zero-amount behavior consistently across internal API, ABI and events.
4. Prove whether balance and pledged maps have enumerable indexes; if not, define
   how production invariant tooling computes full closure.
5. Credis currently consumes a shielded nullifier without an obvious ledger-level
   reservation tying a position to pledged escrow. ADR-C-PRM-002 must close the seam;
   Gratis cannot presently prove live-position backing.
6. Define repair/migration procedure for an escrow mismatch; ordinary user
   unpledge must not be used as an implicit repair mechanism.
7. Verify native COEN mining preserves `Gratis burned == COEN credited` across every
   nested-call failure and fee rule in ADR-C-GRT-002.
8. Establish maximum supply/account count bounds for arithmetic, queryability and
   invariant scanning.
9. Human protocol review is required before accepting the non-transferability and
   shared-escrow model as normative.
