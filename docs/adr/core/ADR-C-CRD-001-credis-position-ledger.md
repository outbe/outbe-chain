# ADR-C-CRD-001: Credis owns credit positions and the price-path state machine

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17 (revised 2026-08-19 for the daily price-path scan)
- **Decision owners:** Credis protocol maintainers
- **Scope:** `crates/core/credis`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004
- **Related:** ADR-C-CRD-002, ADR-C-VLT-001
- **Supersedes:** Credis ledger portions of former broad pre-space Gratis/economic aggregate (previously numbered 030); the ten-installment anadosis schedule this ADR previously described

## Context

Credis records a borrower's position after pricing and liquidity delivery have been
approved by CredisFactory. It owns position identity, sealed terms, account indexes and
the lifecycle state machine. It does not consume pledge tickets, read Oracle rates, move
ERC-20 assets or release Gratis.

The product model (`credis-v2-product-paper.md`) puts a position on the COEN price path
rather than a calendar: there are no installments, no due dates and no maturity. Time
drives only the interest day count and the response window opened by a call.

## Decision

A position is created once from a globally unique id derived as
`keccak256(pledge_handle ‖ smart_account)`. It seals, and never afterwards changes:
smart account, originating agent, settlement asset and ISO currency, the sealed pledger
ciphertext, principal `P`, collateral `G`, policy rate `r`, entry price `P₀`, the derived
call price `P₀ + 64%`, and the origination timestamp. Only
`outstanding`, `collateral_locked`, the accrual anchor, `called_at` and `state` move over
the position's life.

The FSM is explicit and stored as a `u8` decoded through `CredisState::from_u8`, so an
unknown byte is rejected rather than silently reinterpreted:

```text
Open --mark_called--> Called
Open|Called --settle(P_out -> 0)--> Settled            (terminal)
Called --void_position--> Void                         (terminal)
```

A position is settleable from the moment it opens; no price condition gates settlement.
`Open -> Called` is the sustained-breach trigger. Both `Settled` and `Void` are terminal.

**Interest** is not accrued per block. It is computed at settlement as simple,
non-compounding, ACT/365 interest on the outstanding principal over the **whole** UTC
days elapsed since the accrual anchor, rounded up so rounding favours the protocol. The
anchor starts at origination and advances by exactly the whole days each settlement
charges — never to the settlement timestamp. Advancing it to `now` would discard the
sub-day remainder, and because a sub-day settlement charges nothing, repeated dust
payments just under 24 hours apart would hold the day count at zero and evade the coupon
entirely.

**Settlement** applies interest first and principal second. A payment below the accrued
interest is rejected outright rather than partially applied. Only what the position needs
is consumed, so an over-payment is not over-pulled. Collateral release is
principal-proportional against the original principal and floored; the closing settlement
short-circuits to the exact remaining locked collateral so no dust is stranded.

**Void** writes off only the unpaid share. Settlement remains open on unchanged terms
throughout the call window, so whatever the owner settled they have already reclaimed.

## Authority and interfaces

The public ABI is enumerable rather than array-returning: `totalSupply` /
`positionByIndex` walk the global creation order, `balanceOf` /
`positionOfAddressByIndex` walk one owner's index, and `getPosition` / `ownerOf` address
a single position. A caller therefore paginates instead of forcing an unbounded return.
`ownerOf` takes the position id as its 32-byte big-endian form; any other length is
rejected rather than zero-extended, so a truncated id cannot silently address a
different position. An out-of-range index fails rather than returning a zeroed record.
`accruedInterest` reads the block timestamp from storage rather than calldata, so it is
deterministic. `credisPrincipalAndOutstandingOf` returns both sums from one walk of the
owner index, so the two can never be read at different heights. Opening, latching,
calling, settling and voiding are privileged internal APIs intended only for
CredisFactory. The ledger trusts factory-supplied sealed inputs only after validating
local representational invariants.

Settlement is deliberately payable by any caller: the collateral released is owed to the
pledger recorded on the position, so a payer can never redirect value to themselves.

## Persistent state and invariants

- Every position id is unique and points to one nonzero smart account.
- Every account index entry points to an existing position owned by that account; every
  position appears exactly once in its owner's dense index.
- A position appears in the dense active index iff its state is non-terminal
  (`Open` or `Called`); `Settled` and `Void` are swap-popped out, and
  `active_position_index` always holds the entry's current slot. This is what lets the
  daily scan visit only the positions that can still transition instead of the whole book.
- `called_position_counts[account]` equals the number of that account's positions in
  `Called`. It is bumped by `mark_called` and dropped by whichever transition resolves the
  call — the settlement that closes the position, or the void — so `has_called_position`
  is an O(1) read rather than a walk.
- Sealed terms are immutable after creation.
- `Σ principal settled + outstanding + principal written off = P`.
- `Σ released collateral + collateral_locked + burned collateral = G`.
- Because each partial release is floored, `collateral_locked >= floor(G × P_out / P)`,
  with the drift always toward the protocol; the closing settlement releases the exact
  remainder.
