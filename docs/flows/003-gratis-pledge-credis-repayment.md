# PFS-003: A Gratis pledge opens a Credis position and the price path resolves it

- **Status:** Draft
- **Actors:** Gratis owner/borrower bundle, Gratisfactory, Gratis, CredisFactory,
  Credis, Oracle, VaultRouter and reserve asset/vault
- **Trigger:** User pledges Gratis, then requests Credis against the sealed pledge ticket
- **Topology/services:** Finalizing network with configured Oracle (COEN price and
  policy-rate feeds), reserve asset, vault, source/target registrations and TEE enclave
- **Referenced ADRs:** ADR-C-GRT-001, ADR-C-GRT-002, ADR-C-GRT-003, ADR-C-FID-001,
  ADR-C-CRD-001, ADR-C-CRD-002, ADR-C-VLT-001, ADR-S-ORC-001
- **Supersedes:** the ten-installment anadosis repayment flow, removed with the Credis v2
  price-path model (`credis-v2-product-paper.md`)

## Notation

| symbol | meaning |
|---|---|
| `P` | principal — stablecoin minor units disbursed. Fixed at opening |
| `P_out` | outstanding principal. Reaching zero closes the position |
| `G` | collateral — pledged Gratis, valued 1:1 against `P` at the entry price. Fixed |
| `P₀` | entry price — COEN price in the position's currency, sealed at pledge time |
| `r` | policy rate — the currency's annual official policy rate, pinned at opening |
| `I` | interest accrued since the accrual anchor, simple and ACT/365 on `P_out` |

Derived and sealed at opening: floor price `P₀ + 8%`, call price `P₀ + 32%`.

## Outcome

One shielded Gratis pledge backs one uniquely identified Credis position, exact
stablecoin liquidity reaches the borrower bundle, and settlement — any amount, any time,
by any payer, once the price has crossed the floor — returns collateral in proportion to
the principal it covers, without losing conservation or permitting replay.

## Acceptance contract

- **Source:** Gratis owner operating through its borrower bundle.
- **Trigger:** A user pledges an eligible Gratis denomination, opens Credis against the
  sealed ticket, then submits settlements of arbitrary size.
- **Environment:** Finalizing network with configured TEE enclave, Oracle COEN price and
  policy-rate feeds, reserve asset, vault liquidity and registered source/target modules.
- **Canonical inputs:** Bundle-bound pledge handle and `spendAuth`, denomination and
  collateral, Fidelity eligibility, Oracle COEN and policy rates, exact reserve asset,
  vault shares, allowances and settlement amounts.
- **System under test:** Gratisfactory, Gratis, CredisFactory/Credis, Oracle, VaultRouter
  and reserve token/vault adapters.
- **Expected response:** Pledge/ticket evidence, one Credis position with sealed terms,
  asset disbursement, consumed pledge ticket, settlement receipts and proportional
  collateral release to the original pledger.
- **Response measures:** Principal, collateral, token and vault equations close; every
  pledge ticket and position is consumed at most once; a closed position rejects further
  settlement.
- **Failure guarantee:** Failed enclave call, withdrawal or deposit leaves the
  transaction's prior ticket, position, collateral and token/vault balances intact.

## Preconditions and canonical inputs

- User owns sufficient liquid Gratis and satisfies accepted Fidelity eligibility.
- Denomination is pledge- and Credis-eligible; the pledge ticket is sealed under the
  enclave state key.
- Bundle holds no unresolved called position; asset reports a registered ISO currency.
- Oracle has a COEN price for the position's currency and a non-zero policy rate for it.
- VaultRouter holds matching reserve shares and CredisFactory is the registered
  target/source as applicable.

## Success sequence

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | Gratisfactory | seal a pledge ticket and move the denomination to the pledged ledger | pledged/liquid balance deltas, sealed ticket |
| 2 | CredisFactory/Gratis | consume the ticket with the bundle-bound `spendAuth` | ticket consumed, sealed pledger EOA returned |
| 3 | CredisFactory/Oracle | read the asset's ISO code and pin the currency's policy rate | position `policyRate` |
| 4 | Credis | open the position, deriving floor and call from the sealed entry price | position/index records, `PositionCreated` |
| 5 | VaultRouter | withdraw the exact asset into the borrower bundle | token/vault deltas and event |
| 6 | Oracle/Credis | first time the live COEN price exceeds the floor, latch settleable | `PositionSettleable`, one-way |
| 7 | payer, repeatable | settle any amount: interest first, principal second | `SettlementApplied`, `PositionSettled` on close |
| 8 | Gratis | release `G × p / P` of collateral to the original pledger | pledged/liquid balance deltas |

Steps 6 and 7 compose in one transaction: `settle` latches the position itself when the
live price is above the floor, so a settler never has to wait for a separate step.

## Boundaries and conservation

Pledge, request, and each settlement are separate user transactions. Within each
transaction every listed module/external call rolls back together. Replay protection
crosses transactions through pledge-ticket uniqueness, position id, and the position's
terminal states.

Intended closure is:

```text
live locked collateral <= pledged Gratis backing
sum(principal settled + P_out + principal written off) = P
sum(released collateral + collateral_locked + burned collateral) = G
vault/token deltas = disbursement and settlements in the position asset
```

