# Outbe Divergence Register

- **Verified against:** `main` at `d84ebef1`
- **Date:** 2026-09-27
- **Scope:** `crates/core`, `crates/system`, `contracts/precompiles`, `contracts/intex`
- **Companion:** `inc-01-settle-nod-missing-call-gates-2026-08-31.md` (INC-01 in full)

Thirty-seven findings between stated design intent, the documentation, and the
Rust implementation, found while mapping the protocol onto a farming model, and
verified against `main` at `d84ebef1`.

Method: every claim is read from source under `crates/` and `contracts/`, and
every status names the pull request that set it. Identifiers are stable; a
finding keeps its number after it closes. The 125 commits since `296e8375`
closed most of the open list, split the Nod's payment back out of mining behind
gates that keep it payable only while mineable, moved the auction onto the
pool, and added a CCA registry. The four call instruments, the daily pipeline,
Fidelity and the payment rails were read side by side against each other.

| Open | Need a decision | Latent or documentation | Resolved on main | Superseded or moot |
|---|---|---|---|---|
| 9 | 3 | 4 | 16 | 5 |

---

## A · Still open

Nine. One blocks a product outright and one can abort block building; the rest
are rules that differ between instruments, or between the code and what it
documents.

### INC-26 · Credis cannot be opened: its pledge is priced by an oracle stub that always returns nothing

| Pledge | Oracle |
|---|---|
| `previous_half_open_8hours_vwap(…)` → `.ok_or(PledgePriceUnavailable)?` | "The period implementation is pending; currently returns `None`." Body: `Ok(None)` |

Every `pledgeGratis` reverts, and `issueCredis` can only consume a ticket that
`pledgeGratis` produced, so no Credis position can open. Nod, Gem and Intex all
take their entry price from a finalized previous-day VWAP.

Knock-on: CCA reward weights are written only when a position opens, so the
CCA emission share finds no weight and returns to Metadosis every day. The
stub's doc also gives an 18-decimal scale, where every other COEN/currency VWAP
the oracle serves is six-decimal. Worth settling when it is implemented.

- `crates/core/gratisfactory/src/runtime.rs:92–98` · `crates/system/oracle/src/api.rs:155–171`
- `crates/core/ccaregistry/src/emission_sink.rs:31–33` · #475 (stub) · #477 (wired into the pledge)

### INC-27 · The pool is promised when a day is requested but drawn when it activates, and a short draw aborts block building

| Request | Activation |
|---|---|
| caps the auction at `min(nominal − Lysis limit, pool + today's credit)`, reading the pool without reserving it | draws exactly that amount; any shortfall is `OcompLimitReceiptMismatch` → storage corruption → `PrecompileError::Fatal` |

Metadosis sets no cap on OCOMP jobs in flight, so a later day's request can
count pool balance an earlier day was promised and has not yet drawn, and
whichever day activates second finds the pool short. The doc on the draw
anticipates the case and says it "fails the activation … and the day's whole
emission returns to the accumulator". The code raises a fatal error on the
result-vote path, which passes fatal errors through, and a fatal error there
aborts the payload build. Reached only when an OCOMP job outlives the next
day's request.

- `crates/core/metadosis/src/ocomp_limits.rs:54–66, 111, 136–157` · `metadosis/src/errors.rs:75–86, 94–95`
- `metadosis/src/ocomp/activation.rs:389` · `metadosis/src/ocomp/vote.rs:165` · `metadosis/src/constants.rs:64–67` · #362

### INC-23 · Three terminal outcomes close a day without retiring its tributes, two of them without the failure receipt the terminal path requires

`EmptyTributeDay` retires the sealed tribute partition. `ZeroDayLimit`,
`UnknownDayType` and `ZeroGratisAllocation` commit the day's transition and
return the limit to the pool without touching it, and the first two land the
day in `Failed` while the terminal module treats a failed day with no failure
receipt as storage corruption.

Reachable after a halt of more than a day: the cycle forfeits the missed UTC
days rather than settling them, so a Worldwide Day whose emission day fell
inside the halt is never given a limit, takes the `ZeroDayLimit` path when it
reaches READY, and its tributes are never retired.

