# ADR-C-INX-006: Intex bid escrow conserves locked value through finalization and recovery

- **Status:** Proposed; current Solidity implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `contracts/intex/src/target/EscrowAdapter.sol`, The Compact seam
- **Depends on:** ADR-C-INX-005, ADR-C-VLT-001, ADR-B-CAP-001
- **Related flow:** PFS-004

## Context

EscrowAdapter pools bid funds and commit bonds in The Compact, tracks per-series/bidder
locks, executes batched finalization, exposes refund claims and burns winning portions
that can no longer be distributed (the series was already routed on Outbe). It is the
monetary authority for the target auction.

## Decision

Primary state distinguishes bid lock, commit bond, auction aggregate and finalization
identity. For each bidder, locked value terminates exactly once as paid proceeds,
claimable refund, burned remainder (canonical dead address) or released/abandoned bond.
Finalization is identified by authenticated `receiveId` plus series and immutable
instruction commitment. Per-item failure is parked with enough data for deterministic
retry; success cannot replay.

The Compact balance is pooled physically but logically equals the sum of all live bid
locks and bonds. Burns are terminal within their transaction; no external-debt state
survives them.

## Authoritative interfaces

Auction-only `lockFunds`, `lockCommitBond`, `releaseCommitBond`; router/bridger-only
`finalizeAuction` and `retryFinalize`; public `claimRefund` and abandoned bond claim
are the closed commands. Allocator callbacks authorize only the corresponding planned
Compact claim.

## Invariants

- Pooled Compact balance equals sum of live bid locks plus live bonds, adjusted only by
  explicitly withdrawn terminal effects.
- `refunded + paid == locked` for every successful finalization instruction.
- Auction total locked equals the sum of its nonterminal bidder locks.
- A finalization item and receive identity can succeed at most once.
- Proceeds recipient, payment token, Compact and allocator are valid, versioned wiring
  and cannot change while outstanding locks exist.

## Atomicity, replay and failure

Lock state and Compact deposit, terminal state and withdrawal/distribution/burn, and
claim and transfer are atomic pairs. Batched finalization isolates an item only through
a self-call trampoline and durably records its failure. State is marked before external
calls under reentrancy guard and rolls back on failure unless an explicit failed-split
record is committed.

## Determinism and bounds

Instruction count, amounts, failure bytes and retry storage are bounded. Arithmetic is
checked at ERC-20 decimal scale. Token behavior is restricted or measured. Permissionless
settlement cannot force unbounded work.

## Compatibility, trust and activation

Payment token, Compact lock id/tag, allocator, reset period, auction/router roles and
UUPS layout form one deployment manifest. Rewiring is forbidden with outstanding locks
and uses governed two-step activation.

## Production-interface verification evidence

Inspected wiring, allocator callbacks, lock/bond/finalization/retry/refund/burn paths and
external Compact/token calls. A Foundry stateful invariant already checks pooled
balance equals live auction locks plus bonds across randomized actions; it is valuable
evidence but does not yet cover every external failure and upgrade/reconfiguration path.

## Consequences

Auction clearing can survive individual downstream failures without losing the monetary
ledger. The failed-split records must remain observable and operable.

## Rejected alternatives

- Best-effort transfers without durable debt are rejected.
- Rewiring active escrow is rejected.
- Treating pooled Compact balance as belonging to one series is rejected.

## Open questions and technical debt

- **Critical:** extend conservation evidence across real Compact failures,
  fee-on-transfer/reentrant tokens, partial finalization and retries.
- Prove per-item self-call isolation and receive-id replay cannot finalize twice or leave
  aggregate totals inconsistent.
- Define terminal accounting when a refund remains unclaimed indefinitely.
- Enforce `hasOutstandingLocks` on every wiring/upgrade path, including bonds.
- Bound instruction batches and stored revert reasons; add gas/DoS tests at maxima.
- Audit permissionless `claimRefund` and relayer `retryFinalize` for unauthorized state
  selection and griefing despite value destinations being fixed.

