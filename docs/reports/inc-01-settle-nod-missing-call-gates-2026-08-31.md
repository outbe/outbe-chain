# INC-01: `settle_nod` accepts payment for a Nod that can never be mined

- **Status:** open, unfixed
- **Severity:** high — irrecoverable loss of user funds, reachable on the normal path
- **Date:** 2026-08-31
- **Scope:** `crates/core/nodfactory`, `crates/core/nod`
- **Related:** `ADR-C-NOD-002` (settlement and mining orchestration), `ADR-C-GEM-002`,
  `ADR-C-INX-002`

## Summary

`NodFactory::settle_nod` performs no lifecycle validation beyond an idempotence
guard. `NodFactory::mine_gratis` rejects a Nod whose bucket call notice has
lapsed, but `settle_nod` still accepts payment for that same Nod. The cost is
pulled from the payer and deposited into the reserve vault; there is no refund
path, and the Nod is subsequently deleted by the forfeit sweep.

The equivalent Gem and Intex paths both reject settlement past the deadline. Nod
is the only instrument missing the gate.

The exposure is not a narrow race. The forfeit sweep is rate-limited to 256 Nod
bodies per daily run and resumes on a cursor, so a Nod can remain past its
deadline — and settleable — for days. A correlated mass forfeit is the documented
expected shape of a call event, which is precisely when the window is widest.

## The divergence

`mine_gratis_inner` validates five conditions before burning the Nod:

| # | Condition | Error | Line |
|---|---|---|---|
| 1 | `caller == item.owner` | `NotOwner` | `nodfactory/src/runtime.rs:260` |
| 2 | valid proof-of-work | `InvalidPow` | `nodfactory/src/runtime.rs:263` |
| 3 | `bucket.is_qualified` | `NodNotQualified` | `nodfactory/src/runtime.rs:266` |
| 4 | `called_at == 0 \|\| now <= called_at + CALL_NOTICE_PERIOD` | `CallDeadlineExpired` | `nodfactory/src/runtime.rs:277` |
| 5 | `item.is_settled` | `NodNotSettled` | `nodfactory/src/runtime.rs:281` |

`settle_nod` validates one:

| # | Condition | Error | Line |
|---|---|---|---|
| 1 | `!item.is_settled` | `NodAlreadySettled` | `nodfactory/src/runtime.rs:126` |

`nod_api::settle_nod` delegates to `NodContract::record_nod_settled`
(`nod/src/state.rs:203`), which sets `is_settled = true` and writes the body. No
state gate exists anywhere on the path.

### Comparison with the sibling instruments

| Path | Owner check | State gate | Deadline gate |
|---|---|---|---|
| `settle_nod` | none | **none** | **none** |
| `settle_gem` (`gemfactory/src/runtime.rs:262`, `:267`, `:272`) | yes | `Qualified` or `Called` | yes |
| Intex `settle` (`intexfactory/src/runtime.rs:766`, `:774`) | holder or authorised settler | `Qualified` or `Called` | yes |

The absent owner check on `settle_nod` is intentional and documented — any address
may settle any live Nod, matching Credis third-party settlement. The absent
deadline gate is not.

## Loss path

`settle_nod_inner` (`nodfactory/src/runtime.rs:133`) executes, in order:

1. `nod_api::settle_nod(...)` — sets `is_settled = true`.
2. `IERC20::transferFrom(payer -> NOD_FACTORY_ADDRESS, cost_amount_minor)`.
3. `IERC20::approve(VAULT_ROUTER_ADDRESS, cost_amount_minor)`.
4. `outbe_vaultrouter::api::deposit(asset, cost)`.
5. Emits `NodSettled`.

After step 4 the cost is held as vault shares by VaultRouter. `nodfactory`
exposes no `unsettle`, no refund selector and no reversal of any kind. When the
sweep later reaches the Nod, `forfeit_members` (`nod/src/called.rs:211`) removes
the record outright without checking `is_settled` and without refunding, so the
only remaining trace of the payment is the `NodSettled` log entry.

The payer loses `cost_amount_minor` and receives nothing. No offsetting value is
created: `gratis_load_minor` is minted only by `mine_gratis`, which never runs.

## Exposure window

The window opens when `now > called_at + CALL_NOTICE_PERIOD` and closes when the
daily sweep deletes that specific Nod body.

Relevant bounds (`nod/src/constants.rs`):

| Constant | Value | Line |
|---|---|---|
| `CALL_LOOKBACK_DAYS` | 28 | `:24` |
| `CALL_BREACH_DAYS` | 21 | `:29` |
| `CALL_NOTICE_PERIOD` | 7 days | `:33` |
| `MAX_NOD_CALL_VISITS` | 4096 buckets per run | `:39` |
| `MAX_NOD_FORFEITS_PER_RUN` | **256 bodies per run** | `:45` |

The forfeit arm (`nod/src/called.rs:144`) draws from a per-run budget and stops
when it is spent; the cursor resumes the remainder on the next daily run. A call
is armed per **bucket** — every Nod sharing a `(worldwide_day, floor_price,
reference_currency)` key — and a sustained breach arms every bucket denominated in
that currency at once.

