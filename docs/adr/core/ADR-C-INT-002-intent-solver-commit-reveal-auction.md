# ADR-C-INT-002: Intent solver auction selects one bounded canonical winner

- **Status:** Proposed; current Solidity implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `contracts/intent/src/Auction.sol`
- **Depends on:** ADR-C-INT-003

## Context

Solvers commit and reveal output quotes for an intent order. The router resets auction
state after consuming a winner. Auction timing and tie-breaking directly determine who
may claim and receive origin input.

## Decision

Per order, auction is `Absent -> Commit -> Reveal -> Ended -> Consumed`. Commitment
binds solver, order id, output amount and salt under one domain-separated version.
At most `maxQuotesPerOrder` valid reveals are stored. Winner is greatest output amount,
with a canonical deterministic tie-break independent of transaction/storage quirks.
Only Router may consume/reset and it records the selected winner in the order transition.

## Authoritative interfaces

Public `commit`, `reveal` and winner/read methods own solver participation. Router-only
`resetAuction` owns consumption. Owner timing/limit/router setters are activation changes.

## Invariants

- One solver has at most one live commitment and accepted reveal per order/version.
- Reveal matches commitment and canonical order bytes before entering ranking.
- Winner belongs to accepted quotes and satisfies the user's minimum output.
- Deadline phases are disjoint and configuration cannot change an active auction.
- Consumed/reset state cannot enable a second claim for the same order.

## Atomicity, replay and failure

Commit/reveal and their indexes update atomically. Router consumes winner together with
destination claim or leaves auction unconsumed. Duplicate reveal/reset is explicit.

## Determinism and bounds

Quote count is bounded and winner selection is O(maxQuotes). Time equality and tie-break
are specified. Hash arithmetic and amount comparisons are checked.

## Compatibility, trust and activation

Commit codec, periods, quote cap, tie-break and Router address form one profile.
Configuration changes apply only to auctions opened after an activation epoch.

## Production-interface verification evidence

Inspected all Auction mutations, phase calculation and winner reads plus Router callers.
Unit/E2E tests cover typical competition, but exhaustive timestamp/configuration/replay
and permutation evidence is incomplete.

## Consequences

Winner selection becomes independently testable and bounded. Router cannot silently
replace a winner after claim.

## Rejected alternatives

- Unbounded quote arrays are rejected.
- First-reveal tie-breaking is rejected as ordering-dependent.
- Immediate owner changes affecting live auctions are rejected.

## Open questions and technical debt

- Define and test canonical tie-break for equal output quotes; current storage order may
  make the winner depend on reveal transaction ordering.
- Prevent owner period/cap/router changes from reinterpreting active auctions.
- Bind commitment to chain, contract, codec version and solver explicitly.
- Define reset/consumption atomicity with Router claim; current separate call can create
  winner reuse or lost auction state on partial logic.
- Add model/property tests for phase boundaries, max quotes, duplicate solver,
  permutation invariance and malicious encoded order data.

