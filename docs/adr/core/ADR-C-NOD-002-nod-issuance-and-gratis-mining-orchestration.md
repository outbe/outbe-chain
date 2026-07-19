# ADR-C-NOD-002: NodFactory owns Nod issuance and PoW-gated Gratis mining orchestration

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `crates/core/nodfactory`, its inbound ABI, outbound token/vault
  calls and cross-module issuance/mining commands
- **Depends on:** ADR-C-LYS-001, ADR-C-GRT-002, ADR-C-FID-001, ADR-C-VLT-001, ADR-C-NOD-001, ADR-C-LBM-001
- **Supersedes:** NodFactory assumptions previously embedded in Lysis documentation

## Context

NodFactory has two different authorities. Lysis calls it to construct one Nod from
validated transformation economics. A Nod owner later calls it to prove work, pay
the recorded cost into reserve liquidity, consume the qualified Nod and mint the
matching Gratis plus Fidelity cohort. It owns no independent persistent ledger, but
it owns an economically critical multi-module transaction boundary.

## Decision

`issue_nod` is a system-only typed command intended for Lysis. It validates owner
and uniqueness, derives canonical Nod and bucket identities, stamps canonical block
time, delegates authenticated ledger mutation to ADR-C-NOD-001 and emits `NodIssued`.

`INodFactory.mineGratis` is the sole user ABI command. It rejects value, requires exact
36-byte Nod id, verified owner and bucket bodies, caller ownership, valid bounded
PoW nonce and a qualified bucket. For nonzero recorded cost it transfers the chosen
asset from owner to NodFactory, approves VaultProvider and deposits the exact cost.
It then consumes the Nod, emits `NodBurned`, and mints exactly its `gratis_load_minor`
through Gratisfactory, which also records the Fidelity acquisition cohort.

## Inputs, effects and invariants

Issuance input comprises owner/day/league/floor/Gratis load/entry price/cost and
currency codes. The Nod id and bucket key are derived, never caller-selected, and
`issued_at` is the executing block timestamp. One owner/day identity can be issued
only once while live.

A successful mining receipt proves all of:

- caller was the authenticated Nod owner and the shared bucket was qualified;
- PoW validated over the exact 36-byte id and a nonce representable as `u64`;
- when cost is nonzero, the configured asset moved exactly that amount into the
  registered reserve-vault path;
- exactly one Nod and its membership/supply contribution were removed;
- exactly the recorded Gratis load was minted to the same owner and entered one
  Fidelity cohort;
- payment, allowance, vault shares, Nod/CE state, Gratis/Fidelity state and all
  events committed in the same EVM transaction or none did.

NodFactory has no durable replay map. The authenticated Nod deletion is the mining
replay guard: the same id cannot mine twice. PoW itself is reusable evidence and is
not a consumption marker.

## Authority and production entrypoints

The user can reach only `mineGratis`; no ABI issuance selector exists. Lysis calls
the public Rust `outbe_nodfactory::api::issue_nod`. Tests and other crates can call
the same function, so the intended Lysis authority is currently conventional.
Mining also has a public Rust API duplicating the ABI command.

Outbound effects use EVM subcalls to arbitrary `asset`, then the fixed
VaultProvider precompile. VaultProvider independently requires NodFactory to be a
registered `NodCostPrice` liquidity source and chooses the configured reserve vault.
Gratis/Fidelity mutation uses an in-process typed API after Nod removal.

## Atomicity, external calls and reentrancy

The outer EVM transaction journal is the authoritative rollback domain. A failure
in transfer, approval, vault deposit, Nod removal, event emission, Gratis mint or
Fidelity cohort must revert earlier child-call and compressed-entity effects.
Lysis provides a still larger checkpoint around all Nod issues and Tribute
consumption.

External token and vault calls occur before the Nod is consumed. The storage
subcall adapter rejects provider-borrow re-entry at its internal seam, but the
module requires explicit production evidence for EVM reentrancy and stale verified
capabilities. Checks-effects-interactions or a typed in-progress guard is preferred
if arbitrary token callbacks can re-enter the precompile.