- `outstanding == 0` iff the state is `Settled`, for any position that ever settled fully.
- The accrual anchor is monotonically non-decreasing and never exceeds the current time.
- No unpaid interest carries between settlements: accrual restarts on the reduced
  principal.

Saturating subtraction is forbidden for invariant closure: a settlement larger than
outstanding state must fail as corruption, and every arithmetic path uses `checked_*`
returning `ArithmeticOverflow`.

## Atomicity, replay and failure

Position creation writes the record and both owner indexes in one EVM frame. Settlement
bookkeeping is rolled back with CredisFactory's token pull, vault deposit and collateral
release. Position identity guards duplicate creation; the terminal states guard replay —
a `Settled` or `Void` position rejects further settlement with `PositionClosed`.

Missing record, closed position and a payment below accrued interest
are business/state errors. Broken indexes, arithmetic mismatch and underflow are
invariant failures. No getter may silently skip corrupt records and still report a
healthy position.

## Determinism, bounds and compatibility

The interest formula and its rounding direction, the day-count convention, the call
markup, the currency/rate scale, field widths, the state discriminants and
position-id derivation are consensus formats. Changes require migration and before/after
vectors. Per-account scans require a maximum or pagination before they may be used in
transaction admission at unbounded size.

The v2 layout is a **clean break**: `Position` and the contract's top-level slots were
renumbered wholesale with no reserved or deprecated fields, on the accepted basis that no
environment holds live Credis positions. Any such environment must be re-genesised. On the
same basis, dropping `floor_price` and the `Settleable` discriminant (2026-08-24) closed
the gaps rather than reserving them.

## Production-interface verification evidence

Inspected schema, opening arithmetic, position/account indexing, the interest day count
and its anchor, settlement ordering and collateral release, the state machine including
the terminal guards, and ABI reads. Tests cover the product paper's §5 worked example end
to end, interest/collateral rounding directions in both directions, the dust-settlement
accrual-evasion regression, and rejection of unknown state bytes. They do not yet cover
generated closure over arbitrary terms, corruption, or all factory rollback points.
Status remains Proposed.

## Consequences

Credis becomes a pure position-state module. Pricing, assets, liquidity and the
confidential pledger ledger remain in CredisFactory/Oracle/VaultRouter/Gratis and can
fail without weakening its FSM. Because interest is evaluated lazily, no per-block
accrual state exists and nothing schedules off a position's creation date.

## Rejected alternatives

- **Infer state from events:** events do not own position state.
- **Accrue interest per block:** would add per-block work and per-position state for a
  quantity that is exactly reconstructible from two timestamps.
- **Advance the accrual anchor to the settlement time:** discards the sub-day remainder
  and makes the whole coupon evadable with dust payments.
- **Re-read rates each settlement:** recorded obligations would change over time.
- **Saturate outstanding amounts:** corruption would masquerade as completion.

## Open questions and technical debt

1. **Resolved 2026-08-18** — outstanding subtraction now uses `checked_sub` with an
   explicit `ArithmeticOverflow` invariant failure.
2. **Resolved 2026-08-18** — there are no due dates, so early payment is not a concept.
   Settlement is open at any time from the moment the position opens.
3. **Resolved 2026-08-18** — position status is now an explicit stored `u8` decoded via
   `CredisState::from_u8`, replacing the cursor-derived status.
4. **Resolved 2026-08-19** — the `credis_call_daily` scan (ADR-C-CRD-002) arms the call
   from the finalized daily reference series, so `Called` and the write-off are both
   reachable in production. Acceleration and restructuring remain undefined and out of
   scope: the product has no due dates to accelerate.
5. **Resolved 2026-08-18** — the interest path is fully `checked_*`; the month arithmetic
   it referred to no longer exists.
6. **Resolved 2026-08-18** — obsolete: there are no installments, and the day count is
   ACT/365 over whole UTC days.
7. Add a generated model proving the principal and collateral closure equations, anchor
   monotonicity and account indexes over arbitrary terms and bounds.
8. Add corruption tests for wrong owners, duplicate account entries and impossible
   state/outstanding combinations.
9. **Partially resolved 2026-08-19** — the ABI no longer returns unbounded position
   arrays; callers enumerate through `positionByIndex` / `positionOfAddressByIndex`, and
   `has_called_position` is now the O(1) `called_position_counts` read this item asked
   for. `credisPrincipalAndOutstandingOf` still walks every position an account owns, so
   that one read remains unbounded; bound it with running per-account totals.
10. Define stable position-id domain separation beyond pledge-handle uniqueness and add
    collision/reference vectors.
11. Prove the opening/settlement/void APIs have no caller except CredisFactory.
12. Define historical retention after closure and whether terminal positions may ever be
    pruned without breaking auditability.
13. Decide the product paper's §11.1 downside resolution: a position whose price never
    reaches its call price currently waits forever with no write-off path.
14. The originating agent is recorded as `cca` but is neither authorized at opening nor
    held accountable at void; the CCA program is not yet implemented.