- `crates/core/metadosis/src/settlement.rs:206–262` · `metadosis/src/reducer.rs:209–210` · `metadosis/src/terminal.rs:163–170`
- `crates/system/cycle/src/handler.rs:336–346`

### INC-28 · An auction's schedule is fixed when its day is requested, but the auction is briefed when the day activates

The request freezes `logical_anchor` at its own block time. Activation replays
it when it writes the brief, and the brief anchors the auction to that
timestamp's midnight if at least 18 hours of commit window remain. Every hour
between request and activation comes out of the commit window, and a START at
or after the window's end cancels the auction as overdue and returns its limit
to the pool.

- `crates/core/metadosis/src/ocomp/request.rs:269` · `metadosis/src/ocomp_limits.rs:164`
- `crates/core/desis/src/runtime.rs:54–66, 516–523` · `desis/src/constants.rs:46–49` · #362

### INC-29 · Gem charges base gas for the PayNote proof that Nod and Intex charge 300 000 for

| Nod · Intex | Gem |
|---|---|
| the PayNote settle call adds `ZK_VERIFY_GAS` = 300 000 | `PRECOMPILE_BASE_GAS` for every call: "still verifies its PayNote proof but does not add a separate `ZK_VERIFY_GAS` tariff" |

All three run the same verifier. The gas schedule is a consensus rule, and this
one prices a full proof verification at base cost on one of the three paths.

- `crates/core/gemfactory/src/precompile.rs:27–31` · `nodfactory/src/precompile.rs:24–30` · `intexfactory/src/precompile.rs:33–37`
- `crates/blockchain/primitives/src/storage/gas.rs:30` · #408

### INC-30 · A Nod's PayNote must belong to the Nod's owner; its ABI says the caller, and Gem and Intex bind it to the caller

| Nod | Gem · Intex |
|---|---|
| `claim.owner != terms.owner_reference`: the Nod's owner; the submitter is ignored | `claim.owner != caller` · `claim.owner != settler` |

`INodFactory.sol` says the proof "must name the caller as its owner". On Nod
anyone may relay the owner's note, and a broadcast proof can be spent on
another of the same owner's Nods with the same cost and asset; nobody can pay
someone else's Nod with their own note, which Gem and Intex allow. The ERC-20
rail accepts any payer on all three.

- `crates/core/nodfactory/src/runtime.rs:341–350` · `nodfactory/src/precompile.rs:63` · `contracts/precompiles/src/INodFactory.sol:63`
- `gemfactory/src/runtime.rs:305–314` · `intexfactory/src/runtime.rs:926–935` · #433

### INC-31 · Credis counts the partial issuance day toward its breach count; the other three start at the first full day

| Credis | Nod · Gem · Intex |
|---|---|
| `timestamp_to_date_key(position.issued_at)` | `first_full_day(issued_at)` |

A Credis position opened mid-day counts that day toward its 21, so it can be
called a day sooner than a Nod, Gem or Intex issued at the same moment. The
Credis scan's own doc says it mirrors Gem's.

- `crates/core/credisfactory/src/called.rs:255–266` · `gem/src/runtime.rs:35` · `nod/src/called.rs:338` · `intexfactory/src/called.rs:406`
- `crates/blockchain/primitives/src/time.rs:91–98` · #394 and #433 moved the other three

### INC-32 · Off mainnet, Gem and Intex run test call terms while Nod and Credis run production terms

| Gem · Intex | Nod · Credis |
|---|---|
| profile unset → DEV on every chain but mainnet: 10% markup, 5% floor, 3-day notice, 2 breach days of 3 | constants on every chain: 256% / 64% markup, 7-day notice, 21 of 28 |

A testnet therefore runs Gem and Intex on different economics from Nod and
Credis, and from mainnet. The storage docs read "(0 = prod, 1 = dev)" while
the constants are `AUTO = 0`, `DEV = 1`, `PROD = 2`.

- `crates/core/gem/src/config.rs:14–19, 44–70` · `intexfactory/src/config.rs:17, 48–66` · `gem/src/schema.rs:185` · `intexfactory/src/schema.rs:65`
- `nod/src/state.rs:28–49` · `credis/src/runtime.rs:186–195` · #334

### INC-21 · Credis sweeps once a day; the other three carry an unfinished sweep into every block