At 256 bodies per day, a population of 10,000 forfeitable Nods drains over
roughly 40 days. Every one of them is settleable for the whole of that period.

`MAX_NOD_FORFEITS_PER_RUN`'s own doc comment states this is the expected regime,
not a tail case:

> A correlated mass-forfeit is the expected shape of a call event, not a tail
> case, so the burst needs its own cap.

## Who is affected

Because `settle_nod` takes an arbitrary `payer`:

- an owner settling their own Nod without realising the bucket was called;
- any third-party settlement service, agent or bot paying on a user's behalf;
- any counterparty induced to settle a Nod chosen by an attacker.

There is no single read that answers "is this Nod still mineable?". The condition
spans the item body (`is_settled`), the bucket body (`is_qualified`) and the
`bucket_called_at` map, plus the notice arithmetic. A caller cannot cheaply
pre-check what the contract itself declines to check.

## Why it was missed

The guard in `mine_gratis_inner` carries this comment
(`nodfactory/src/runtime.rs:268`):

> Mining stays open during the notice period — that is what the notice is for.
> Past it the Nod is forfeit, and this check closes the gap before the daily
> sweep reaches it.

The lag between deadline and sweep was understood and closed on the mining path.
`forfeit_members` reasons about the same lag (`nod/src/called.rs:205`):

> A bucket holding more members than the budget resumes on the next run, which
> cannot change an outcome: the deadline has already passed and mining is closed,
> so nothing can rescue the remainder.

That conclusion is correct for mining and incorrect for settlement, which never
closed. Only one of the two user-facing paths was considered.

## Proposed fix

### 1. Deadline gate — required

Mirror check 4 into `settle_nod`, before `settle_nod_inner` moves any value:

```rust
let called_at = NodContract::new(storage.clone())
    .bucket_called_at
    .read(&item.body().bucket_key)?;
let now = storage.timestamp()?.to::<u64>();
if called_at != 0 && now > called_at.saturating_add(CALL_NOTICE_PERIOD) {
    return Err(NodFactoryError::CallDeadlineExpired.into());
}
```

Settling a Nod that can never be mined has no legitimate use. `CallDeadlineExpired`
already exists in `NodFactoryError`, so no new error variant is required. Both
sibling instruments revert in this state.

### 2. Qualification gate — decision required

Gem and Intex accept settlement only from `Qualified` (or `Called` within
notice). Nod accepts it in any state, including before its bucket has qualified.

This is a weaker hazard: an unqualified Nod may still qualify later, so a
pre-payment is not necessarily lost. It should nonetheless be an explicit choice
rather than an omission, and the chosen rule recorded in `ADR-C-NOD-002` so a
future instrument inherits it.

Note the tail case if pre-qualification settlement is retained: a callable bucket
is armed only at qualification (`nod/src/runtime.rs:50`, `insert_callable_bucket`).
A bucket that never qualifies can therefore never be called and never forfeited,
so a Nod settled before qualification whose floor is never reached leaves the
payment in the reserve vault indefinitely against a Nod that may never become
mineable. Stranded rather than lost, with no expiry on either side.

### Not in scope

Leave the arbitrary-payer rule as is. Third-party settlement is deliberate in
both Nod and Credis and is unrelated to this defect.

## Tests to add

1. `settle_nod` reverts with `CallDeadlineExpired` for a bucket whose
   `called_at + CALL_NOTICE_PERIOD` has elapsed and which the sweep has not yet
   reached.
2. `settle_nod` still succeeds inside the notice window, matching `mine_gratis`.
3. A forfeit sweep constrained by `MAX_NOD_FORFEITS_PER_RUN` leaves a residue of
   past-deadline Nods, and every one of them rejects settlement.
4. Structural test asserting that `settle_nod`, `settle_gem` and Intex `settle`
   agree on the deadline predicate, so the three cannot drift again.

## References

| File | Lines | Subject |
|---|---|---|
| `crates/core/nodfactory/src/runtime.rs` | 116–131 | `settle_nod`, sole guard |
| `crates/core/nodfactory/src/runtime.rs` | 133–190 | `settle_nod_inner`, value movement |
| `crates/core/nodfactory/src/runtime.rs` | 254–284 | `mine_gratis_inner`, full check sequence |
| `crates/core/nod/src/state.rs` | 203–217 | `record_nod_settled` |
| `crates/core/nod/src/called.rs` | 144–156 | budgeted forfeit arm |
| `crates/core/nod/src/called.rs` | 204–257 | `forfeit_members` |
| `crates/core/nod/src/constants.rs` | 24–45 | call and sweep bounds |
| `crates/core/nod/src/runtime.rs` | 44–62 | callable bucket armed at qualification |
| `crates/core/gemfactory/src/runtime.rs` | 254–276 | `settle_gem` gates |
| `crates/core/intexfactory/src/runtime.rs` | 764–776 | Intex settle gates |
