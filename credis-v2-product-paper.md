# Credis — Product Paper

**Purpose.** This paper describes the Credis product — the outbe protocol's
liquidity primitive — and the CCA program built around it, at the level of behavior,
economics, and rules. It is the source document for the engineering team to derive
their technical specification from. Values marked **TBD** are product decisions still
open; §11 lists them together.

---

## 1. What Credis is

Credis lets a holder of Gratis (the soulbound right to mine COEN) unlock spendable
money today without giving up their future upside. The user pledges Gratis as
collateral and receives stablecoins from the protocol reserve (the vault) into their
smart-account card. The position then lives on the COEN price path, not on a calendar:
there are no installments, no due dates, and no maturity. The owner repays when
COEN has appreciated — exactly when repaying is cheaper than mining and selling — and
reclaims collateral in proportion to what they repay. If COEN appreciates strongly and
the owner still doesn't act, the protocol calls the position and, after a response
window, writes off whatever remains.

Credis is the vault's **only** outflow. Repayments and interest are vault inflows.
Nothing in the product ever market-sells collateral: the write-off path burns Gratis
and credits the protocol's invest-side capacity (the Promis limit), so a failed Credis
contracts future issuance rather than cascading.

## 2. Actors

- **Owner** — a Gratis holder operating through a smart-account bundle (the card).
  Their identity as pledger is confidential end-to-end: the position stores only an
  encrypted reference; collateral always returns to them no matter who pays.
- **CCA (Consumer Credit Agent)** — a registered, bonded service business that opens
  Credis positions for its customers and operates their card spending within limits.
  CCAs earn the CCA emission sink for origination and are accountable for how their
  book resolves (§8).
- **Vault (Reserve)** — holds settlement proceeds in stablecoins per currency; funds
  disbursements; receives repayments and premiums. Carries no liabilities.
- **Semiosis (the price oracle)** — supplies, per supported currency: the live COEN
  price, the official daily reference price series, and the official central-bank
  policy rate (§6).

## 3. The position

Opening a position seals its full economics at the start — nothing about its terms
ever changes afterward. Each position records:

| term | meaning |
|---|---|
| Currency | the ISO currency of the Credis (USD, EUR, …), determined by the disbursed stablecoin |
| Principal | the stablecoin amount disbursed |
| Origination date | the UTC date the position opens — the anchor of the interest day count (accrual runs from here until the first settlement; the position also tracks the date of the most recent settlement, from which accrual continues) |
| Policy rate (r) | the **annual** official central-bank policy rate of the currency (× a policy-rate factor, default 1), **recorded at opening and fixed for the position's life**. Interest accrues at the policy rate as normal simple (non-compounding) interest on the outstanding principal, day-counted ACT/365, and is collected whenever a settlement happens — there are no interest due dates |
| Collateral (G) | the pledged Gratis, valued 1:1 against principal at the entry price |
| Entry price (P₀) | the COEN price in the position's currency, quoted at pledge time |
| Floor price | P₀ + **8%** — the price at which settlement unlocks |
| Call price | P₀ + **32%** — the price whose sustained breach triggers the call |
| Outstanding principal (P_out) | starts at P and decreases with each settlement; the position closes when it reaches zero |

One pledge → one position. Users who want granular exposure open several smaller
positions rather than one large one.

## 4. Lifecycle

```
                     price > floor (once, ever)
   OPEN ────────────────────────────────────────► SETTLEABLE
    │                                                  │
    │   daily reference ≥ call price                   │  partial or full
    │   for 21 consecutive UTC days                    │  settlement any time
    ▼                                                  ▼
  CALLED ──── 14-day window, settlement still open ── SETTLED (fully repaid)
    │
    └── window lapses with debt remaining ──► remainder VOID
        (unpaid share of collateral burned → Promis limit; debt written off)
```

**Settlement unlock (the floor).** The moment the live COEN price (in the position's
currency) exceeds the floor price, the position becomes settleable — permanently. This
is a one-way latch: a later price fall does not re-lock it.

**Settlement formula (partial or full).** Once settleable (and equally while called),
the owner — or anyone paying on their behalf — may send any amount `S`. Interest is
computed **at the moment of settlement** from the days elapsed, and the payment is
applied **interest first, principal second**:

```
d    = UTC days elapsed since the previous settlement
       (for the first settlement: since origination)
I    = P_out × r × d / 365        accrued interest (simple, non-compounding)
S    ≥ I                          a payment below the accrued interest is rejected
p    = min(S − I, P_out)          principal covered by this payment
ΔG   = G × p / P                  collateral released to the owner
P_out ← P_out − p                 (P_out = 0 ⇒ SETTLED; accrual restarts at d = 0)
```

The vault receives `I + p`. Because interest is settled in full at every payment, no
unpaid interest ever carries between events — accrual simply restarts on the reduced
principal, so the position needs no per-block interest bookkeeping. Collateral release
is **principal-proportional**: covering x% of the original principal returns x% of the
pledged Gratis. Repeated partial settlements compose. Rounding always favors the
protocol and leaves no dust: released amounts are floored, and the final settlement
(`p = P_out`) releases exactly the remaining locked collateral.

**The call.** If the **official daily reference price** stays at or above the call
price for **21 consecutive UTC days**, the position is CALLED. A day below the call
price resets the count. Being called changes only two things: a **14-day** settlement
window starts, and the owner cannot open new positions until the call is resolved.
Settlement (partial or full) remains open on unchanged terms throughout the window.

**Void of the remainder.** If the window lapses with `P_out > 0`, **only the remainder
is voided**:

```
burned        G × P_out / P   → value credited to the Promis limit
written off   P_out, plus the interest accrued on it since the last
              settlement — neither is ever collected
```

Whatever the owner settled — before or during the window — they have already
reclaimed. A fully-unpaid called position loses all its collateral; a 75%-settled one
loses a quarter. The unpaid fraction `P_out / P` is also what scales the originating
CCA's penalty (§8.4).

**Why the call exists.** At +32%, collateral is worth far more than the debt — any
present owner settles. The call forces resolution of deeply in-the-money positions:
active owners return stablecoins to the vault precisely when COEN is strong, and
abandoned positions (lost keys, dormant users) are swept off the books into invest-side
capacity instead of lingering forever.

## 5. Worked example (USD)

*(Figures rounded to cents and to 0.01 Gratis units; on-chain arithmetic is exact.)*

**Origination — day 0.** Maria pledges Gratis worth $1,000 at an entry price of
P₀ = $0.50 per COEN — collateral G = **2,000 units** — and receives principal
P = **1,000 USDC** on her card. The Fed policy rate that day is 4% annual, so her
recorded rate is r = **0.04**. Her token reads: floor **$0.54** (P₀ + 8%), call
**$0.66** (P₀ + 32%), P_out = 1,000. These terms will never change.

**Partial settlement while latched — day 100.** COEN trades up through $0.54; the
position latches settleable, permanently. Maria sends **S = $400**:

- elapsed d = 100 days → accrued interest
  **I = 1,000 × (4%/365 × 100) = $10.96**;
- principal covered **p = 400 − 10.96 = $389.04**;
- released **ΔG = 2,000 × 389.04/1,000 = 778.08 units** to her confidential balance;
- **P_out = 1,000 − 389.04 = $610.96**; locked collateral 1,221.92 units; the vault
  received $400 ($10.96 interest + $389.04 principal); accrual restarts.

**The call — day 458.** The official daily USD reference price holds at or above
$0.66 for **21 consecutive UTC days** (a single day below would have reset the
count). The position is CALLED; a 14-day window opens (through day 472); settlement
terms are unchanged.

**Partial settlement while called — day 465.** Seven days into the window Maria
sends another **S = $400**:

- elapsed since the last settlement d = 465 − 100 = 365 days → accrued interest
  **I = 610.96 × (4%/365 × 365) = $24.44**;
- principal covered **p = 400 − 24.44 = $375.56**;
- released **ΔG = 2,000 × 375.56/1,000 = 751.12 units**;
- **P_out = 610.96 − 375.56 = $235.40**; locked collateral 470.80 units.

**Void of the remainder — day 472.** She never sends the rest. The window lapses
with P_out = $235.40 (unpaid fraction 235.40/1,000 = **23.54%**):

- burned: **2,000 × 235.40/1,000 = 470.80 units**, their value credited to the
  Promis limit;
- written off: **$235.40** of principal, plus the $0.18 of interest accrued on it
  over the window's last 7 days — neither is ever collected; her cost is the
  collateral forfeit and her Fidelity standing;
- her originating CCA's multiplier takes a 0.10 × 23.54% ≈ **2.35%** cut.

