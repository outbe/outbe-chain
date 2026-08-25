# ADR-C-CRD-002: CredisFactory owns collateral-proof, pricing and settlement orchestration

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17 (revised 2026-08-19 for the daily price-path scan)
- **Decision owners:** Credis protocol maintainers
- **Scope:** `crates/core/credisfactory`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-C-GRT-001, ADR-C-GRT-003, ADR-C-CRD-001, ADR-C-VLT-001, ADR-S-ORC-001, ADR-S-CYC-001
- **Related:** ADR-C-GRT-002, ADR-C-FID-001
- **Supersedes:** CredisFactory portions of former broad pre-space Gratis/economic aggregate (previously numbered 030)

## Context

CredisFactory turns one sealed pledged-Gratis ticket into a credit position and
reserve disbursement, then turns settlements into vault inflows and proportional
collateral releases. It is the atomicity owner across Gratis, Oracle, Credis, external
asset contracts and VaultRouter. It does not own any of those modules' state.

## Decision

### Request Credis

`requestCredis` derives the borrower from a nonzero smart account and performs:

1. validate nonzero asset/bundle and a Credis-eligible denomination;
2. reject a bundle holding any unresolved called Credis position;
3. verify the GratisPool spend proof bound to the bundle, action, chain and zero
   context nonce, consuming the nullifier;
4. convert the denomination's 18-decimal Gratis amount into six-decimal stable
   amount using the pinned `COEN/840` Oracle rate and explicit decimal gap;
5. staticcall the selected asset's `isoCode()` and pin the currency's annual official
   policy rate, scaled by the policy-rate factor;
6. create the Credis position using the nullifier as unique identity input;
7. persist the original denomination for reclaim derivation; and
8. withdraw exactly the position asset/amount through VaultRouter into the bundle.

The caller cannot redirect a copied proof because receiver binding uses the bundle.
All steps and the success event are one EVM rollback domain.

### Settle

Any caller may settle, including on behalf of another account: value is pulled from the
caller's own balance while the freed collateral is always released to the original
pledger, so a payer can never redirect value to themselves. A position is settleable
from the moment it opens; only a terminal position reverts, as closed. Before mutation
the factory validates the position and asset and rejects a zero amount. It then:

1. applies the settlement in Credis at canonical time, interest first and principal
   second, rejecting any amount below the accrued interest;
2. pulls exactly `interest + principal covered` from the caller;
3. approves and deposits it through VaultRouter; and
4. releases the proportional collateral share to the pledger recovered from the
   position's sealed ciphertext via a `RevealOwner` enclave round-trip.

`settle` returns the split — `(principal, interest)` — rather than a single total.
A caller reconciling a payment needs to know how much of it retired debt and how much
was the coupon; recovering that from one number would mean re-deriving the interest
from the position's accrual anchor, which races the settlement that just moved it.
Their sum is what was pulled.

Failure at any external call rolls back the position bookkeeping and the release.

### The daily price-path scan

`called::scan_and_call` is the `credis_call_daily` Cycle trigger (id 7, period 86_400,
no accounting gate). It walks the credis active-position index — the non-terminal
positions only — from a persisted cursor, and applies up to two transitions per
position in lifecycle order:

1. `Open -> Called` when the finalized daily reference price sat at or above the call price on
   `CALL_BREACH_DAYS` of the trailing `CALL_LOOKBACK_DAYS` closed days. Counting
   breach days rather than requiring a run means the window absorbs the difference in below-call
   or unpublished days, and needs no per-position streak state: the count is recomputed
   from oracle history every run. A day that predates the position ends the count, so no
   position inherits a breach run from before it existed. Same shape as
   `outbe_gem`'s `CALL_WINDOW` / `CALL_THRESHOLD` pair.
2. `Called -> Void` when the settlement window has lapsed with principal outstanding:
   burn the unpaid collateral share, drop the pledger's fidelity cohort, credit the Promis
   Reserve.

Ordering is structural, not incidental: the Oracle finalizes the closed day's VWAP in a
pre-execution hook, and `CycleTick` is a body transaction in the begin zone, so the scan
always reads the current block's finalization. A watermark behind the last closed day
means that ordering broke, and the run skips loudly rather than misreading an unfinalized
day as one with no published price.

The handler is total with respect to market data. A Cycle handler error propagates out of
the `CycleTick` system transaction and fails the block, so an unregistered pair, an
unpriced currency and an unfinalized day each degrade to "no transition".

**Error isolation is deliberately split.** The call arm is pure storage and
arithmetic, so each position runs inside its own checkpoint and a deterministic error
skips just that position — one bad position can never halt the run. The void arm is *not*
isolated: it makes two TEE enclave round-trips, whose faults (sidecar unavailable, socket
timeout, poisoned global connection mutex) are node-local rather than a function of
committed state. Swallowing one would leave one validator's state root diverged from the
rest with no block failure to catch it, so the void propagates and fails the block
instead. The three domain errors the void can raise are fully pre-filtered by its guard,
so nothing deterministic reaches that path.

