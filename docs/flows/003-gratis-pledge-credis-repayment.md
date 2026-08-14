# PFS-003: A Gratis pledge opens a Credis position and the price path resolves it

- **Status:** Draft
- **Actors:** Gratis holder (pledger EOA), card bundle (smart account), CCA
  (Credis Card Agent), GratisFactory, Gratis + enclave, CcaRegistry, CredisFactory,
  Credis, Oracle, VaultRouter + reserve vault, Fidelity, PromisLimit, Cycle
- **Trigger:** The pledger locks Gratis as collateral; the CCA then presents the
  pledge handle to open a position
- **Topology/services:** Finalizing network with an attested TEE enclave, a
  registered `COEN/<iso>` oracle pair carrying both a live spot rate and a
  published policy rate, a funded per-currency reserve vault whose router lists
  both factories as liquidity targets, and the Cycle triggers running
- **Referenced ADRs:** ADR-C-GRT-001, ADR-C-GRT-002, ADR-C-GRT-003, ADR-C-FID-001,
  ADR-C-CRD-001, ADR-C-VLT-001, ADR-S-ORC-001
- **Supersedes:** The pre-TEE revision of this document, which described a
  ZK commitment/nullifier pool, a ten-installment `anadosis` schedule and
  per-installment reclaim notes. None of that exists: the shielded pool is now an
  enclave-sealed pledge ticket, and the position runs on the COEN price path with
  no schedule at all.

## Outcome

One confidential Gratis pledge collateralizes exactly one Credis position; the
borrower receives stablecoin at the price the pledger accepted; and the position
closes either by repayment — releasing collateral in proportion to principal
covered — or, after a sustained price breach and a lapsed response window, by
voiding only the unpaid remainder.

## Acceptance contract

- **Source:** A Gratis holder acting through a registered CCA and a card bundle.
- **Trigger:** `pledgeGratis`, followed within the quote TTL by `requestCredis`.
- **Environment:** Finalizing network; enclave available; `COEN/<iso>` pair
  registered with a live rate, a finalized daily VWAP series and a non-zero policy
  rate; reserve vault holding the disbursement currency; CCA registered and out of
  quarantine.
- **Canonical inputs:** Requested stablecoin amount and asset; the pledger's Gratis
  modify-key MAC and op-nonce; the spend authorization binding the ticket to a
  bundle; oracle spot rate, daily VWAP series and policy rate; block timestamp.
- **System under test:** GratisFactory, Gratis + enclave, CcaRegistry,
  CredisFactory, Credis, Oracle, VaultRouter, Fidelity, PromisLimit.
- **Expected response:** A pledge handle; a position whose geometry is sealed from
  the pledge quote; disbursed stablecoin; per-settlement collateral releases; and a
  terminal `Settled` or `Void`.
- **Response measures:** Collateral conservation — the sum of releases plus any
  burn equals `G` exactly, with nothing stranded. Principal conservation — the sum
  of principal covered plus principal written off equals `P`. The pledger EOA never
  appears on-chain in plaintext.
- **Failure guarantee:** Any rejected step reverts its whole transaction: the
  ticket is not consumed, no position exists, no stablecoin moves, and the
  pledger's confidential balances are unchanged.

## Notation

| Symbol | Meaning |
|---|---|
| `P` | principal — stablecoin minor units disbursed (6 decimals) |
| `P_out` | outstanding principal; the position closes when it reaches zero |
| `G` | collateral — pledged Gratis (18 decimals) |
| `G_locked` | the share of `G` still locked |
| `P₀` | entry price — COEN in the position's currency at pledge time (1e18) |
| `r` | policy rate — the currency's annual central-bank rate (1e18), pinned at open |
| `m` | the CCA's performance multiplier (1e18), from 1 downward |

Floor price is `P₀ × 1.08`; call price is `P₀ × 1.32`. Both are sealed at open and
never move.

---

## Step 0 — Prerequisites (not part of the Credis flow)

### 0a. The pledger holds Gratis

**Preconditions.** Gratis reaches a holder either from Nod settlement
(`outbe_nodfactory` mints it as part of the daily cycle) or from
`IGratisFactory.mineFromPromis`, which burns confidential Promis 1:1. There is no
public mint.

**State out.** `balance_ct[pledger]` is non-zero. Balances are enclave ciphertext;
the holder reads them by decrypting `IGratis.balanceOf` with their view key.

