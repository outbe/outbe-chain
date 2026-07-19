# ADR-C-FID-001: Fidelity owns acquisition cohorts and retention leagues

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Protocol economics maintainers
- **Scope:** `crates/core/fidelity`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004
- **Related:** ADR-C-GRT-001, ADR-C-GRT-002, ADR-C-PRM-001, ADR-C-PRM-002
- **Supersedes:** Fidelity portions of former broad pre-space Gratis/economic aggregate (previously numbered 030)

## Context

Fidelity measures retained protocol value. It is not a token balance and does not
decide minting or conversion. It records when value was acquired and disposed,
then derives RCFI and a league at a canonical timestamp. Its historical ledger is
consumed by eligibility and allocation modules, so silent cohort damage changes
protocol economics even when token supply remains correct.

## Decision

Fidelity owns, per account, an active LIFO cohort stack and an append-only sold
cohort log. It also stores the account's first qualification time and the earliest
qualification time across all accounts.

`cohort_in(account, amount, block_time)` appends an active cohort. The first nonzero
acquisition initializes both applicable qualification anchors.

`cohort_out(account, amount, block_time)` consumes the youngest active cohorts
first. Full consumption moves a cohort to the sold log. Partial consumption records
the sold slice with its original acquisition time and shrinks the active tail.

RCFI is calculated on demand from persisted history:

- active quantity contributes decayed age to numerator and denominator;
- sold quantity contributes its decayed holding interval to the denominator;
- efficiency is numerator/denominator;
- RCFI is qualification age multiplied by efficiency; and
- league partitions `[0, max_rcfi_at(now)]` into 4096 one-based slots.

The global earliest qualification supplies the synthetic maximum. Public ABI is
read-only. Mutation is an internal protocol hook whose callers must be enumerated.

## Persistent state and invariants

- Active slots are dense in `[0, active_count)` and every slot has positive size.
- Sold slots are dense in `[0, sold_count)`, positive and append-only.
- `acquired_at <= sold_at <= current block time` for newly recorded sales.
- `qualified_start` is the first acquisition time and never moves forward.
- `first_qualified_start` is the minimum nonzero qualification time globally.
- A `cohort_out` consumes exactly the requested quantity or fails atomically.
- Active quantity reconciles with the tracked economic holdings after accounting
  for explicitly age-preserving Promis-to-Gratis conversion.
- RCFI lies between zero and the synthetic maximum; league lies in `1..=4096`.

Missing dense slots, impossible timestamps or insufficient active quantity are
invariant failures, not reasons to clamp a calculation.

## Authority, atomicity and replay

Gratisfactory and PromisFactory are the intended acquisition/disposal workflow
owners. Direct internal calls are privileged. A structural test must enumerate all
callers and the matching economic mutation.

Cohort mutation and the corresponding Gratis/Promis mint, burn or conversion must
share one EVM rollback domain. Fidelity has no independent replay key; the owning
workflow supplies it. Callers may not supply wall time—only canonical execution
time is accepted at the boundary.

## Determinism, bounds and compatibility

Decay function, scale, cohort ordering, domain-separated slot keys, LIFO policy,
league count and timestamp interpretation are consensus economics. Changes require
activation, migration/reference vectors and proof that historical results remain
interpretable.

RCFI reads are linear in an account's complete active and sold history. Protocol
bounds or aggregation are required before history growth makes ABI reads or
consuming transactions unbounded.

## Production-interface verification evidence

Inspected storage schema, mutation API, LIFO/split runtime, fixed-point computation,
read-only ABI and Python/reference tests. Existing tests compare numerical examples,
but do not prove full economic reconciliation, corrupted dense indexes, overflow
bounds or all cross-module rollbacks. Status remains Proposed.

## Consequences

Fidelity remains a history/metric module with no issuance authority. Consumers
import a typed league or index rather than reimplementing retention calculations.

## Rejected alternatives

- **Use current balance only:** acquisition age and sold-history penalty disappear.
- **FIFO disposal:** it changes the intended retention economics.
- **Reset age on Promis-to-Gratis conversion:** conversion would game loyalty.
- **Silently skip missing cohorts:** corrupt state would yield plausible rankings.

## Open questions and technical debt

1. `cohort_out` currently stops on a missing tail and may leave an unconsumed
   remainder. Replace this clamp with a structural error.
2. Prove and enforce that requested disposal never exceeds active quantity.
3. Fixed-point products and sums use unchecked `U256` arithmetic under “realistic
   supply” assumptions. Establish protocol bounds or use checked operations.
4. Timestamp subtraction uses saturation. Impossible future acquisition/sale times
   must fail instead of appearing age zero.
5. Add a structural caller/effect test pairing every cohort mutation with its exact
   Gratis or Promis economic mutation.
6. Define and test active-quantity reconciliation when value converts between
   Promis and Gratis without cohort mutation.
7. Sold history grows forever and reads are linear. Specify maximum cohorts,
   safe aggregation/compaction, pagination and gas limits.
8. `first_qualified_start` assumes monotonic timestamps and first write is global
   minimum. Prove behavior under genesis imports/migrations and timestamp rules.
9. Add generated model tests for arbitrary deposits, full/partial LIFO sales,
   conversions, identical timestamps and rollback failures.
10. Define whether zero account is valid for reads and prohibit it for mutations.
11. Pin Python/reference implementation version and golden vectors in CI.
12. Human economics review is required for decay, LIFO and 4096-slot league policy.