| Instrument | Call sweep | Forfeit / void / expiry | Cursors |
|---|---|---|---|
| Nod | daily, continued every block · 4 096 buckets visited per block | 256 forfeits per block | currency, bin, forfeit |
| Gem | daily, continued every block · 256 calls per block | 16 expiry steps per block | currency |
| Intex | daily, continued every block · 256 group decisions per block | 256 series actions per block | currency |
| Credis | once a day, backlog not coalesced · 4 096 positions visited | 64 voids per day | one |

A lapsed Credis position can no longer be settled (INC-15), so the backlog no
longer moves value between parties. What it delays is the burned collateral's
credit to PromisLimit, which is the auction's cap (INC-04).

- `crates/system/cycle/src/lifecycle.rs:64–70` · `nod/src/hooks.rs:18–27` · `nod/src/constants.rs:55, 61`
- `gem/src/constants.rs:14, 21` · `intexfactory/src/constants.rs:31–32` · `credisfactory/src/called.rs:40, 55` · `cycle/src/triggers.rs:243–252`

---

## B · Needs a decision

Three. Each is a fork the code has taken on purpose, with a consequence worth
recording.

### INC-33 · Credis marks up a reference price, not its own entry price

| Nod · Gem · Intex | Credis |
|---|---|
| call = entry × (100 + markup) / 100, the same price the cost is struck at | call = anchor × 164 / 100; anchor = previous closed UTC-day COEN/reference VWAP; entry = the pledge quote in the issuance currency |

Deliberate: `ICredisFactory.sol` says the anchor "is independent of spot and
the pledge entry price, even when the reference and issuance currencies
match". Credis is therefore the only instrument whose call level is not a fixed
multiple of its own entry, and `credis/src/constants.rs` still describes the
call as "entry + 64%". Worth recording whether 64% is growth on the loan's own
price or on the reference market.

- `crates/core/credis/src/runtime.rs:94–99, 186` · `credis/src/schema.rs:144–146` · `credis/src/constants.rs:15`
- `credisfactory/src/runtime.rs:136–140, 193–212` · `contracts/precompiles/src/ICredisFactory.sol:30–39` · #452

### INC-34 · A CCA now needs a one-billion-COEN bond to originate

| To originate | On a void |
|---|---|
| Active standing at the CCA registry: bonded ≥ 10⁹ COEN, unbonding over 128 days. Required by `issueCredis` and `reserveStables`. | nothing is slashed; the burned collateral comes off the CCA's reward weight for that day, and any excess carries as a deficit |

#348 removed the per-position stake escrow and its burn; #410 puts a standing
bond in its place. Worth confirming that is the intent.

- `crates/core/ccaregistry/src/constants.rs:5–8` · `ccaregistry/src/runtime.rs:44–54, 160–186`
- `credisfactory/src/runtime.rs:62–63` · `vaultrouter/src/runtime.rs:408–415` · #410, #441

### INC-35 · A Nod and an Intex from the same day can be priced off different UTC days

| Nod | Intex |
|---|---|
| the finalized VWAP of the UTC day before the day's scheduled processing, frozen at PrepareOcomp | the VWAP of the UTC day before the auction's START |

The two coincide when the auction starts on the processing day. A late brief
moves START to a later midnight (INC-28), and the two instruments of one day
then enter at different prices. Desis states the choice: "The day before this
start, not before the brief".

- `crates/core/lysis/src/runtime.rs:224–237` · `crates/core/desis/src/runtime.rs:54–66, 458–465` · #455, #467

---

## C · Latent or documentation

Nothing here moves a balance today. Each costs something the first time someone
edits the code around it.

### INC-24 · A PayNote deposit and a Credis settlement trust the requested amount; the Nod, Gem and Intex rails measure it

Nod, Gem and Intex settlement check the token calls' return data and require
their balance to move by exactly the cost. The PayNote pool derives the note's
commitment from the requested amount and ignores `transferFrom`'s return data;
Credis settlement pulls and approves with unchecked calls. VaultRouter's own
pull from those contracts is checked, so the gap matters only for a token that
misreports a transfer. The PayNote comment "the asset and amount this call
actually moves" says more than the code does.