### 0b. The CCA is registered

| | |
|---|---|
| **Call** | `ICca.register()` at `0x1019` |
| **Caller** | the agent itself — permissionless, no allowlist |
| **Preconditions** | `msg.sender` is non-zero and not already registered |
| **State in** | no `CcaRecord` for `msg.sender` |
| **State out** | `CcaRecord { multiplier: 1e18, registered_at: now, state: Active }`; `CcaRegistered` emitted |
| **Rejects** | `already registered`, `invalid cca address` |

> **Known gap.** §8.1 of the product paper prices identity reset with a COEN bond.
> The bond is not implemented, so registration is currently free and a penalized
> agent can re-register fresh. The accountability arithmetic below is complete; its
> economic deterrent is not.

### 0c. The asset is priced, funded, and rated

Three separate registrations have to exist before a position in `asset` can run its
course. They are enforced at three different moments, and none of them is a single
admissibility flag:

| Requirement | Enforced by | When |
|---|---|---|
| a registered `COEN/<iso>` pair | `coenRateFor` reverts without it | `pledgeGratis`, at pricing |
| a reserve vault holding enough `asset` | `IVaultRouter.reserve` reverts on a share shortfall | `pledgeGratis`, as the credit is claimed |
| a non-zero policy rate for `iso` | `getCurrencyRate` reverts without it | `requestCredis`, when the rate is pinned |

Genesis seeds USD/840; any other currency needs `registerPair`, a vault added via
`addVault`, plus a system-only `setCurrencyRate`.

> **Operational prerequisite.** GratisFactory (`0x2003`) must be registered as a
> vault-router **liquidity target** — `addLiquidityTarget(GRATIS_FACTORY_ADDRESS,
> StablesTarget.Credis)`, an owner call — exactly as CredisFactory already is.
> Taking assets out of a vault is what that registry authorizes, and `pledgeGratis`
> now does so. Nothing in this repository registers targets; it is a deployment
> runbook step, and until it is done every `pledgeGratis` reverts.

---

## Step 1 — `pledgeGratis`

| | |
|---|---|
| **Call** | `IGratisFactory.pledgeGratis(amountStables, asset, maxGratis, mac, opNonce)` at `0x2003` |
| **Caller** | the pledger EOA (this is the only step that authenticates the pledger) |
| **Returns** | `pledgeHandle` (bytes32) |

**Preconditions**

1. `asset != 0` and `amountStables != 0`.
2. The asset self-reports an ISO 4217 code via `IReferenceCurrency.isoCode()`.
3. `(mac, opNonce)` is a valid Gratis modify authorization: `opNonce` equals the
   caller's current `IGratis.opNonceOf(caller)`, and the MAC binds
   `amountStables` under the caller's modify key.
4. The caller's confidential Gratis balance covers the derived cost.
5. Fidelity eligibility: the enclave-returned league is not `u16::MAX`.
6. `gratis_cost <= maxGratis` — the caller's slippage cap, authenticated by their
   transaction signature rather than the MAC.
7. The reserve vault can cover `amountStables` — enforced by taking it, not by
   testing for it. See **the claim** below.

**Pricing.** The cost is derived here and nowhere else:

```
gratis_cost = ceil(amountStables × 1e12 × 1e18 / spot_rate)
```

rounded **up**, so the collateral always covers the credit. `spot_rate` is
`COEN/<iso>` at this block. This is the quote the pledger accepts.

**The claim.** The credit is *reserved*, not merely checked. `pledgeGratis` calls
`IVaultRouter.reserve(pledgeHandle, asset, amountStables)`, which redeems the assets
out of the reserve vault into the router's own custody, wired to the handle. A
liquidity check here would not survive the gap to `requestCredis` — any other
withdrawal can take the same shares in the meantime — so the pledge holds the assets
instead. This is what makes the delivery a guarantee rather than a hope.

The debit and the claim are **atomic**: `pledge_gratis` wraps them in one storage
checkpoint, so a vault that cannot pay leaves the pledger with their full balance
and no ticket. A pledger debited without a claim is the exact failure this removes.

**State transition**