## Cross-module invariants

- One consumed pledge nullifier opens at most one position.
- Position terms use the exact asset/currency/rate and amount delivered.
- Bundle token increase equals recorded principal under the supported token policy.
- Each successful settlement collects the accrued interest in full before any
  principal, deposits exactly what it pulled, and releases collateral proportional to
  the principal it covered.
- Live unreclaimed position collateral is backed by pledged Gratis escrow.
- Reclaim notes cannot be redirected, duplicated or created with an unverifiable
  denomination.

The last two are intended invariants that current implementation does not yet prove.

## Failure, replay and external trust

User/proof/authorization/rate/liquidity errors revert. Oracle values and asset ISO
are snapshotted for later determinism. ERC-20 and VaultRouter are adversarial
external-call boundaries: return data, actual balance deltas and asset identity must
be validated according to supported-token policy.

Pool nullifier, position id and the position's terminal states are replay guards. A node restart
uses canonical EVM state; no local cache may authorize a request/payment.

## Compatibility and evidence

Action tags, proof inputs, denomination ladder, ISO ABI, decimal scales, conversion
formula, position identity and reclaim rule are consensus/proof formats. Inspected
both runtime commands, ABI dispatch, Oracle/staticcall seams and tests. No full
production-interface credit lifecycle or failure-injection matrix exists.

## Consequences

CredisFactory presents two business commands while hiding proof/pricing/vault
choreography. Credis and VaultRouter remain separately auditable state owners.

## Rejected alternatives

- **Let caller supply rate/currency:** obligations become manipulable.
- **Create position after transferring liquidity without rollback:** failed state
  persistence could make an untracked loan.
- **Use transaction sender instead of bound bundle blindly:** smart-account credit
  ownership would be wrong.
- **Accept opaque reclaim forever:** valid repayments can strand collateral.

## Open questions and technical debt

1. Opening a position consumes a pool nullifier but does not visibly reserve or
   decrement per-account Gratis pledge accounting. Add a position-to-escrow
   reservation and prove aggregate backing.
2. Reclaim commitment denomination is opaque and can be wrong yet accepted. Add a
   verifiable denomination-bound insertion proof.
3. Define relationship between transaction caller and smart account; `_caller` is
   currently unused on request, so any relayer can submit a bundle-bound proof.
4. Decide and enforce early-payment policy imported from ADR-C-CRD-001.
5. Use actual ERC-20 balance deltas or explicitly reject fee-on-transfer/rebasing
   assets for disbursement and repayment.
6. Validate that asset ISO, Oracle settlement pair, VaultRouter vault asset and
   token decimals describe the same economic currency.
7. Hard-coded six-decimal conversion and `COEN/840` symbol require a versioned
   multi-currency/decimal design.
8. Add failure injection after ticket consumption, position creation, denomination
   write, vault withdrawal, settlement application, token pull/deposit and collateral
   release.
9. Add ABI-level proof replay/front-running/redirection and restart tests matching
   PFS-003.
10. Define allowance reset/nonstandard ERC-20 safe-call policy.
11. The factory stores `position_denom` separately from Credis. Prove one-to-one
    closure and decide cleanup/retention on completed positions.
12. Add maximum loan, rate, multiplication and decimal-conversion bounds.
13. Define behavior when Oracle data changes between mempool admission and block
    execution; execution snapshot is authority.
14. Production deployment must structurally prove CredisFactory is registered as
    the correct VaultRouter source/target type.
15. **Resolved 2026-08-19** — the `credis_call_daily` scan arms the call, so the void
    path and the unresolved-call guard on `requestCredis` are both reachable in
    production. `has_called_position` is now an O(1) counter read rather than a walk.
16. **Obsolete 2026-08-24** — settlement is no longer gated on a price latch, so there is
    nothing for a crossing to unlock.
18. One run voids at most `MAX_CREDIS_VOIDS_PER_RUN` (64) positions, because each void
    costs two blocking enclave round-trips on a process-global connection. A sustained
    breach calls a whole currency's book at once and it lapses together 14 days later, so
    a backlog larger than the budget drains over several days. Spending the budget only
    declines further voids — the pass continues, so the call arm keeps its
    full `MAX_CREDIS_DAILY_VISITS` reach and a void backlog cannot throttle it (both
    arms share one cursor, so ending the pass there would have). Raise the budget
    once real sidecar latency is measured, or batch the round-trips.
19. Only `COEN/840` is registered at genesis and `register_pair` has no precompile entry
    point, so a position in any other issuance currency has no daily series and can never
    be called through the scan. Live-config limitation; the scan degrades
    correctly.
17. The originating agent is recorded as `cca` on the position and in `CredisRequested`
    but is not authorized at origination or penalized at void; the CCA program is not yet
    implemented.