- `crates/core/paynote/src/runtime.rs:126–160` · `credisfactory/src/runtime.rs:272–292`
- `nodfactory/src/runtime.rs:126–150` · `gemfactory/src/runtime.rs:397–409` · `intexfactory/src/runtime.rs:875–887` · `vaultrouter/src/runtime.rs:854`

### INC-36 · A failing Gem forfeit drops the gem from the queue and strands its load

On an error the expiry sweep removes the gem from the called queue. The gem
stays Called past its deadline, so it can neither be settled nor swept again,
and its load never reaches PromisLimit. Nod retries the bucket on its next
pass, Intex defers and retries an hour later, and Credis fails the block. Error
path only.

- `crates/core/gem/src/hooks.rs:285–296` · `nod/src/called.rs:388–391` · `intexfactory/src/expired.rs:100, 113–129` · `credisfactory/src/called.rs:153–171` · #366

### INC-37 · Qualification reads month maxima that nothing builds for VWAPs recorded before #488

Qualification walks the two edge months day by day and reads every month
between them from a month-maximum map. Its only writer is
`record_utc_day_vwap`, added in #488, and nothing rebuilds the map for days
recorded earlier. On a chain upgraded in place with earlier history, an
above-floor day in an interior month is missed and a Nod, Gem or Intex can read
as unqualified. Fresh chains are unaffected.

- `crates/system/oracle/src/state.rs:847–866, 872–899` · `oracle/src/api.rs:484` · #488

### INC-25 · Names and comments that describe code that is no longer there

None of these change a balance. Each will mislead the next reader.

- **Stake.** `CcaStakeMismatch` and a parameter named `stake` for what is now
  COEN the CCA sells to the borrower (INC-22); "credis escrow" in the Gratis
  API doc; and `Void::unpaid_share`, documented as scaling "the originating
  CCA's penalty" and read by nothing; the void penalty uses `gratis_burned`.
  `credisfactory/src/errors.rs:18` · `credisfactory/src/runtime.rs:56` ·
  `gratis/src/api.rs:36` · `gratisfactory/src/runtime.rs:4–5` ·
  `credis/src/runtime.rs:86–88`
- **Credis.** "14 days later they all lapse together" over a 7-day notice; a
  `positionId` "derived from `pledgeNote` and `smartAccount`" where the code
  hashes CCA, smart account, asset and block; "Call price: entry + 64%" over an
  anchor-based price (INC-33); `requestCredis` names left after the rename to
  `issueCredis`; the error text "call window has lapsed" for the notice
  period; and `hasCalledPosition`, which reads as a standing check and gates
  nothing. `credisfactory/src/called.rs:48–49` · `ICredisFactory.sol:41` ·
  `credis/src/schema.rs:227` · `credis/src/constants.rs:15` ·
  `gratis/src/api.rs:91, 132` · `credis/src/errors.rs:28` · `ICredis.sol:115`
- **CCA registry.** `ICcaRegistry.sol` declares `CcaNotActive(address, State)`;
  the code reverts with a string. `ICcaRegistry.sol:23` ·
  `ccaregistry/src/errors.rs:34`
- **Nod.** `INodFactory.sol` says "Pay a qualified Nod", though a called Nod
  pays too, inside its notice; `tokenURI` shows Called where `nodData` shows
  Forfeited; and the on-chain description says "its owner pays the settlement
  cost" where any payer may. `INodFactory.sol:55` · `nod/src/metadata.rs:19–23` ·
  `nod/src/api.rs:26–27` · `nod/src/constants.rs:5`
- **Gem.** `IGem.sol` says Genesis gems are "Born Qualified", where
  qualification is derived from finalized VWAPs; the call-price doc says entry
  is "the issuance-time coen rate", where it is the previous day's VWAP;
  `gem/src/api.rs` says forfeit runs "from the daily scan", where it runs every
  block; and the same owner-pays description. `IGem.sol:49` ·
  `gem/src/api.rs:51, 66–67` · `gemfactory/src/runtime.rs:702` ·
  `gem/src/constants.rs:4`