| | in | out |
|---|---|---|
| `balance_ct[pledger]` | `B` | `B − gratis_cost` |
| `pledged_ct[pledger]` | `Q` | `Q` — **unchanged**; the amount is parked in the ticket, not the pledged ledger |
| pledge ticket | absent | sealed record holding `{stables_amount, gratis_amount, asset, entry_rate}` and the pledger EOA |
| `pledge_quoted_at[handle]` | 0 | `block.timestamp` |
| `pledge_queue` | `[…]` | `[…, handle]` — appended; the TTL is constant, so insertion order is expiry order |
| vault shares held by the router | `S` | `S − previewWithdraw(amountStables)` |
| `reservationOf(handle)` | `(0, 0)` | `(asset, amountStables)` in router custody |

**Outputs.** `pledgeHandle`, `gratis_cost`, a `GratisPledged` event, and the
router's `ReservationCreated`.

> **Privacy boundary.** `amountStables`, `gratisAmount` and the handle are all
> public — calldata and event. Only *cumulative balances* are encrypted. What the
> ticket protects is the **pledger's identity**: the EOA is sealed inside it and
> never appears on-chain in plaintext.

**Off-chain.** The pledger derives `pledgeSecret` from their modify key and the
handle, then computes `spendAuth = HMAC(pledgeSecret, "credis-bind" || bundle)`.
They hand the handle and that authorization to the CCA. A mempool observer who
copies the handle cannot redirect the loan, because the enclave checks the
authorization binds the ticket to the named bundle.

### Step 1-alt — `unpledgeGratis`

If the pledge is never spent, `unpledgeGratis(amountStables, handle, mac, opNonce)`
returns the full collateral to `balance_ct[pledger]`, deletes the ticket, clears the
quote timestamp, and **returns the reservation to the vault** — the credit is no
longer owed to anyone, so it goes back to earning rather than waiting for the sweep.
`amountStables` must match the figure sealed in the ticket.

The queue entry is left behind as a tombstone; the sweep drops it on sight. This
call works before **and** after the quote expires: the ticket outlives the quote,
which is what makes it the pledger's recovery path (see Step 1-exp).

### Step 1-exp — the quote lapses

Driven by Cycle trigger 6 (`pledge_reservation_sweep`, every 300s) or by anyone
calling `IGratisFactory.sweepExpiredPledges(max)`.

**Preconditions**

1. `pledge_quoted_at[head] != 0` — a zero is a spent/unpledged tombstone, popped
   and skipped without touching the router.
2. `now > pledge_quoted_at[head] + PLEDGE_QUOTE_TTL_SECS` — strictly after; the
   deadline itself is still live.

The queue is walked from the head only. `PLEDGE_QUOTE_TTL_SECS` is a constant, so
insertion order **is** expiry order: a head that is not yet due ends the run, which
is why a scheduled sweep with nothing to do costs two storage reads.

**State transition**

| | in | out |
|---|---|---|
| `reservationOf(handle)` | `(asset, amount)` | `(0, 0)` — deposited back into the vault |
| `pledge_quoted_at[handle]` | `t` | 0 — the quote can never be exercised |
| `pledge_queue` | `[handle, …]` | `[…]` |
| pledge ticket | present | **present — unchanged** |
| `balance_ct[pledger]` | `B` | `B` — **unchanged** |

> **The collateral is not released.** Expiry returns the *stablecoin claim*; the
> pledger's GRATIS stays in the ticket until they call `unpledgeGratis` themselves.
> `PledgeQuoteExpired(pledgeHandle, quotedAt)` is emitted so a client can detect the
> lapse and prompt them. Automating it would need a new unauthenticated
> `GratisOp::ExpirePledge`, because `Unpledge` is an owner op gated on a MAC derived
> from a key that never leaves the enclave — a postcard wire change, an
> `inputs_canonical_hash` extension, and a new MRENCLAVE.

A handle whose vault deposit fails is popped anyway rather than blocking the head
forever; its assets stay in router custody and stay recoverable through the
permissionless `returnReservation`.

---

## Step 2 — `requestCredis`

| | |
|---|---|
| **Call** | `ICredisFactory.requestCredis(smartAccount, pledgeHandle, spendAuth)` at `0x1009` |
| **Caller** | the **CCA** — recorded on the position as the originating agent |
| **Returns** | `(positionId, amountStables)` |

**Preconditions, in the exact order they are checked**

