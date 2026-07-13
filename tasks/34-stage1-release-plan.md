# T34 — Stage 1 testnet release/soak plan (early, no code dependencies)

Status: todo
Source: `audit_plan_v2.md` §5 P1-9, P1-8 of audit_plan.md; T25 prerequisite
Depends on: T29 (Variant A scenarios shape the failure schedule; draft may start after T29), T30
(approval/completion waits for T30 outcome codes)
Blocks: T24 (the approved hardware profile + benchmark protocol precede any candidate run — audit-final
B-06/H-06), T25 (the acceptance/soak run executes this plan)

## Summary

Author the written Stage 1 release/soak plan early, decoupled from implementation, so T25's execution has
an approved plan instead of inventing one at the end.

## Contents

### Part A — hardware profile + benchmark/release protocol (EARLY; blocks T24)

- VERSIONED minimum validator hardware profile (audit-final B-06): CPU model/cores, RAM, storage
  class/IOPS, network, OS, toolchain — an approved artifact with a profile ID; T24 verifies the benchmark
  host against it FAIL-CLOSED; T25 binds benchmark and soak evidence to the same profile ID.
- VERSIONED benchmark protocol (audit-final H-06): numeric target scale, concurrency, cold-cache
  procedure, repetition count, the gating statistic, outlier policy, and minimum safety margin — fixed
  BEFORE any candidate run; T24 fails closed on protocol-version mismatch. The `K_domain` selection is
  protocolized too (postfix PF-M06): a versioned candidate set, the measured metrics, an explicit
  objective function, and a deterministic tie-break — the selection is reproducible from the report.
- Manual paired-restore contract (audit-final H-03): this plan OWNS the runbook — paired Reth+CE+body
  checkpoint creation (aligned two-store export identity per T22), verification steps, and the restore
  sequence; T25 REHEARSES restore → verify → catch-up → READY → vote as executable evidence.
- Rollout/abort/rollback contract (audit-final M-04): quorum preflight checks, rollout order, abort
  thresholds, no-downgrade / reset-or-restore policy, and the recovery procedure after a quorum-level
  Mongo halt; T25 rehearses the abort path.

### Part B — soak/release plan

- Soak duration and block/body/key load profile (full-block CE load shape).
- Restart/failure schedule: validator restarts, finalized catch-up, snapshot bootstrap, cross-source
  resume, datadir relocation, ExEx replay, local body loss.
- Variant A scenarios (owner-accepted risks made explicit, minimal model): single-validator Mongo
  outage/data loss → the node's body reads fail, it diverges and falls out of certification, network
  continues on quorum, operator resyncs/restores per runbook, node rejoins; quorum-level Mongo outage →
  testnet halt as an accepted, exercised scenario; fresh-validator bootstrap via `tree-with-bodies` +
  Reth-first sequence (T22/T28).
- Lag/error/proof-serving SLOs; pass/fail thresholds; observed Mongo point-read and Lysis read+verify
  latencies recorded as report metrics (informational — no readiness machinery consumes them).
- Hardware and network topology for soak nodes (MongoDB in Docker, single-node replica sets).
- Evidence locations and sign-off owner.
- Stage discipline restated: everything proven under Variant A is TESTNET evidence; the production gate
  needs the future off-chain computation design.

## Acceptance criteria

1. Plan document merged and approved before T25's suite assembly begins; Part A (hardware profile +
   benchmark protocol + restore/rollout contracts) is approved BEFORE T24 candidate runs begin
   (audit-final B-06/H-06).
2. Every §19.18 soak clause and every Variant A owner-accepted risk maps to a named scenario with a
   pass/fail threshold.
3. T25 references this plan as its execution input (no inline ad-hoc soak definition).

## Files

- `docs/ces-stage1-testnet-release-plan.md`
