# ADR-C-NOD-002: NodFactory owns Nod issuance and PoW-gated Gratis mining orchestration

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `crates/core/nodfactory`, its inbound ABI, outbound token/vault
  calls and cross-module issuance/mining commands
- **Depends on:** ADR-C-LYS-001, ADR-C-GRT-002, ADR-C-FID-001, ADR-C-VLT-001, ADR-C-NOD-001, ADR-C-LBM-001
- **Supersedes:** NodFactory assumptions previously embedded in Lysis documentation

## Context

NodFactory has three different authorities. Lysis calls it to construct one Nod from
validated transformation economics. Anyone may later pay the recorded cost into
reserve liquidity on that Nod's behalf. The Nod owner separately proves work,
consumes the qualified and settled Nod and mints the matching Gratis plus Fidelity
cohort. It owns no independent persistent ledger, but it owns an economically
critical multi-module transaction boundary.

## Decision

`issue_nod` is a system-only typed command intended for Lysis. It validates owner
and uniqueness, derives canonical Nod and bucket identities, stamps canonical block
time, delegates authenticated ledger mutation to ADR-C-NOD-001 and emits `NodIssued`.

`INodFactory.settleNod` and `INodFactory.mineGratis` are the two user ABI commands.
Both reject value and require an exact 36-byte Nod id.

`settleNod` is deliberately unrestricted in payer, but takes no asset argument: any
address may settle any live Nod at any point in its life, including before its bucket
qualifies. It requires only that the Nod exists and is not already settled. For
nonzero recorded cost the settlement asset is resolved from VaultRouter's
`referenceCurrencyAssets` registry for the Nod's `reference_currency` — first entry,
since registry order carries no meaning — then transferred from the payer to
NodFactory, approved for VaultRouter and deposited as the exact cost; it then marks the authenticated Nod body settled and emits `NodSettled`.
Settlement is recorded on the Nod body, so it dies with the Nod and cannot be
inherited by a later Nod re-issued under the same derived identity.

`mineGratis` requires verified owner and bucket bodies, caller ownership, a valid
bounded PoW nonce, a qualified bucket and a settled Nod. It moves no value of its
own. It consumes the Nod, emits `NodBurned`, and mints exactly its
`gratis_load_minor` through Gratisfactory, which also records the Fidelity
acquisition cohort.

## Inputs, effects and invariants

Issuance input comprises owner/day/league/floor/Gratis load/entry price/cost and
currency codes. The Nod id and bucket key are derived, never caller-selected, and
`issued_at` is the executing block timestamp. One owner/day identity can be issued
only once while live.

A successful settlement receipt proves that, when cost is nonzero, a registry-resolved
asset denominating the Nod's reference currency moved exactly that amount into the
registered reserve-vault path, that `NodSettled` names that asset, and that the Nod
body carries `is_settled` afterwards — atomically or not at all.

A successful mining receipt proves all of:

- caller was the authenticated Nod owner and the shared bucket was qualified;
- PoW validated over the exact 36-byte id and a nonce representable as `u64`;
- the Nod was already settled;
- exactly one Nod and its membership/supply contribution were removed;
- exactly the recorded Gratis load was minted to the same owner and entered one
  Fidelity cohort;
- Nod/CE state, Gratis/Fidelity state and all events committed in the same EVM
  transaction or none did.

NodFactory has no durable replay map. The authenticated Nod deletion is the mining
replay guard: the same id cannot mine twice. PoW itself is reusable evidence and is
not a consumption marker.

## Authority and production entrypoints

The user can reach `settleNod` and `mineGratis`; no ABI issuance selector exists.
Lysis calls the public Rust `outbe_nodfactory::api::issue_nod`. Tests and other
crates can call the same function, so the intended Lysis authority is currently
conventional. Settling and mining also have public Rust APIs duplicating the ABI
commands.

Outbound effects use EVM subcalls to arbitrary `asset`, then the fixed
VaultRouter precompile. VaultRouter independently requires NodFactory to be a
registered `NodCostAmount` stables source and chooses the configured reserve vault.
Gratis/Fidelity mutation uses an in-process typed API after Nod removal.

## Atomicity, external calls and reentrancy

The outer EVM transaction journal is the authoritative rollback domain. A failure
in transfer, approval, vault deposit, settlement write, Nod removal, event emission,
Gratis mint or Fidelity cohort must revert earlier child-call and compressed-entity
effects. Lysis provides a still larger checkpoint around all Nod issues and Tribute
consumption.