| # | Check | Rejects with |
|---|---|---|
| 1 | `smartAccount != 0` | `invalid smart account address` |
| 2 | `Cca.canOriginate(msg.sender)` — registered, active, `m ≥ 0.5` | `cca is not permitted to originate` |
| 3 | the bundle has no `Called` position | `owner has an unresolved called position` |
| 4 | `spendAuth` binds the ticket to `smartAccount` (verified **inside the enclave**) | enclave rejection |
| 5 | the ticket's asset is non-zero | `invalid asset address` |
| 6 | quote age `≤ PLEDGE_QUOTE_TTL_SECS` (15 min) | `pledge quote has expired` |
| 7 | matched funding: `IERC20(asset).balanceOf(smartAccount) ≥ P` | `... does not hold matching funds` |
| 8 | the bundle holds `< MAX_OPEN_POSITIONS_PER_OWNER` (16) live positions | `maximum number of open positions` |
| 9 | no position already exists for this `(handle, bundle)` | `position already exists` |

Checks 1–3 run **before** the ticket is consumed, so a quarantined agent or a
blocked owner cannot burn a pledger's ticket. Everything after that reverts the
whole transaction on failure, which restores the ticket anyway.

**Nothing is re-priced here.** `P`, the asset and `P₀` all come out of the ticket,
so the loan is issued at the price the pledger accepted rather than whatever the
oracle reads a transaction later. The only value read fresh is `r`, because the
policy rate belongs to the loan rather than to the collateral.

**State transition**

| | in | out |
|---|---|---|
| pledge ticket | sealed record | deleted |
| `pledged_ct[pledger]` | `Q` | `Q + G` — the collateral lands in the pledger's **own** ledger; there is no escrow account |
| `pledge_quoted_at[handle]` | `t` | cleared — which tombstones the queue entry |
| position | absent | `Position` (below) |
| `reservationOf(handle)` | `(asset, P)` | `(0, 0)` — released, not re-withdrawn |
| `IERC20(asset)` | router custody holds `P` | `smartAccount` holds `+P` |
| CCA day units | `u` | `u + unit` (see §8.3 rules) |

**The delivery cannot fail for want of liquidity.** `requestCredis` calls
`releaseReservation(pledgeHandle, smartAccount)`, handing over assets the pledge
already took out of the vault. There is no vault withdrawal at this step and so no
race with other borrowers — that was the whole point of reserving at Step 1.

The created position:

```
position_id      = keccak256(pledge_handle ‖ smart_account)
smart_account    = the bundle              cca            = msg.sender
asset, issuance_currency                   eoa_ct         = sealed pledger EOA
principal        = P                       outstanding    = P
collateral       = G                       collateral_locked = G
policy_rate      = r × POLICY_RATE_FACTOR   entry_price   = P₀
floor_price      = P₀ × 1.08               call_price     = P₀ × 1.32
originated_at    = last_settled_at = now   called_at      = 0
state            = Open
```

**Origination units (§8.3).** The agent is credited `1` unit, halved once per prior
position it opened for this same owner (`1, ½, ¼, …`), subject to a **$100 minimum
principal** and **at most one unit per owner per day**. An origination that earns
nothing consumes neither the day slot nor a decay step. The multiplier is *not*
folded in here — it is applied at distribution time, so a later recovery is not
lost to a frozen weight.

**Outputs.** `CredisRequested`, `PositionCreated`, and `OriginationRecorded` when a
unit was earned.

---

## Step 3 — The floor latch: `Open → Settleable`

A position cannot be settled until the price has exceeded its floor **at least
once**. The latch is one-way: a later fall does not re-lock it. There are two
producers.

**3a. The daily scan** (`credis_call_daily` Cycle trigger, `period = 86_400`).
Latches any `Open` position whose currency's **last closed UTC day's VWAP** exceeded
its floor.

**3b. On demand inside `settle`.** Reads the **live spot** rate instead, so a
settler who sees the price above their floor right now never waits for the next
daily run.

| | in | out |
|---|---|---|
| `state` | `Open` | `Settleable` |

Emits `PositionSettleable`. Idempotent — re-latching returns without effect.

> **Known ceiling.** The scan reads the daily reference and the on-demand path
> reads live spot, so an intraday spike that clears the floor, does not lift the
> day's VWAP, and that nobody settles into, will not latch. This is
> protocol-favoring and consistent with the call rule, which the paper explicitly
> puts on the daily series. A per-currency floor bin index scanned every block is
> the upgrade path.

