# Outbe Divergence Register

- **Verified against:** `main` at `296e8375`
- **Date:** 2026-09-04
- **Scope:** `crates/core`, `crates/system`, `contracts/precompiles`
- **Companion:** `inc-01-settle-nod-missing-call-gates-2026-08-31.md` (INC-01 in full)

Twenty-five findings between stated design intent, the documentation, and the
Rust implementation, found while mapping the protocol onto a farming model, then
checked against `main` at `296e8375` in two passes: the original list
re-verified, and the forty-two commits since read side by side for what they
changed.

Method: every claim is read from source under `crates/` and `contracts/`.
Identifiers are stable; a finding keeps its number after it closes. The
forty-two commits closed most of the original list, restructured settlement,
replaced the emission curve, and deleted the ADR corpus that four findings were
about. With the ADRs gone the code's only reference is itself, so the second
pass compares the four call instruments, the daily pipeline, Fidelity and the
payment rails against each other.

| Open | Need a decision | Latent or documentation | Resolved on main | Superseded or moot |
|---|---|---|---|---|
| 9 | 2 | 3 | 5 | 6 |

---

## A · Still open

Nine. Two move value today; the rest are rules that differ between instruments
with no stated reason.

### INC-17 · Nod qualification reads a rate of any age; Gem and Intex require one under six hours old

| Nod | Gem · Intex |
|---|---|
| `coen_rate_for_opt`: the stored rate, no timestamp check | `fresh_coen_rate_for_opt`: rejected unless published within `FX_RATE_MAX_AGE_SECONDS`, six hours |

The freshness constant's own doc names "economic transaction paths and
qualification hooks" as its users. Nod's hook is the one that does not use it.
Qualification there is a one-way latch: once a bucket qualifies it stays
qualified, its call price is snapshotted and its call clock is armed. A rate
that went stale above the floor qualifies buckets a live rate would leave
waiting, permanently.

- `crates/core/nod/src/hooks.rs:103` · `nod/src/hooks.rs:9–10` (the latch)
- `gem/src/hooks.rs:63` · `intexfactory/src/qualified.rs:74`
- `crates/system/oracle/src/api.rs:135` vs `:156–192` · `oracle/src/constants.rs:13–14`

### INC-19 · A PayNote that over-covers its Nod keeps the difference in the vault

`mineGratis` checks one thing about the amount: a spend below the cost is
rejected. Anything at or above it is consumed whole. The tokens behind the note
went into the reserve vault when the note was created; the spend records a
nullifier and whatever change the proof itself carved out, and nothing returns
the gap between spend and cost to the spender. The ABI invites exactly that gap:
"covering at least `costAmountMinor`".

| Nod | Gem · Credis |
|---|---|
| any spend ≥ cost accepted · the surplus stays in the vault, unclaimed | Gem pulls exactly the cost · Credis consumes only what the position needs and never over-pulls |

Avoidable by a spender who sets the spend to the exact cost, which the ABI does
not tell them to do.

- `crates/core/nodfactory/src/runtime.rs:281–293` · `paynote/src/runtime.rs:159` (deposit) · `:283–291` (spend)
- `contracts/precompiles/src/INodFactory.sol:72` · `credis/src/runtime.rs:224` · `gemfactory/src/runtime.rs:335–346`

### INC-15 · Credis settlement has no deadline gate; the other three close at the deadline

`settle` accepts a position in `Open` or `Called` without looking at the clock.
The only thing that ends a called position is the daily void sweep, budgeted at
64 positions per run. The constants doc says the window "lapses"; the settle
path never checks that it has. Nod, Gem and Intex all revert once the notice
has passed, on the sweep and on the user-facing call alike.

| Credis | Nod · Gem · Intex |
|---|---|
| settleable on unchanged terms until the sweep reaches it · 64 voids per day | `CallDeadlineExpired` / `DeadlineExpired` the moment `now > deadline` |