**Ledger check.** Collateral: 778.08 + 751.12 released + 470.80 burned =
2,000.00 ✓. Principal: 389.04 + 375.56 settled + 235.40 voided = $1,000.00 ✓.
Interest collected: $10.96 + $24.44 = **$35.40**, each computed on the outstanding
principal for the days actually elapsed. The vault disbursed $1,000 and received
$800.00 back; the invest side gained 470.80 units of Promis-limit capacity; Maria
kept $235.40 she never repaid and lost 470.80 units of Gratis. At no point did a
schedule exist — interest accrued with time, as in any normal loan, but every *event*
above was produced by the price path, except the 14-day response window.

## 6. Multi-currency support

Credis is not USD-only. A position may be denominated in any supported ISO currency —
USD, EUR, GBP, and the rest of the canonical set — chosen implicitly by the stablecoin
the Credis is disbursed in. Everything scales per-currency:

- **Prices.** P₀, floor, and call are in the position's currency; the settlement latch
  and the call streak read that currency's COEN price and official daily reference
  series from Semiosis.
- **Policy rate.** The recorded rate r is the **annual official central-bank policy rate
  of the position's currency** — Fed funds for USD, the ECB rate for EUR, the Bank of
  England rate for GBP — as published on-chain by Semiosis and pinned at opening;
  interest accrues on the outstanding principal (simple, ACT/365) and is collected
  interest-first at each settlement, all in the position's own currency.
- **Vault.** The vault holds and lends per-currency stablecoin reserves; a position's
  disbursement, repayments, and premium stay in its own currency end to end.

**Requirement on Semiosis** (to be reflected in the oracle roadmap): for every
supported currency, Semiosis maintains (a) the COEN daily reference price series in
that currency and (b) an **official policy-rate feed** — the central bank's published
rate, updated when the institution moves it, with the same evidence discipline as its
other official series. A currency is admissible for Credis only when both exist.

## 7. Owner rules

- **Matched funding.** To receive a Credis of X, the owner's card account must
  already hold X of the same stablecoin — the card product's own-funds rule. The
  bundle mechanics (spend limits, CCA permissions, reserve accounting) are unchanged.
- **Standing.** An owner with any CALLED position cannot open new positions until it
  resolves; a per-account cap bounds the number of open positions (**TBD**).
- **Quotes are fresh.** A pledge quote (which fixes P₀ and therefore the whole
  geometry of the position) expires if unused — stale quotes cannot be exercised
  (TTL **TBD**).

## 8. The CCA program

### 8.1 Registration and bond

Anyone may register as a CCA by posting a COEN bond — no approvals, no allowlist. The
bond requirement is **dynamic**: near zero activity (genesis) it sits at a low minimum;
as the CCA program's actual daily payouts grow, the requirement grows proportionally,
between a floor and a cap (clamps proposed at 0.1× and 10× the validator minimum
stake, **TBD**). Rationale: the bond prices *identity reset* — walking away from a
penalized reputation and re-registering must always cost more than serving the penalty
out. Registered CCAs must top up within a 30-day grace when the requirement outgrows
their bond by 50%; otherwise they are suspended from new originations until cured.

### 8.2 Delegated bonds

A CCA may raise part of its bond from supporters. Delegation buys **no reward and no
weight** — the protocol pays delegators nothing (a CCA may share revenue off-protocol
at its own discretion). Protections: the CCA's own contribution is **first-loss** and
must stay ≥ **50%** of the requirement; delegated funds are a senior layer touched
only after the CCA's own layer is exhausted. Unbonding cooldowns: **21 days** for
delegators, **128 days** for the CCA's own contribution. Any withdrawal while the CCA
stands penalized (multiplier below 1, §8.4) pays a proportional exit haircut — first
from the CCA's layer — which is burned.

### 8.3 Rewards: paid for origination