Because each partial release is floored, `collateral_locked >= floor(G × P_out / P)`,
with the drift always toward the protocol; the closing settlement releases the exact
remainder so nothing is stranded.

## Observable completion contract

After request: receipt succeeds, the position is owned by the bundle, its sealed terms
(`principal`, `collateral`, `policyRate`, `entryPrice`, `floorPrice`, `callPrice`) are
readable, the pledge ticket is consumed, the bundle token balance rose by the disbursed
amount, and vault shares fell consistently. After each settlement: interest is collected
in full before any principal, `outstanding` and `collateralLocked` fall together, reserve
liquidity increases, and the freed collateral appears in the original pledger's
confidential balance. After closure no further settlement is accepted.

## Replay, retry, restart and failure

A consumed pledge ticket cannot open a second position, and `spendAuth` binds the ticket
to one bundle. Failed vault withdrawal rolls back the position and the ticket. Failed
settlement deposit rolls back the position bookkeeping and the collateral release. A
payment below the accrued interest is rejected outright rather than partially applied.

## E2E scenario matrix

| Id | Scenario | Given / canonical inputs | When / trigger | Then / outputs and postconditions | Verification |
|---|---|---|---|---|---|
| PFS-003-01 | full credit lifecycle | eligible owner, sealed ticket, Oracle and liquid vault | pledge, request, settle to closure | one position; principal/collateral close; full collateral returned once | in-process `settlement_releases_collateral_proportionally_and_closes_without_dust` |
| PFS-003-02 | sealed terms survive the quote | pledge ticket sealed at `P₀` | request Credis | position pins `P₀`, floor `P₀+8%`, call `P₀+32%` and the currency policy rate | in-process `request_credis_seals_the_position_geometry_from_the_pledge_quote` |
| PFS-003-03 | settlement gated by the floor | live position below its floor price | settle | revert `not settleable`; crossing the floor latches on the way through, one-way | in-process `settle_is_rejected_until_the_price_crosses_the_floor` |
| PFS-003-04 | interest before principal | latched position with elapsed days | settle below the accrued interest | revert; no principal, collateral or anchor movement | in-process `settle_rejects_a_payment_below_the_accrued_interest` |
| PFS-003-05 | over-payment is not over-pulled | latched position | settle more than `I + P_out` | only `I + P_out` pulled; position closes; exact remaining collateral released | in-process `settle_takes_only_what_the_position_needs` |
| PFS-003-06 | third-party payer | live position owned by another bundle | non-owner settles | accepted; value pulled from the payer, freed collateral released to the original pledger | in-process `settle_accepts_a_third_party_payer` |
| PFS-003-07 | void burns only the unpaid share | called position past its window with `P_out > 0` | begin-block void sweep | burn equals `collateral_locked`; Promis limit credited 1:1; a second sweep is a no-op | in-process `void_sweep_burns_only_the_unpaid_share` |
| PFS-003-08 | settled inside the window | called position settled before the window lapses | begin-block void sweep | never voided; collateral fully reclaimed | in-process `a_position_settled_inside_the_window_is_never_voided` |
| PFS-003-09 | zero smart account | valid ticket/asset but zero bundle | request Credis | revert; no ticket or position mutation | in-process `request_credis_rejects_zero_smart_account` |
| PFS-003-10 | unresolved called position | bundle owns a called position | request another Credis | revert; existing position/ticket/vault state unchanged | in-process `request_credis_rejects_an_owner_with_an_unresolved_call`; **GAP:** unreachable in production until the daily scan arms the call |
| PFS-003-11 | insufficient vault shares | valid ticket but vault cannot withdraw required liquidity | request Credis | revert; ticket consumption and position creation roll back | documentation-only: stateful failing VaultRouter absent |
| PFS-003-12 | settlement deposit failure | live latched position but token/vault deposit fails | payer settles | revert; position, collateral and token state unchanged | documentation-only: failing ERC-20/vault adapter absent |
| PFS-003-13 | restart at transaction boundaries | committed pledge/request/settlement checkpoints | restart after each boundary | reads, tickets, balances and position state reconstruct identically | documentation-only: persistent fixture absent |
| PFS-003-14 | the call arms on a sustained breach | daily reference price at/above the call price for 21 consecutive UTC days | daily scan | position CALLED; 14-day window opens | **GAP:** the daily reference-price scan is not in this chunk |

## Open questions and technical debt

- The call is implemented but **unarmed**: nothing in production reaches `Called` until
  the daily reference-price scan lands, so the begin-block void sweep is inert and a
  non-performing position has no write-off path in the interim.
- A price that crosses the floor and falls back before anyone settles does not latch,
  because the latch is currently evaluated only inside `settle`. The daily scan closes
  this.
- No downside resolution: a position whose price never reaches the floor simply waits.
  See `credis-v2-product-paper.md` §11.1 — adopt or defer is still an open product call.
- Current code does not visibly reserve per-position pledged escrow; the intended
  collateral equation is not proven end to end.
- The originating agent is recorded on the position as `cca` but is not authorized or
  penalized; the CCA program is not in this chunk.
- The in-process lifecycle tests cover the Rust module seam, but no scenario yet
  exercises the production ABI with real ERC-20/vault effects and real enclave calls.
