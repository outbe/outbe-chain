# ADR-C-DES-001: Desis owns the cross-chain auction, bid relay and clearing FSM

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-22
- **Owners/scope:** `crates/core/desis`, its OriginRouter ABI, the auction
  schedule, per-chain bid intake and uniform-rate clearing algorithm
- **Depends on:** ADR-C-MET-001, ADR-S-CYC-001, ADR-C-PRM-003, ADR-C-INX-001,
  ADR-C-INX-002
- **Related flow:** PFS-009, PFS-004

## Context

Desis bridges a Metadosis-derived Intex supply to a demand auction on the
day's target chains. Metadosis hands it a one-shot brief; from that point Desis
owns the schedule: it starts the auction, sends stage messages through
OriginRouter to every target of the day's snapshot, accepts unordered per-chain
bid batches back, clears deterministically once every chain reported (or a
deadline passed), issues Intex to winners and returns unsold Promis capacity.
This is one state owner with an asynchronous cross-chain FSM.

## Decision

Desis is the sole owner of per-day auction configuration, stage, the schedule,
per-(day, chain) relay generation/completeness, pending supply and the
latest-clearing inputs used for the next minimum quantity. Metadosis
participates exactly once per day: `dispatch_auction_brief` records supply,
entry price and day type, then never signals again. Two drivers advance the
day from there:

- the `auction_advance` Cycle trigger (12h) walks the schedule: start at the
  anchor, flip to Revealing at commit end, arm the clearing gate at reveal end,
  retire overdue days;
- the `DesisLifecycle` begin-block hook fires the clearing fan-in gate.

Only the fixed OriginRouter address may submit bid batches, completeness
markers or clearing calls through the public ABI.

Every dispatch helper wraps its work in a storage checkpoint: a failed
best-effort command rolls back all of its writes before reporting `false`, so
no partial state survives a swallowed error.

## State machine

```text
None --Metadosis brief----------------------------------> Briefed
Briefed --schedule tick at anchor, green brief----------> Started
Briefed --schedule tick at anchor, red brief (green=0)--> Cancelled (terminal)
Started --schedule tick at commit end-------------------> Revealing
Revealing --schedule tick at reveal end (gate armed)----> Clearing
Clearing --gate: all chains done or deadline------------> Cleared (terminal)
any non-terminal --schedule tick past issuance end------> Cancelled (terminal)
```

The brief anchors to the current UTC midnight while it still leaves the
minimum commit window, else to the next one. Stage messages carry the day
state: a red day still sends STAGE_START (dayState RED) so the targets learn
the outcome, then cancels locally before start. There is no separate reveal
message — targets flip to reveal on their own clocks from the windows carried
in STAGE_START; Desis sends STAGE_START and STAGE_CLEARING only.

## Per-chain intake and the fan-in gate

The day's target set is frozen at STAGE_START (`targetsOf` snapshot read from
the OriginRouter registry). Bid state is keyed by `(day, chain)`: relay
generation, total batches, a 256-bit arrival bitmap, bid count and a done flag.
A chain finalizes once its BIDS_DONE marker and every batch arrived with
matching totals (`ChainBidsDone`); an integrity mismatch keeps it not-done. A
higher generation supersedes that chain's accumulated bids and resets its done
flag; a redelivered batch is an idempotent no-op. Intake is open in
Revealing/Clearing, terminal stages no-op so the transport stops retrying, and
earlier stages revert so premature messages are redelivered.

The gate clears when every snapshot chain is done, or once the fan-in deadline
(12h from arming) passes — then the silent chains are excluded and reported via
`ChainSkipped`, and their bidders rely on the escrow's never-finalized refund
path on their own chain. Each gate-active day runs inside its own checkpoint in
the begin-block hook; an error is retried next block and never escapes into the
hook chain.

## Clearing algorithm and effects

Bids sort by descending rate and ascending timestamp. Eligible bids meet the
minimum rate and quantity; allocation proceeds until the whole-Intex supply is
exhausted; all winners pay the last allocated bid's uniform clearing rate;
losers refund in full. Zero bids or zero supply clear empty
(`AuctionClearedEmpty`) and return the whole supply.

`Cleared` commits, in one transaction: terminal stage, the last-clearing count,
unused-supply return to PromisLimit (`UnusedSupplyReported`), the issuance
hand-off to IntexFactory (winners grouped per chain, the full snapshot for
series provisioning), and the per-chain sends — AUCTION_RESULT to every
snapshot chain (skipped/zero-winner chains get a zero count so their local
auction completes) and REFUND_INSTRUCTIONS to every chain with bidders.

## Determinism and bounds

The 256-batch bitmap bounds batches per chain; clearing loads and stable-sorts
every accumulated bid. Equal rate and timestamp preserve storage order, which
depends on batch arrival order across chains — a canonical tie-breaker is
still an open item. Schedule windows (24h commit / 24h reveal / 24h
settlement), the 12h fan-in deadline, the OriginRouter address, fixed-point
scales, Promis load and floor/call formulas are protocol compatibility
constants.

## Compatibility, trust and evidence

The router is an authenticated transport endpoint, not an economic truth
oracle: `msg.sender` gating plus the body `srcChainId` cross-check performed by
the router are the admission boundary. ABI structs, stage-message layout,
generation semantics, sort/tie-break, rounding and result/refund arrays require
coordinated activation across chains.

Tests cover the schedule walk, per-chain intake/supersede, gate completion and
deadline skip, zero bids/supply, uniform pricing and checkpointed dispatch
failures (unit suite), plus the cross-module day walk with the production hook
chain (`crates/core/e2e/tests/wwd_auction_clearing.rs`).

## Consequences and rejected alternatives

Metadosis stays a one-shot supplier and never owns remote bid state; targets
never learn clearing logic. A fixed router caller gate keeps the public
mutation surface minimal. Keeping a cross-chain reveal message was rejected:
the windows ride in STAGE_START and each chain flips locally. Clearing from an
OriginRouter auto-fire was rejected in favour of the begin-block gate — the
gate is where completeness and the deadline live.

## Open questions and technical debt

- Add a canonical bid id tie-breaker; equal rate and timestamp currently
  preserve arrival order.
- Bound total bids per day, encoded result/refund sizes and child-call gas;
  the 256 bound applies per chain to batch count only.
- Validate bidder nonzero, positive quantity, rate range and timestamp window
  at intake; today the router gate is the principal admission check.
- Result/refund sends rely on the router's park-and-flush queue for transport
  failures; an end-to-end acknowledgement model remains future work.