---

## Step 4 — `settle` (repeatable, any amount, any time)

| | |
|---|---|
| **Call** | `ICredisFactory.settle(positionId, amount)` |
| **Caller** | **anyone** — including a third party paying on the owner's behalf |
| **Returns** | `totalPaid` |

**Preconditions**

1. `amount != 0`.
2. `state ∈ {Settleable, Called}`. `Open` rejects with `not settleable`;
   `Settled`/`Void` reject with `position is closed`.
3. `amount ≥ I`, the interest accrued since the last settlement. Query it first via
   `ICredis.accruedInterest(positionId)`; a payment below it is rejected.
4. The caller's stablecoin balance and allowance cover `totalPaid`.

**Why an open payer is safe without an access check.** The debt is pulled from
`msg.sender`'s own balance, while the freed collateral always goes to the pledger
EOA sealed on the position and recovered through the enclave. A payer can never
redirect value to themselves.

**Arithmetic**

```
d   = whole UTC days since last_settled_at
I   = ceil(P_out × r × d / (365 × 1e18))        interest, rounded UP
p   = min(amount − I, P_out)                    principal covered
ΔG  = if p == P_out { G_locked }                final: exact remainder, no dust
      else { floor(G × p / P) }                 partial: rounded DOWN
```

Rounding always favors the protocol; the settlement that clears the last principal
releases exactly what remains, so nothing is stranded.

**State transition**

| | in | out |
|---|---|---|
| `outstanding` | `P_out` | `P_out − p` |
| `collateral_locked` | `G_locked` | `G_locked − ΔG` |
| `last_settled_at` | `t` | `now` — accrual restarts at `d = 0` |
| `state` | `Settleable`/`Called` | `Settled` if `P_out` reached 0, else unchanged |
| caller's stablecoin | `X` | `X − (I + p)` |
| reserve vault | `V` | `V + (I + p)` |
| `pledged_ct[pledger]` | `Q` | `Q − ΔG` |
| `balance_ct[pledger]` | `B` | `B + ΔG` |
| CCA multiplier | `m` | recovers one step per $1,000 of **principal** repaid, capped at 1 |

Only `p` — not interest — counts toward CCA recovery: §8.4 restores standing for
the book repaying, not for revenue.

**Outputs.** `SettlementApplied`, plus `PositionSettled` when it closes.

**Over-payment.** Only what the position needs is pulled. Sending far more than the
balance owed charges exactly `I + P_out`.

---

## Step 5 — The call: `Settleable → Called`

Driven **only** by the daily Cycle trigger; there is no manual entry point.

**Preconditions**

1. The oracle's finalized-VWAP watermark covers the last closed UTC day. If it
   lags, the run logs and skips rather than misreading an unfinalized day as
   missing data.
2. The position is `Settleable`.
3. The daily reference price sat **at or above** the call price for **21
   consecutive UTC days**.
4. The whole 21-day window is at or after the position's origination day — a
   position cannot inherit a streak that predates it.

The consecutive rule is evaluated as `min(vwap[d−20 ..= d]) ≥ call_price`, which
needs no per-position streak state: one rolling minimum per currency decides every
position denominated in it. A day with **no published price resets the streak**
(§11.3 is still an open product decision; resetting is the conservative reading —
it can only delay a call, never trigger one spuriously).

| | in | out |
|---|---|---|
| `state` | `Settleable` | `Called` |
| `called_at` | 0 | `now` |
| owner's called count | `n` | `n + 1` — blocks new positions until resolved |

Emits `PositionCalled` carrying `settlementDeadline = called_at + 14 days`.

**Being called changes only two things:** a 14-day window opens, and the owner
cannot open new positions. Settlement stays open on completely unchanged terms
throughout — Step 4 applies verbatim.

---

## Step 6 — The void: `Called → Void`

Same daily scan, third arm.

**Preconditions**

1. `state == Called`.
2. `now ≥ called_at + 14 days`.
3. `outstanding > 0` — a position fully settled inside the window is never voided.

**Only the remainder is voided.** Because every settlement already released its
proportional share, `collateral_locked` is by construction `G × P_out / P`.
Whatever the owner settled they have already reclaimed.

