# ADR-C-GEM-002: GemFactory owns Gem issuance, settlement and Promis mining

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Gem protocol maintainers
- **Scope:** `crates/core/gemfactory`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-S-ORC-001, ADR-C-PRM-001, ADR-C-PRM-002, ADR-C-VLT-001, ADR-C-GEM-001
- **Related:** ADR-S-EMI-001 and the future daily-emission flow
- **Supersedes:** GemFactory portions of former broad pre-space Gratis/economic aggregate (previously numbered 030)

## Context

GemFactory maps authorized protocol outcomes to typed Gems, derives Oracle-priced
cost/floor parameters, accepts owner settlement into reserves and consumes Settled
Gems to mint Promis. It owns workflow authority and a small aggregate statistics
record; Gem owns identity/index state.

## Decision

### Issuance

Privileged internal callers mint a typed Gem for a nonzero owner. Factory requires
registered/reference currency support, reads the canonical COEN/issuance rate,
snapshots issue time and derives cost/floor with checked 18-decimal arithmetic.

Genesis Gems are born Qualified. Validator, Wallet, CCA and SRA Gems are born
Issued; SRA uses its configured cost coefficient. Merchant issuance is explicitly
rejected until designed. Cross-currency issuance is not supported by the current
single-rate path.

Factory calls Gem add, increments its own `total_gems_issued` statistic with checked
arithmetic and emits issuance in one frame.

### Settlement and mining

Only Gem owner may settle a Qualified Gem. Factory transitions to Settled, then if
cost is nonzero pulls the configured reserve asset from owner, approves and deposits
it through VaultProvider. Failure rolls the transition back.

Only owner may mine a Settled Gem. Factory verifies shared-scheme PoW over Gem id and
nonce, burns the Gem, mints exactly `gem_load` Promis through PromisFactory and emits
completion atomically.

## Authority and invariants

- Every mint caller is an explicitly authorized system/factory outcome and cannot
  select an unsupported type/currency policy.
- Derived entry/floor/cost and initial state match the type table exactly.
- `total_gems_issued` is monotonic issuance history and is not confused with live
  Gem supply.
- Successful cost-bearing settlement deposits the exact supported-asset amount and
  ends in Settled; failed payment leaves Qualified.
- Successful mining burns one owner Gem and mints exactly its immutable load as
  Promis/Fidelity; replay is impossible.
- Factory statistics such as parked Intex have a named writer and conservation
  meaning or are removed.

## Atomicity, replay and external trust

Each mint, settlement and mining call is one EVM transaction across Gem, Oracle,
ERC-20, VaultProvider, PromisFactory and events. Gem record/id prevents mint/burn
replay; consumed record and PoW input prevent a second mining result.

Oracle, reserve asset, ERC-20 and vault are external/configuration trust boundaries.
Rates are read at execution. Caller cannot choose reserve asset in the public
settlement call, but current `assetAt(0)` resolution is configuration-order dependent.

## Compatibility and evidence

Gem type table, coefficients, floor markup, scales, initial states, reserve asset
binding, PoW scheme and privileged caller set are activation-critical economics.

Inspected all runtime commands, checked pricing helpers, Oracle mapping reads,
VaultProvider calls and tests including genesis flow/ownership/state failures. Real
asset/vault behavior, caller closure, multi-currency rules and complete rollback
matrix remain unproven.

## Consequences

GemFactory is independently auditable as the economic workflow owner. Rewards and
other producers import a narrow mint seam; the daily reward saga belongs in a PFS.

## Rejected alternatives

- **Let caller supply rate/cost/floor:** issuance can be underpriced.
- **Mint Promis directly:** Fidelity coupling is bypassed.
- **Allow Merchant as an undocumented variant:** no accepted workflow exists.
- **Use first registered asset as permanent currency identity:** registry order can
  redirect settlement.

## Open questions and technical debt

1. Settlement uses `VaultProvider.assetAt(0)`. Bind each Gem's issuance/reference
   currency to an exact reserve asset and verify vault asset identity.
2. Current code comments say cross-currency is unsupported but do not visibly enforce
   `issuance_currency == reference_currency` after checking reference registration.
   Enforce or implement a chained rate.
3. Merchant flow is deferred; keep its enum uncallable and write a separate ADR/PFS
   before activation.
4. Enumerate all internal mint callers (Rewards and any agent factories), their
   allowed Gem types and one-time source/replay guards in structural tests.
5. Settlement pulls nominal amount without measuring actual ERC-20 balance delta.
   Reject fee-on-transfer/rebasing tokens or implement explicit delta economics.
6. Validate VaultProvider returned shares/nonzero reserve effect; current settlement
   ignores the returned value through the internal API.
7. Add failure injection after Gem transition, token pull/approval, vault deposit,
   Gem burn, Promis/Fidelity mint and events.
8. Pin PoW preimage/difficulty/domain and activation with independent reference
   vectors; confirm nonce range checks.
9. Prove Gem id uniqueness for multiple identical rewards in the same block; current
   owner/load/block derivation may collide.
10. Define and reconcile `total_gems_issued` and `total_intex_parked`; the latter's
    production semantics/writers were not closed in this inspection.
11. Add maximum load/rate/cost and floor arithmetic/domain bounds beyond checked
    multiplication.
12. Add production ABI/e2e flows for each enabled type, settlement, mining, replay,
    token/vault failure and restart.
13. Human economics review is required for coefficients, markup, genesis exception
    and 1:1 Gem-load-to-Promis conversion.