## Determinism, PoW and bounds

PoW uses the shared `outbe_common::pow` scheme over the exact encoded Nod id and a
big-endian `u64` nonce. Hash recipe and difficulty are consensus/economic
compatibility surfaces. All loops in the mining command are constant-sized, but
child calls currently forward an effectively unbounded gas limit and depend on
outer EVM accounting.

Issuance economics are already computed by Lysis; NodFactory must validate or
faithfully transport them rather than create a second formula. Timestamp, currency
registry and asset mapping must come from canonical block/state inputs.

## Compatibility and production evidence

Inbound/outbound ABI selectors, event order, PoW preimage/difficulty, identity
derivation, currency/asset mapping and cross-module receipt schema require
activation-controlled evolution. Token compatibility includes return-data behavior,
allowance semantics and vault asset conformance.

Evidence inspected includes NodFactory runtime/API/precompile/errors/tests and
Solidity interfaces, Lysis production caller, Nod verified-capability API,
Gratisfactory/Fidelity mint path, VaultProvider authorization/deposit path and EVM
subcall adapter. Current unit tests cover issuance overlay visibility, duplicate and
owner rejection, qualified zero-cost removal and event order. They do not prove the
nonzero-cost production path.

## module audit profile

The intended commands are `IssueFromLysis(LysisNodReceipt)` and
`MineQualifiedNod(MiningRequest) -> MiningReceipt`. The latter receipt must account
for asset movement, vault shares, consumed Nod id and minted Gratis/cohort. Closure
requires typed asset selection, checked token results, reentrancy safety and tests
through the real ABI/subcall interfaces.

## Consequences and rejected alternatives

Keeping orchestration outside Nod preserves a small authenticated ledger and makes
external payment risk independently auditable. Deleting before payment was not
adopted in current code, but journal rollback means either order can be atomic;
the selected order still needs a reentrancy argument. Treating PoW as a ledger
field was rejected because Nod deletion already provides one-shot consumption.
Combining this ADR with ADR-C-NOD-001 was rejected because external assets, VaultProvider
and Gratis/Fidelity are a separate authority and failure domain.

## Open questions and technical debt

- Bind `asset` to the Nod's recorded `reference_currency` through an authoritative
  currency/asset registry. The production code contains an explicit TODO and today
  accepts any nonzero token address for a nonzero cost.
- Decode and require `true` from ERC20 `transferFrom` and `approve`; current raw
  `storage.call` treats a successful frame with `false` return data as success.
- Define safe allowance handling for USDT-like zero-first tokens, fee-on-transfer,
  rebasing and malicious tokens. Prove the vault received exactly `cost`, not merely
  that calls returned.
- Validate the VaultProvider returned share amount and bind a minimum/expected
  receipt if economic conservation depends on shares; NodFactory currently ignores
  the decoded result.
- Close `issue_nod` behind an unforgeable Lysis capability/receipt and validate all
  issue economics at this boundary. Any crate can currently issue arbitrary Nods.
- Add a reentrancy proof/test for malicious asset and vault callbacks. External
  calls happen before the Nod replay guard is consumed.
- Put an explicit checkpoint/command guard around the complete mining orchestration
  or prove the outer EVM journal always includes CE overlay events and every
  in-process mutation on all production entrypoints.
- Define zero-cost mining asset semantics in the ABI. Accepting any address,
  including zero, is implemented but not capability/version signaled.
- Pin and version PoW difficulty/preimage and specify whether old Nods retain their
  issuance-era difficulty after a protocol update.
- Validate nonzero Gratis load and cost/floor/entry/currency relationships before
  issuance; a zero-load Nod can currently be mined for a zero mint.
- Add nonzero-cost production tests using real ERC20 return variants, allowance,
  registered VaultProvider/vault, rollback at every step, malicious callbacks and
  exact balance/share conservation.
- Add replay/concurrency tests for two mining transactions targeting the same Nod
  and for re-execution after a reverted downstream Fidelity mutation.