The mirror image of INC-01. There, a dead instrument still took payment; here,
the instrument stays alive past its deadline. Value moves the other way:
collateral that would have gone to the pool is reclaimed by whoever settles
late, and a large call event makes "late" weeks long (INC-21).

- `crates/core/credis/src/runtime.rs:209–217` · `credis/src/constants.rs:26–28` · `credisfactory/src/called.rs:53–55`
- `nodfactory/src/runtime.rs:201–203` · `gemfactory/src/runtime.rs:287–289` · `intexfactory/src/runtime.rs:773–775`

### INC-14 · Credis counts a breach at or above the call price; Nod, Gem and Intex count strictly above

| Credis | Nod · Gem · Intex |
|---|---|
| `value >= position.call_price` | `value > call_price` |

Each side documents its own rule, and the Credis constants say the pair
"mirrors gem's". A day that closes exactly on the call price is a breach day
for Credis and not for the other three. Rare, but a consensus rule, and the
only place the four breach scans differ.

- `crates/core/credisfactory/src/called.rs:258` · `credis/src/constants.rs:22–23`
- `nod/src/called.rs:187` · `gem/src/runtime.rs:70` · `intexfactory/src/called.rs:318`

### INC-16 · Call terms are read live for Nod and Credis, and snapshotted for Gem and Intex

| Gem · Intex | Nod · Credis |
|---|---|
| window, threshold and notice written into the record at issuance: "a later change cannot re-term a live gem" | nothing stored per bucket or position: `CALL_LOOKBACK_DAYS`, `CALL_BREACH_DAYS`, `CALL_NOTICE_PERIOD`, `CALL_WINDOW_SECS` read at check time |

Any retune re-terms every live Nod bucket and Credis position, called ones
included, and leaves every live Gem and Intex series on the terms it was issued
with. This is the trap under INC-12: shortening the Credis window binds
positions already in their notice period the moment it activates. Gem states
the snapshot as an invariant; Nod's constants make no claim either way, and the
code could not honour one.

- `crates/core/gem/src/api.rs:44` · `gem/src/runtime.rs:43–45` · `intexfactory/src/runtime.rs:66–70`
- `nod/src/called.rs:145, 183, 191` · `nodfactory/src/runtime.rs:201` · `credis/src/runtime.rs:92`

### INC-12 · The Credis notice period is 14 days where the other three are 7

| Nod · Gem · Intex | Credis |
|---|---|
| `CALL_NOTICE_PERIOD = 7 * 24 * 3600` | `CALL_WINDOW_SECS = 14 * 24 * 60 * 60` |

Intent is a uniform seven-day notice. The 21-of-28-day breach rule and the
markup semantics match across all four instruments; the notice length is the
one parameter that differs without a stated reason.

- `crates/core/credis/src/constants.rs:28`

**Ship it carefully.** Credis reads the constant live (INC-16):
`settlement_deadline(position) = called_at + CALL_WINDOW_SECS`. Halving the
value applies retroactively to every open position, including ones already
called, and could put a live position past its deadline the moment it
activates. Snapshot the window per position first, or gate the new value to
positions opened after activation.

### INC-20 · Nod and Gem settle against different asset allow-lists, from different sources of truth

| Nod | Gem |
|---|---|
| the assets the VaultRouter registers under the Nod's reference currency, and only those | any asset with a registered vault whose own `isoCode()` equals the reference *or* the issuance currency |

Two differences. Gem accepts payment in the issuance currency, Nod does not.
And Nod trusts the router's currency index while Gem trusts the asset's
self-reported code, so an asset whose code disagrees with the currency its
vault was registered under passes one gate and fails the other.

- `crates/core/nodfactory/src/runtime.rs:303–311` · `gemfactory/src/runtime.rs:385–405`

### INC-18 · Nod's breach window starts on the worldwide-day key; the window itself is in UTC days