| | in | out |
|---|---|---|
| `outstanding` | `P_out` | 0 — written off, never collected |
| accrued interest on it | `I` | written off, never collected |
| `collateral_locked` | `G_locked` | 0 |
| `pledged_ct[pledger]` | `Q` | `Q − G_locked` (burned) |
| Gratis total supply | `S` | `S − G_locked` |
| Fidelity cohort | — | an `Out` (sale) cohort recorded for the pledger |
| Promis Reserve unallocated | `R` | `R + G_locked` |
| `state` | `Called` | `Void` |
| CCA multiplier | `m` | `m × (1 − 0.10 × unpaid_share)` |

Nothing is market-sold and nothing is collected: the burned collateral becomes
invest-side capacity instead. The fidelity cohort drop rides inside the *same*
enclave round-trip as the burn — no extra trip.

Emits `PositionVoided` with the burn and both write-off amounts.

> **Timing.** The void lands at the next daily tick after the window lapses, so up
> to ~24h late, and the owner can still settle during that grace. Accepted;
> `outbe_gem` behaves the same way.

---

## Boundaries and conservation

Every user-facing step is its own EVM transaction and rolls back whole. The
latch/call/void arms run inside the `CycleTick` **system transaction**; each
position is additionally wrapped in its own storage checkpoint, so one bad
position is logged and skipped rather than halting the daily run. The scan handler
never returns an error for missing market data — a revert there would fail the
block.

**Collateral closes exactly:**

```
Σ ΔG(settlements) + G_burned = G
```

**Principal closes exactly:**

```
Σ p(settlements) + P_written_off = P
```

Worked example (paper §5, `P = $1,000`, `G = 2,000 COEN`, `P₀ = $0.50`, `r = 4%`):
releases of `778.082190` and `751.123286` plus a burn of `470.794524` sum to
`2,000.000000` G; principal covered of `389.041095` and `375.561643` plus
`235.397262` written off sum to `$1,000.000000`. This is pinned as a single
regression test.

**Replay protection** spans transactions through the Gratis op-nonce (each modify
authorization is single-use), the one-shot pledge ticket, and
`position_id = keccak256(handle ‖ bundle)` uniqueness.

## Observable completion contract

| Question | Authoritative read |
|---|---|
| position terms and state | `ICredis.getPosition(positionId)` |
| what must I pay at minimum? | `ICredis.accruedInterest(positionId)` |
| is the owner blocked? | `ICredis.hasCalledPosition(smartAccount)` |
| pledger's confidential balances | `IGratis.balanceOf` / `pledgedOf`, decrypted with the view key |
| agent standing | `ICca.canOriginate` / `multiplierOf` |
| is my credit still held for me? | `IVaultRouter.reservationOf(pledgeHandle)` |
| can this asset fund a pledge right now? | `IVaultRouter.hasLiquidity(asset, amount)` — a preflight; it checks, it does not claim |

Events are the audit trail; storage reads are authoritative if they disagree.

## Replay, retry, restart and failure

- **Retry key** for `requestCredis` is `(pledgeHandle, smartAccount)`: a second
  attempt fails on the consumed ticket, and would fail on `position already exists`
  even if the ticket survived.
- **`settle` is not idempotent** — it is intentionally repeatable. Callers must not
  blind-retry a settlement whose receipt they did not see; re-reading `outstanding`
  is the safe recovery.
- **Latch, call and void are all idempotent**: each returns without effect when the
  position is already past that state.
- **A missed daily tick** is caught up one slot per block. Because the scan
  recomputes from oracle history rather than from carried state, a catch-up run
  reaches the same conclusion.
- **A cold oracle** blocks nothing that was already latched: the on-demand latch
  uses a non-reverting read and simply declines to latch.
- **The expiry sweep is level-triggered and idempotent.** Trigger 6 sets
  `coalesce_missed_slots`, so a gap resolves to one run at the newest elapsed slot
  rather than a per-slot backlog — re-walking the same queue N times would be
  wasted work, and would make two blocks at one timestamp differ. Every other
  trigger keeps the one-slot-per-block catch-up, where each slot is its own event.
- **A reservation is never stranded.** `returnReservation` is permissionless, so a
  handle the sweep could not unwind stays recoverable by anyone.

## E2E scenario matrix

