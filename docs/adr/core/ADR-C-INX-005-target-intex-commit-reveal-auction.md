# ADR-C-INX-005: Target Intex auction owns a bounded commit/reveal FSM

- **Status:** Proposed; current Solidity implementation profiled
- **Date:** 2026-07-22
- **Owners/scope:** `contracts/intex/src/target/IntexAuction.sol`
- **Depends on:** ADR-C-DES-001, ADR-C-INX-003, ADR-C-INX-006
- **Related flow:** PFS-004

## Context

The target-chain auction receives authenticated stage commands, accepts bonded bid
commitments, reveals signed bids, exposes a relayable bid list and completes clearing.
It owns timing, commitment and bidder participation state independently of Desis.

## Decision

Each series follows `CommittingBids -> RevealingBids -> Issuance -> Completed`,
or lands as a `Cancelled` record when the day state delivered at `auctionStart` is
red. Stage timestamps, the final day state and immutable auction parameters are
installed once by the authorized router; the commit-to-reveal flip follows the
installed schedule on the local clock — no cross-chain reveal signal exists. One bidder has at most one live commitment and one accepted reveal. Commitment
binds series, bidder, quantity, rate, destination chain and salt using a canonical
domain-separated encoding. Bond lock/release/abandonment effects are consumed through
typed escrow receipts.

Clearing input is a bounded canonical list independent of transaction arrival where
economic ordering requires it. Permissionless reap/claim operations use cursors and
cannot move the stage or seize a non-expired bond.

## Authoritative interfaces

Router-only `auctionStart`, `startClearingStage` and `executeAuctionClearing`
own stages. Public `commitBid`, `cancelCommit`, `revealBid`,
`claimCommitBond` and `reapAuction` own bidder actions and cleanup.

## Invariants

- Series configuration is nonzero, internally valid, immutable and created once.
- Deadlines are monotonic and exactly one stage predicate is true at any timestamp.
- One commitment consumes one bond; accepted reveal consumes/releases it exactly once.
- Revealed bid matches commitment, signer/bidder, series and expected chain/domain.
- Bid/reveal count and relay bytes remain within the activated capacity profile.

## Atomicity, replay and failure

Commit plus bond lock, cancel/release, reveal validation/state/release and reap/abandon
are each one atomic transaction. Duplicate commands are explicit idempotent outcomes or
typed errors. Router stage replay cannot regress or repeat clearing.

## Determinism and bounds

Time comparisons define equality boundaries. Signatures are low-s/canonical and
domain-separated by contract, chain and version. Reaping is cursor-bounded. No stage
transition iterates the whole bidder set.

## Compatibility, trust and activation

Stage enum, commitment/signature encoding, deadlines, chain ids, role wiring and UUPS
layout are one profile coordinated with Desis and TargetRouter.

## Production-interface verification evidence

Inspected all stage, commit/cancel/reveal/reap/claim methods, signature verification and
escrow calls. Tests exercise principal paths, but the catalog lacks exhaustive boundary,
replay and failure-injection evidence across the real escrow/router stack.

## Consequences

Target participation remains locally enforceable while Desis owns economic clearing.
The two auction representations require an explicit PFS message reconciliation contract.

## Rejected alternatives

- Unbonded or plaintext-only bid admission is rejected.
- An unbounded “finalize all bidders” loop is rejected.
- Router ownership of bidder commitments is rejected.

## Open questions and technical debt

- Prove the exact commit hash and reveal signature bind all economic/domain fields and
  cannot replay across series, chains, deployments or versions.
- Define every timestamp equality boundary and behavior under target-chain timestamp skew.
- Bound total commitments/reveals and relay pages; current list storage can make clearing
  relay exceed destination gas/message limits.
- Add invariant tests connecting every live commitment to exactly one escrow bond.
- Test router stage duplicates/reordering and failures before/after every escrow call.
- Specify permissionless reap cursors and storage reclamation for large expired auctions.