The scan builds its window from plain UTC date keys and cuts it off at
`start_day`, which it reads from `bucket_worldwide_day`, a UTC+14 key. For
fourteen hours of every day the two keys differ by one. The comment above the
window builder says the call counts plain UTC days and "NOT the UTC+14
`WorldwideDay` key"; the cutoff fifty lines below uses that key. Gem, Intex and
Credis all cut off at the UTC day of issuance.

Effect: whether day one of the twenty-eight counts, for a Nod, depends on the
clock at bucket formation. One day, one instrument, deterministic, and a
consensus rule.

- `crates/core/nod/src/called.rs:72–74, 126, 184` · `crates/blockchain/primitives/src/time.rs:23, 61–63`
- `gem/src/runtime.rs:63` · `intexfactory/src/called.rs:354` · `credisfactory/src/called.rs:252`

### INC-21 · The same call event clears in hours for Intex, days for Gem, and weeks for Nod and Credis

| Instrument | Call sweep | Forfeit / void budget | Resume across currencies |
|---|---|---|---|
| Nod | once a day · 4 096 buckets visited | 256 per day | none: every block starts again at the first currency |
| Gem | daily; an unfinished slice continues every block | 256 per run | cursor |
| Intex | daily; an unfinished slice continues every block | 256 per block (expiry) | cursor |
| Credis | once a day · 4 096 positions visited | 64 per day | single cursor |

Same job, four throughputs. Nod's module doc says each currency "resumes from
its own per-bin cursor"; the bin cursors exist, the currency cursor that Gem
and Intex keep does not, so a heavy first currency is served first every block.
The Credis void budget is what stretches INC-15 from a day into weeks.

- `crates/core/nod/src/called.rs:49` · `nod/src/hooks.rs:28–31, 77–79, 99–108` · `nod/src/constants.rs:39, 45`
- `gem/src/hooks.rs:29–31` · `gem/src/schema.rs:163, 176` · `gem/src/constants.rs:17`
- `intexfactory/src/qualified.rs:36–44` · `intexfactory/src/schema.rs:79, 81` · `intexfactory/src/constants.rs:55`
- `credisfactory/src/called.rs:39, 55, 102–105`

---

## B · Needs a decision

Neither is a defect. Each is a fork the code has already taken one branch of.

### INC-13 · Nod pays through PayNote at mining; Gem and Intex still pay through a separate settle step