The daily CCA emission sink (4% of the day's emission) is distributed pro-rata to
**origination units** earned the previous day. One successful origination = one unit,
moderated by:

- a minimum principal for a unit to count (dust guard, **TBD**);
- **repeat-owner decay**: the nth position opened for the same owner by the same
  CCA earns 1, ½, ¼, … units;
- at most one unit per owner per day;
- a per-CCA cap of **32%** of the day's pool, with the excess redistributed;
- a per-unit reward ceiling (**TBD**) so that thin early days cannot make a single
  origination worth a fortune; anything undistributed returns to the main
  (Metadosis) emission sink.

The balances accumulated at the CCA and Merchant sink addresses before this program
activates are transferred to the Metadosis sink at activation — there is no
retroactive distribution.

### 8.4 Accountability: penalized for voids

Each CCA carries a **performance multiplier** m (from 1 downward) applied to its daily
units:

- **Penalty.** When a position it originated voids, m is cut in proportion to the
  unpaid share: a fully-unpaid void costs the full penalty step (10%); a void of a
  25% remainder costs a quarter of that.
- **Recovery.** m recovers only as the CCA's book **repays**: settled value on its
  originated positions restores m in steps. Idle time restores nothing; more
  origination restores nothing.
- **Quarantine.** Below m = 0.5 the CCA cannot originate. Its existing book can still
  settle — which is precisely its road back.
- **Exit.** Deregistering while penalized triggers the §8.2 haircut. The bond
  formula (§8.1) guarantees that re-registering fresh is never the cheaper path.

The economic intent: origination pays for the CCA's real service (bringing funded,
reachable owners into the system); voids mark its real failure (a book abandoned at
the moment it must respond); repayment volume — not marketing volume — restores
standing.

## 9. Value flows (summary)

| event | stablecoins | Gratis | COEN emission |
|---|---|---|---|
| Open | vault → owner card | pledged (locked, confidential) | CCA earns origination unit next day |
| Partial/full settlement | payer → vault: accrued interest I + principal p | release G × p/P to owner | settled value counts toward CCA recovery |
| Void of remainder | none | unpaid share burned | burned value → Promis limit; CCA multiplier cut |
| Daily CCA pool | — | — | 4% of day emission → units × multiplier, capped; residue → Metadosis |

## 10. Product parameters

| parameter | value |
|---|---|
| Settlement floor | entry + 8% |
| Call price | entry + 32% |
| Call streak | 21 consecutive UTC days at/above call price (a below day resets) |
| Call window | 14 days |
| Policy rate r | currency's annual official central-bank policy rate at opening × policy-rate factor (default 1, **TBD**); interest accrues at r as simple interest on outstanding principal, ACT/365, computed at settlement and applied interest-first |
| Max open positions per owner | **TBD** |
| Pledge quote TTL | **TBD** |
| CCA bond | dynamic; clamps ~0.1×–10× validator min stake (**TBD**); λ = 2 safety factor |
| CCA self-bond floor | 50% of requirement |
| Unbonding cooldowns | delegators 21 days; CCA own bond 128 days |
| Bond top-up grace | 30 days (triggered at 1.5× outgrowth) |
| Per-CCA daily reward cap | 32% of pool |
| Per-unit reward ceiling | **TBD** |
| Minimum principal per unit | **TBD** |
| Repeat-owner decay | ×0.5 per repeat |
| Void penalty step | 10% of multiplier, × unpaid share |
| Multiplier recovery | stepwise with settled value (rate **TBD**); origination floor 0.5 |

## 11. Pending product decisions

1. **Downside resolution.** As designed, a position whose price never reaches the
   floor simply waits — nothing forces closure on depreciation. A symmetric downside
   write-off (sustained depressed price → burn to Promis limit) exists as a designed
   option and matters doubly because void penalties can only occur after rallies;
   without it, no CCA penalty can fire in a flat or falling market. Decide adopt /
   defer.
2. **All TBD values** in §10 — policy-rate factor, position cap, quote TTL, bond
   clamps, unit ceiling and minimum, recovery rate.
3. **Missing-data days** in the call streak (no official daily reference published):
   define whether such a day pauses or resets the 21-day count.
4. **Supported currency list at launch** and the order in which Semiosis policy-rate
   feeds come online.

## 12. Notes for the spec authors

The product deliberately reuses existing protocol machinery: the confidential pledge
and release flow, the price-latch mechanism used by Nod qualification, the
call-and-deadline pattern used by Intex, the capped pro-rata distribution used by
WAA/SRA rewards, and the burn-to-Promis-limit path of the current default handler.
Settlement must remain safely payable by third parties (payer pays, pledger receives
collateral), events must not link the pledger's identity to positions, and the reward
weight calculation must read only service events — never balances, bond sizes, or
identities. Time never *forces an event* — there are no due dates and no maturity;
the only time-driven events are the sustained call streak and the response window.
Interest does accrue with time like any normal loan (simple, ACT/365), but it is
computed lazily at settlement from elapsed days, so no per-block accrual state
exists, and nothing may ever schedule off a position's creation date.