| Id | Scenario | Required assertions | Automated by |
|---|---|---|---|
| PFS-003-01 | pledge → request → partial settle → full settle | geometry sealed from the quote; collateral released proportionally; closes with zero `collateral_locked` | `credisfactory::tests::e2e` |
| PFS-003-02 | settle rejected before the latch, accepted after | `not settleable`; latch is one-way across a price fall | `credisfactory::tests::e2e` |
| PFS-003-03 | third-party payer | payer's ledgers stay zero; pledger receives the collateral | `credisfactory::tests::e2e` |
| PFS-003-04 | 21-day streak calls; 20 days, one below-day, one missing day do not | state, `called_at`, deadline | `credisfactory::tests::called` |
| PFS-003-05 | window lapses with a remainder | burn == `collateral_locked`; Promis limit credited exactly; second run is a no-op | `credisfactory::tests::called` |
| PFS-003-06 | settled inside the window is never voided | nothing burned; whole pledge returned | `credisfactory::tests::e2e` |
| PFS-003-07 | §7 owner rules | unmatched funding, stale quote and the per-owner cap each reject | `credisfactory::tests::e2e`, `credis::tests` |
| PFS-003-08 | §8.4 agent accountability | void penalizes; repayment restores; quarantine blocks origination | `cca::tests`, `credisfactory::tests::e2e` |
| PFS-003-09 | multi-currency | two positions latch/call off their own daily series | `credisfactory::tests::called` |
| PFS-003-10 | localnet client path | `1-pledge-gratis` → `3-request-credis` → settle | GAP — the example script is still named `5-user-pays-anadosis.ts` |
| PFS-003-11 | dry reserve vault | `pledgeGratis` locks nothing — no debit, no ticket, no queue entry; a reservation cannot exceed the vault's shares | `gratisfactory::tests`, `vaultrouter::tests` |
| PFS-003-12 | a claim is held, delivered once, and gated | `reserve` → `releaseReservation` delivers exactly once; double-release and double-reserve reject; both legs answer to the liquidity-target registry | `vaultrouter::tests` |
| PFS-003-13 | quote lapses unspent | credit returns to the vault, quote cleared, `PledgeQuoteExpired` emitted, **ticket and collateral untouched**, and `unpledgeGratis` still works afterwards | `gratisfactory::tests` |
| PFS-003-14 | sweep ordering and budget | live head ends the run; a spent handle is a tombstone; a backlog drains oldest-first within `max` | `gratisfactory::tests` |

## Open questions and technical debt

1. **The CCA bond does not exist.** §8.1/§8.2 — dynamic requirement, delegated
   senior layer, 21/128-day cooldowns, exit haircut. Until it lands, registration
   is free and the §8.4 penalty has no economic teeth.
2. **The CCA reward pool is still a pure accumulator.** `PoolKind::Cca` credits
   `CCA_ADDRESS` rather than distributing against the origination units this flow
   records. The 32%-capped distribution and the activation sweep to Metadosis are
   not implemented.
3. **No downside resolution.** A position whose price never reaches the floor waits
   forever — nothing forces closure on depreciation. Deferred by decision, but it
   also means no CCA penalty can fire in a flat or falling market.
4. **A 0% policy rate is unrepresentable.** Zero doubles as the "no rate published"
   sentinel, so `setCurrencyRate` rejects it — wrong for the ECB 2016–2022 and the
   BoJ. Needs an explicit presence flag beside the rate.
5. **Missing-data days in the call streak** (§11.3) reset rather than pause the
   count. Placeholder, not a decision.
6. **`releaseToEoa` and `burnPledged` are unauthenticated at the enclave boundary.**
   The Credis position is the sole accounting authority for both, so any bug in the
   settlement math is directly a collateral-integrity bug, with no second line of
   defence.
7. **Amounts are public.** Only identity is protected. Do not over-claim amount
   privacy in client-facing docs.
7b. **Expiry does not return the pledger's collateral.** The sweep frees the vault
   credit; recovering the GRATIS is a manual `unpledgeGratis`. Automating it needs
   an unauthenticated enclave op and a new MRENCLAVE — see Step 1-exp.
7c. **A live quote parks vault liquidity.** Reserved assets earn nothing for the
   quote's life, and cheap pledges could hold liquidity 15 minutes at a time. The
   GRATIS cost is the only deterrent today; a per-account cap on live reservations
   is the upgrade path if that proves insufficient.
8. **All §10 TBDs ship as placeholder constants:** quote TTL 15 min, position cap
   16, dust guard $100, recovery $1,000/step, policy-rate factor ×1.