The PayNote change (#314) folded the Nod's payment into `mineGratis`: the
caller passes a spend proof, `discharge_cost` consumes it after the owner, PoW,
qualification and deadline checks, and paying and mining are one transaction.
Gem and Intex were not moved. Both keep `settleGem` / `settle` as an ERC-20
pull into the vault, followed by a separate `minePromis`.

| Nod | Gem · Intex |
|---|---|
| one call · shielded bearer note · spender bound to caller · no paid-but-unmined state | two calls · public ERC-20 transfer · a settled-but-unmined state exists and can be forfeited |

Two payment rails for the same economic act. The Nod rail is strictly better on
every axis the register has cared about: it cannot pay for a dead instrument,
it cannot strand a paid one, and the payer is private. Whether Gem and Intex
are meant to follow is a decision worth recording either way.

- `crates/core/nodfactory/src/runtime.rs:262` (`discharge_cost`)
- `crates/core/gemfactory/src/runtime.rs:270` · `intexfactory/src/runtime.rs:748` (settle steps retained)

### INC-22 · The CCA's matching COEN survives #348 as a pass-through to the borrower

`requestCredis` is still payable and still requires `msg.value` to equal the
pledged collateral exactly, in native COEN. What #348 removed is the escrow,
the release and the burn. The COEN now goes straight to the borrower's smart
account as unrestricted balance, in the same transaction that withdraws the
stablecoin principal from the vault to that account, and it never comes back.
The ABI says so, and three tests pin it.

| Borrower receives | Borrower repays |
|---|---|
| the principal in stablecoin from the vault, plus COEN equal to the collateral from the CCA | the principal plus interest, in stablecoin; the COEN is theirs to keep, settled or voided |

If the intent behind removing the stake was the payment itself, this is open.
If the CCA is meant to fund the borrower's COEN and be paid for it from the CCA
emission sink, this is closed and only the names are residue (INC-25).

- `crates/core/credisfactory/src/runtime.rs:101–105, 143–147` · `credisfactory/src/errors.rs:20`
- `contracts/precompiles/src/ICredisFactory.sol:24–27` · `credisfactory/src/tests/e2e.rs:539, 556, 587`

---

## C · Latent or documentation

Nothing here moves a balance today. Each costs something the first time someone
edits the code around it.

### INC-23 · Three terminal dispositions close a day without retiring its tribute partition; two of them without the failure receipt the terminal path requires

`EmptyTributeDay` retires the sealed partition. `ZeroDayLimit`,
`UnknownDayType` and `ZeroGratisAllocation` commit the day's transition and
return the limit to the pool without touching it, and the first two land the
day in `Failed` while the terminal module treats a failed day with no failure
receipt as storage corruption.

Latent rather than live: the emission floor is 2^26, so a zero day limit cannot
occur; the day type resolves to RED on missing data, so `Unknown` does not
reach settlement in the normal flow; and a zero allocation needs a day whose
whole nominal is a few minor units. But three modules hold three views of what
a failed day looks like.

- `crates/core/metadosis/src/settlement.rs:202–271` · `metadosis/src/reducer.rs:222–224` · `metadosis/src/terminal.rs:163–170`
- `crates/system/emissionlimit/src/day_emission.rs:18` · `metadosis/src/lifecycle.rs:431–439`

### INC-24 · A PayNote deposit trusts the requested amount; a Gem settlement measures what arrived

The note pool transfers `amount`, approves `amount`, deposits `amount`, and
derives the commitment from `amount`. Gem reads its balance before and after
the pull and deposits the difference. For any asset that takes a fee on
transfer, the pool mints a note for value it never received. Nothing registered
today does that; the two rails still disagree on whom to trust.

- `crates/core/paynote/src/runtime.rs:120, 134–159` · `gemfactory/src/runtime.rs:335–346`

### INC-25 · Names and comments that describe code that is no longer there

None of these change a balance. Each will mislead the next reader.

- **Stake.** `CcaStakeMismatch`, a parameter still called `stake`, "credis
  escrow" in the Gratis API doc, and a `Void::unpaid_share` field documented as
  scaling "the originating CCA's penalty" that nothing reads.
  `credisfactory/src/errors.rs:20` · `credisfactory/src/runtime.rs:54` ·
  `gratis/src/api.rs:36` · `gratisfactory/src/runtime.rs:4–5` ·
  `credis/src/runtime.rs:76–78, 324, 350`
- **Call constants.** `CALL_RATE_PCT` is 256 in Nod and 64 in Credis;
  `CALL_RATE` is 128 in Gem and Intex; `CALL_WINDOW` is the 28-day lookback in
  Gem and Intex while `CALL_WINDOW_SECS` is the 14-day settlement window in
  Credis; `PRICE_RATE_DEN` is a named constant in Credis and Intex and a bare
  `100` in Nod and Gem. The tail of INC-10.
- **Load sizing.** `F_FP_DEFAULT` (32%) and `F_MAX_FP` (64%) are read only by
  tests. Production derives the fraction as the day's allocation over the
  day's nominal, which is 32% only when demand binds on a GREEN day and 4% on
  a RED one; the test prose calls the same constants 8% and 16%.
  `lysis/src/constants.rs:11–13` · `lysis/src/program_v1/execute.rs:423–425` ·
  `lysis/src/tests.rs:506–518`
- **Credis scales and rounding.** `policy_rate` is documented at 1e18 and
  computed at 1e6; interest days are documented as "rounded up" and are
  floored, with the amount ceiled instead. `credis/src/runtime.rs:35, 101–121`
- **Gem ABI.** `IGem.sol` declares `transferFrom`, `safeTransferFrom`,
  `approve` and `setApprovalForAll`; every one reverts `NonTransferable`. Its
  `GemData` carries no call price, call time or notice, where the Nod, Intex
  and Credis structs do. `contracts/precompiles/src/IGem.sol:5–24` ·
  `gem/src/precompile.rs:47–49`
- **Credis ABI.** `hasCalledPosition` reads as a standing check; the
  implementation calls it "informational only". `ICredis.sol:92–93` ·
  `credis/src/runtime.rs:364–365`
- **PayNote ABI.** `IPayNote.sol` declares five custom errors; the
  implementation reverts with strings and never emits any of them.
  `IPayNote.sol:18–26` · `paynote/src/errors.rs:53`
- **Metadosis and emission docs.** `ReferenceCurrencyUnpriced` describes a
  worldwide-day price fallback the oracle marks "no longer produced"; the
  emission dispatcher's module doc counts six sinks over a table of five and
  promises `Fatal` where the code returns `Revert`; `metadosis/src/constants.rs`
  carries a second `UTC_PLUS_14_OFFSET` that nothing reads.
  `IMetadosis.sol:35–38` · `oracle/src/api.rs:28–29` ·
  `emissionlimit/src/block.rs:9–11, 32, 44–46` ·
  `emissionlimit/src/allocation.rs:51` · `metadosis/src/constants.rs:85`
- **Intex.** The Intex test module defines `CALL_NOTICE_PERIOD` as 21 days;
  `mark_called` accepts `Issued` or `Qualified` and names `Qualified` as the
  only expectation when it rejects. `intex/src/tests.rs:15` ·
  `intex/src/api.rs:97–101`
- **Gratis.** The factory's module doc describes a `mine` function; the
  function is `mint`. The Fidelity eligibility gate tests for league
  `u16::MAX`, a value the league formula cannot produce, under a `todo`.
  `gratisfactory/src/runtime.rs:5–7, 106–108, 129`

---

## D · Resolved on main

Each names the commit that closed it.

### INC-01 · `settle_nod` accepted payment for a Nod that could never be mined

**Closed by restructuring, not by a gate.** `settle_nod` no longer exists.
Payment moved inside `mineGratis` as a PayNote spend, consumed *after* the
`CallDeadlineExpired` check, so there is no longer any state in which a cost
can be paid against a Nod that cannot be mined. The residual hazard raised
later, settled in good time and then forfeited unmined, is closed by the same
change, since no paid-but-unmined state exists.

- #314 feat: paynote: "replace settlement logic with Paynote-based payment model and remove is_settled field"

### INC-02 · Nod call price was a multiple where the others were markups

`nod::CALL_RATE_PCT` is now applied as `entry × (100 + 256) / 100` =
**3.56×**. The ladder reads 64 / 128 / 256 growth as intended, and the
constant's doc comment now says "markup".

- `crates/core/nod/src/constants.rs:18` · `nod/src/runtime.rs:52`

### INC-03 · Forfeited Nod and Gem load was not routed to carry-over

All three instruments now credit `PromisLimit` on forfeit. Intex and Gem expiry
return their load as well.

- #337 Nod: `nod/src/called.rs:270`
- #319 Gem: `gem/src/runtime.rs:102` · `gemfactory/src/expired.rs:79`
- #317 Intex: `intexfactory/src/expired.rs:96`

### INC-04 · The auction had no consumption ceiling

```
auction_base = min(nominal_total, day_limit) − lysis_budget
```

`RequestBudgetSplit::derive` now takes `nominal_total`, so a day can never
auction more than its own claims less what the farmers took as seed, and the
empty-day case no longer briefs the whole limit.

**One thing to confirm.** This is the *day-limit* reading of the ceiling. The
reading chosen in discussion was the *pool* reading:
`min(nominal − lysis, PromisLimit.total_unallocated)`. The pool is still
drained into `day_limit` at formation, so the two only coincide when the day
limit binds. If the pool reading is still the intent, this is closed on the
wrong branch.

- #321 fix(metadosis): issue the day's nominal instead of the whole day limit: `metadosis/src/ocomp_budget.rs:37`

### INC-10 · Two constants named `CALL_RATE_PCT` used different formulas

Both are markups now, so the semantic collision is gone. The names still differ
across modules (`CALL_RATE` in Gem and Intex, `CALL_RATE_PCT` in Nod and
Credis) but every one of the four means the same thing.

---

## E · Superseded or moot

Closed by a decision that went another way, or by the thing they were about
ceasing to exist.

### INC-05 · The pool-backed auction ceiling collided with the pool drain

The ceiling was implemented against the day limit rather than the pool (see
INC-04), so the drain conflict never arises. `checked_take_carry_over` still
zeroes the pool into `day_limit` at formation, unchanged. This item reopens
only if INC-04 is reworked to the pool reading.

### INC-06 · Two ADRs said Promis→Gratis preserved age; the code created a fresh cohort

The ADR corpus was deleted (#322). The behaviour stands: `PromisFactory.mineGratis`
mints a fresh cohort, and the ABI now documents it: "records a fresh Fidelity
acquisition cohort, exactly as any other gratis acquisition does." The
conversion also moved from GratisFactory to PromisFactory and was renamed.

- `contracts/precompiles/src/IPromisFactory.sol:26` · `crates/core/promisfactory/src/runtime.rs:69`

### INC-07 · Nod and Gem call lifecycle undocumented in the ADRs

ADR corpus deleted (#322). No documentation remains to be stale.

### INC-08 · Merchant gems documented as rejected, shipped in full

ADR corpus deleted (#322). The model itself was renamed in passing:
`mintGemPosition` / `mintMerchantGem` are now `issueGemPosition` / `issueGem`
(#329).

### INC-09 · Emission sink table: six in the ADR, five in the code

ADR corpus deleted (#322); the five-sink table is unchanged. Separately, the
curve those sinks divide was replaced (#347): it now rises from 2^28 COEN to a
peak near 8.7 × 2^26 around day 1024, then falls to a 2^26 floor at day 3072,
on two logistic phases. The earlier exponential decay from 2^30 no longer
describes anything.

- `crates/system/emissionlimit/src/day_emission.rs`

### INC-11 · Settlement authority differed across all four instruments

The Nod row of that table no longer exists: there is no Nod settle call.
Payment is a bearer note whose proof names its spender, and `mineGratis`
requires spender = caller = owner. Anyone may fund the note; only the owner
may spend it. Gem and Intex are unchanged. What remains of the asymmetry is
INC-13.

---

## What is left

Nine open items. Two move value today: a Nod bucket can qualify on a stale rate
and stay qualified (INC-17), and a PayNote that over-covers its Nod leaves the
surplus in the vault (INC-19). Five are rules that differ between instruments
with no stated reason (INC-12, 14, 15, 16, 20); two are calendar and cadence
(INC-18, 21). Two decisions: whether Gem and Intex follow Nod onto PayNote
(INC-13), and whether the CCA's matching COEN is meant to reach the borrower
(INC-22). INC-04 closed on the day-limit reading and still wants a one-word
confirmation. The latent and documentation items (INC-23 to 25) cost nothing
until someone touches the code around them.

First pass surfaced eleven. The re-check against `main` at `296e8375` closed
nine and added INC-13. The sweep of the forty-two commits at the same head added
INC-14 to INC-25; the daily split, the emission curve and sinks, the pool
accounting, Fidelity's mutation sites, the Promis-to-Gratis conversion, the
StableFactory bridge, agent rewards and contributor payouts all checked out
against each other.