- **Intex.** `IIntexFactory.sol` says issuance-currency settlement "needs fresh
  rates", where it uses the last closed day, and still mentions the
  authorised-settler setter #393 removed; "their load belongs to the settler",
  where it goes to the owner; the mining doc says "`owner` is the caller",
  where it is an argument; the Solidity `markCalled` error still names only
  `Qualified`; and the settled-token id is documented as a keccak hash, where
  it is a tagged series id. `IIntexFactory.sol:6, 20` · `IIntex.sol:40` ·
  `intexfactory/src/runtime.rs:830, 1090–1091` ·
  `IntexNFT1155.sol:26, 247, 410, 457`
- **PayNote.** `IPayNote.sol` declares five custom errors the implementation
  never emits, and names proof version 1.1.0 where the Rust says 1.2.0.
  `IPayNote.sol:13, 17–26` · `paynote/src/errors.rs:53` · `paynote/src/lib.rs:11`
- **Call constants.** `CALL_RATE_PCT` (Nod 256, Credis 64) against `CALL_RATE`
  (Gem and Intex 128); `PRICE_RATE_DEN` named in Credis and Intex and a bare
  `100` in Nod and Gem; and the ABIs expose different subsets of the sealed
  call terms: all of them on `INod.sol`, none on `ICredis.sol`.
  `nod/src/constants.rs:25` · `credis/src/constants.rs:13–16` ·
  `gem/src/constants.rs:45` · `nod/src/state.rs:40` · `INod.sol:80–86` ·
  `ICredis.sol:53–90`
- **Pipeline.** The local, non-OCOMP path still computes the auction as
  `min(nominal, day limit) − Lysis` under "headroom" comments; three
  `split_total ≤ day_limit` checks are always true; the late-residue docs say
  the next day's formation consumes the pool, which formation no longer draws;
  `OcompDayLimitFormed.carryOverTaken` is always zero; the emission
  dispatcher's doc counts six sinks over a table of five and promises `Fatal`
  where the code returns `Revert`; and Lysis test prose names `LYSIS_LIMIT_MIN`
  and `LYSIS_LIMIT_MAX`, which do not exist. `metadosis/src/settlement.rs:66–70` ·
  `ocomp-protocol/src/intent.rs:319` · `result.rs:508` · `receipts.rs:429` ·
  `metadosis/src/emission_sink.rs:39–40` · `metadosis/src/commit.rs:289` ·
  `emissionlimit/src/block.rs:9, 32, 44, 55–56` · `lysis/src/tests.rs:641, 922`
- **Tribute and naming.** `ITributeFactory.sol` describes a caller and L1
  binding that now also covers the L2 chain; the `amount_atto` →
  `amount_micro` rename left an `atto` field in the enclave and an unused
  "invalid atto amount format" error; AgentReward calls all three reward pools
  its own, though the CCA pool belongs to the registry; and `IDesis.sol` names
  `supply` what Rust calls `desis_limit_minor`. `ITributeFactory.sol:10` ·
  `bin/outbe-tee-enclave/src/zk_claim.rs:40, 69, 78` ·
  `tributefactory/src/errors.rs:105–106` ·
  `agentreward/src/distribution.rs:158, 177` · `IDesis.sol:91`
- **Gratis.** The factory's module doc describes a `mine` function; the
  function is `mint`. The Fidelity eligibility gate tests for league
  `u16::MAX`, which the league formula cannot produce.
  `gratisfactory/src/runtime.rs:5–7, 133, 155` · `fidelity-math/src/lib.rs:36`

---

## D · Resolved on main

Sixteen. Each names the pull request that closed it and was re-read at
`d84ebef1`.

### INC-01 · `settle_nod` accepted payment for a Nod that could never be mined

**Holds, with the pay step back.** #314 folded payment into mining; #417 split
it out again as `settleNod` and `settleNodWithPayNote`. Both accept payment
only while the Nod can still be mined: an uncalled Nod must be qualified, a
called one must be inside its notice, and the check runs again inside the
state change. Mining then needs only the paid flag, with no deadline, and the
forfeit sweep walks unpaid Nods only, treating a paid one as corruption. No
paid-but-unmineable state exists.

- `crates/core/nodfactory/src/runtime.rs:182–216, 255–272` · `nod/src/state.rs:386–390, 415–421` · `nod/src/called.rs:379–382, 538`
- #314, #417, #423, #431

### INC-02 · Nod call price was a multiple where the others were markups