`settleNod` follows checks-effects-interactions: the settled flag is written before
the token and vault subcalls, so a token that re-enters the precompile for the same
Nod hits `NodAlreadySettled` rather than paying twice. `mineGratis` no longer makes
external calls before consuming its replay guard. The storage subcall adapter
rejects provider-borrow re-entry at its internal seam, but the module still requires
explicit production evidence for EVM reentrancy and stale verified capabilities.

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
Gratisfactory/Fidelity mint path, VaultRouter authorization/deposit path and EVM
subcall adapter. Current unit tests cover issuance overlay visibility, duplicate and
owner rejection, qualified zero-cost removal, event order, the unsettled-mining
rejection, third-party settlement, double-settlement rejection, empty-registry
rollback and the stubbed nonzero-cost payment sequence. They do not prove the
nonzero-cost path against real ERC20 and vault implementations.

## module audit profile

The intended commands are `IssueFromLysis(LysisNodReceipt)`,
`SettleNod(SettlementRequest) -> SettlementReceipt` and
`MineQualifiedNod(MiningRequest) -> MiningReceipt`. The settlement receipt must
account for asset movement and vault shares; the mining receipt for consumed Nod id
and minted Gratis/cohort. Closure requires typed asset selection, checked token
results, reentrancy safety and tests through the real ABI/subcall interfaces.

## Consequences and rejected alternatives

Keeping orchestration outside Nod preserves a small authenticated ledger and makes
external payment risk independently auditable. Splitting payment out of mining lets
a Nod be funded ahead of time and by a third party, at the cost of one extra
transaction and one extra bit of authenticated body state. Recording `is_settled` on
the Nod body was chosen over a NodFactory-side map because derived Nod identities
are reusable after a burn, and a side map would leak a stale settled flag into a
re-issued Nod. Treating PoW as a ledger field was rejected because Nod deletion
already provides one-shot consumption.
Combining this ADR with ADR-C-NOD-001 was rejected because external assets, VaultRouter
and Gratis/Fidelity are a separate authority and failure domain.

## Open questions and technical debt

- `settleNod` resolves its asset from VaultRouter's `referenceCurrencyAssets`
  registry for the Nod's recorded `reference_currency`, so a caller can no longer
  choose the payment token. Taking the first registry entry is arbitrary by design
  (order is documented as meaningless); if several assets are ever registered for one
  currency, whether the payer should be able to pick among them, and whether issuance
  currency should ever take precedence as it does for Gem, are open.
- Decode and require `true` from ERC20 `transferFrom` and `approve`; current raw
  `storage.call` treats a successful frame with `false` return data as success.
- Define safe allowance handling for USDT-like zero-first tokens, fee-on-transfer,
  rebasing and malicious tokens. Prove the vault received exactly `cost`, not merely
  that calls returned.
- Validate the VaultRouter returned share amount and bind a minimum/expected
  receipt if economic conservation depends on shares; NodFactory currently ignores
  the decoded result.
- Close `issue_nod` behind an unforgeable Lysis capability/receipt and validate all
  issue economics at this boundary. Any crate can currently issue arbitrary Nods.
- Add a reentrancy proof/test for malicious asset and vault callbacks against the
  `settleNod` effects-before-interactions ordering.
- Put an explicit checkpoint/command guard around the complete mining orchestration
  or prove the outer EVM journal always includes CE overlay events and every
  in-process mutation on all production entrypoints.
- Define zero-cost settlement asset semantics in the ABI. Accepting any address,
  including zero, is implemented but not capability/version signaled. A zero-cost
  Nod still requires an explicit `settleNod` call before it can be mined.
- Pin and version PoW difficulty/preimage and specify whether old Nods retain their
  issuance-era difficulty after a protocol update.
- Validate nonzero Gratis load and cost/floor/entry/currency relationships before
  issuance; a zero-load Nod can currently be mined for a zero mint.
- Add nonzero-cost production tests using real ERC20 return variants, allowance,
  registered VaultRouter/vault, rollback at every step, malicious callbacks and
  exact balance/share conservation.
- Add replay/concurrency tests for two mining transactions targeting the same Nod
  and for re-execution after a reverted downstream Fidelity mutation.