Applied as `entry × (100 + 256) / 100` = **3.56×**, now sealed on each bucket
at issuance.

- `crates/core/nod/src/state.rs:28–40` · `nod/src/constants.rs:25`

### INC-03 · Forfeited Nod and Gem load was not routed to carry-over

All four credit PromisLimit 1:1: Nod, Gem and Intex on forfeit or expiry, Gem
positions on expiry, and Credis on void.

- `crates/core/nod/src/called.rs:559–568` · `gem/src/runtime.rs:71–75` · `gemfactory/src/expired.rs:79–80`
- `intexfactory/src/expired.rs:176–207` · `credisfactory/src/runtime.rs:351–352`

### INC-04 · The auction had no consumption ceiling

```
GREEN: auction = min(nominal − Lysis limit, pool + (day limit − Lysis limit))
RED:   auction = 0
```

**Closed on the pool reading (#362).** Day formation takes only the day's own
emission. The request credits what Lysis leaves of it to PromisLimit, and the
auction draws from PromisLimit, capped by the nominal beyond the symbolic
share. With an empty pool it reduces to the day-limit result. Two details: the
cap subtracts the Lysis limit rather than what Lysis actually allocated, and
the unused part reaches the pool at activation without enlarging that day's
auction. The draw's timing is INC-27.

- `crates/core/metadosis/src/ocomp_limits.rs:40–66, 104–130, 144–157` · `metadosis/src/emission_sink.rs:82–96` · #362

### INC-05 · The pool-backed auction ceiling collided with the pool drain

Day formation no longer drains the pool (`carry_over_taken` is zero), and the
only draw is the auction's, at activation.

- `crates/core/metadosis/src/emission_sink.rs:82–96` · `promislimit/src/ocomp_limits.rs:36` · #362

### INC-10 · Two constants named `CALL_RATE_PCT` used different formulas

All four markups apply as `base × (100 + X) / 100`. Credis's base differs
(INC-33), and the names still differ (INC-25).

### INC-12 · The Credis notice period was 14 days where the other three were 7

Seven days, sealed on each position at opening.

- `crates/core/credis/src/constants.rs:46` · `credis/src/runtime.rs:191` · #358, #379

### INC-13 · Nod paid through PayNote at mining; Gem and Intex through a separate settle step

One shape for all three: a settle call on either rail, ERC-20 into the reserve
vault or a PayNote spend, followed by a separate mine. Credis settles by
ERC-20 only.

- `contracts/precompiles/src/INodFactory.sol:60, 65` · `IGemFactory.sol:18, 26` · `IIntexFactory.sol:21, 30` · #417, #423, #363, #435

### INC-14 · Credis counted a breach at or above the call price

Strictly above, as in the other three.

- `crates/core/credisfactory/src/called.rs:272` · #360

### INC-15 · Credis settlement had no deadline gate

A called position past its deadline is rejected with `CallWindowClosed`, as
the other three reject theirs.

- `crates/core/credis/src/runtime.rs:272–274` · #480

### INC-16 · Call terms were read live for Nod and Credis and snapshotted for Gem and Intex

All four seal window, threshold and notice on each record: Nod on each bucket
at issuance, Credis on each position.

- `crates/core/nod/src/state.rs:28–49, 632–660` · `credis/src/runtime.rs:186–195` · `gem/src/api.rs:35–42` · `intexfactory/src/runtime.rs:70–74` · #379, #433, #470

### INC-17 · Nod qualification read a rate of any age; Gem and Intex required one under six hours old

All three derive qualification when it is read: the highest finalized daily
VWAP since the first full day must exceed the floor. No live rate and no
stored latch.

- `crates/core/nod/src/api.rs:38–52` · `gem/src/api.rs:68–75` · `intexfactory/src/runtime.rs:962–972` · `oracle/src/api.rs:484` · #400, #470, #402, #469, #471

### INC-18 · Nod's breach window started on the worldwide-day key

Nod cuts off at the first full UTC day of its sealed issuance time; no UTC+14
key remains in any breach path. Credis differs in the other direction
(INC-31).

- `crates/core/nod/src/called.rs:333–338` · #433

### INC-19 · A PayNote that over-covered its Nod kept the difference in the vault

A PayNote spend must equal the cost exactly on all three instruments; any
surplus stays with the payer as a change note. The ERC-20 rails pull exactly
the cost, and Credis clamps to what the position owes.

- `crates/core/nodfactory/src/runtime.rs:355–362` · `gemfactory/src/runtime.rs:317–321` · `intexfactory/src/runtime.rs:938–942` · `paynote/src/runtime.rs:272` · #359, #466

### INC-20 · Nod and Gem settled against different asset allow-lists

One rule for Nod, Gem and Intex: any asset with a reserve vault whose
self-reported ISO code is the reference or the issuance currency, the issuance
leg converting at the previous closed day's VWAP. Credis settles in the one
asset sealed at pledge.

- `crates/core/nodfactory/src/runtime.rs:396–411` · `gemfactory/src/runtime.rs:469–480` · `intexfactory/src/runtime.rs:1061–1072` · #433

### INC-22 · The CCA's matching COEN passed through to the borrower unpaid

Paid for. At issuance the CCA sends COEN equal to the collateral to the
borrower's account, and the vault pays the stablecoin principal to the CCA;
the borrower repays in stablecoins. The "stake" naming is left (INC-25); the
CCA's new bond is INC-34.

- `crates/core/credisfactory/src/runtime.rs:108–118, 159–172` · `contracts/precompiles/src/IVaultRouter.sol:205–211` · `ICredisFactory.sol:22–33` · #441

---

## E · Superseded or moot

Closed by a decision that went another way, or by the thing they were about
ceasing to exist.

### INC-06 · Two ADRs said Promis→Gratis preserved age; the code created a fresh cohort

The ADR corpus was deleted (#322). `PromisFactory.mineGratis` mints a fresh
cohort, and its ABI says so: "records a fresh Fidelity acquisition cohort,
exactly as any other gratis acquisition does."

- `contracts/precompiles/src/IPromisFactory.sol:24–25` · `crates/core/promisfactory/src/runtime.rs:78–80`

### INC-07 · Nod and Gem call lifecycle undocumented in the ADRs

ADR corpus deleted (#322). No documentation remains to be stale.

### INC-08 · Merchant gems documented as rejected, shipped in full

ADR corpus deleted (#322). The calls are `issueGemPosition` and `issueGem`
(#329).

### INC-09 · Emission sink table: six in the ADR, five in the code

ADR corpus deleted (#322). The five-sink table and the curve are unchanged:
2^28 COEN rising to a peak near 8.7 × 2^26 around day 1024, then falling to a
2^26 floor at day 3072. Since #410 the CCA share is paid to CCAs by reward
weight, and whatever finds no weight returns to Metadosis.

- `crates/system/emissionlimit/src/day_emission.rs` · `emissionlimit/src/allocation.rs:10–13` · `crates/core/ccaregistry/src/emission_sink.rs:31–33`

### INC-11 · Settlement authority differed across all four instruments

On the ERC-20 rail any payer can settle any of the four. The one asymmetry
left is the PayNote binding, INC-30.

---

## What is left

Nine open items. Two come first: Credis cannot be opened until its pledge
price is implemented (INC-26), and an OCOMP job that outlives the next day's
request can abort block building at activation (INC-27). After a multi-day
halt, the skipped days close without retiring their tributes (INC-23). The
other six are rules that differ between instruments, or between code and its
ABI: the auction anchor (INC-28), Gem's PayNote gas (INC-29), the Nod PayNote
owner (INC-30), Credis's partial issuance day (INC-31), the test profile off
mainnet (INC-32), and Credis's once-a-day sweep (INC-21). Three decisions:
Credis's call anchor (INC-33), the CCA bond (INC-34), and the entry day of a
Nod against an Intex (INC-35).

First pass surfaced eleven. The re-check against `main` at `296e8375` closed
nine and added INC-13 to INC-25. The pass at `d84ebef1` closed ten more,
INC-12 to INC-20 and INC-22, moved INC-04 onto the pool reading chosen in
discussion and closed INC-05 with it, and added INC-26 to INC-37. The daily
split, GREEN/RED, the emission curve and sinks, every PromisLimit credit,
Fidelity's mutation sites, the Promis-to-Gratis conversion, agent rewards and
contributor payouts checked out against each other.
