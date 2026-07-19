# ADR-C-LYS-001: Lysis atomically transforms a sealed Tribute day into Nods

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Tribute/Nod economics and authenticated-state maintainers
- **Scope:** `crates/core/lysis` and its direct Tribute, Nod, Fidelity, Oracle,
  Intex-contributor and compressed-entity seams
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-C-GRT-001, ADR-C-MET-001, ADR-C-FID-001, ADR-C-TRB-001, ADR-C-NOD-001
- **Related:** ADR-B-OCD-001 through ADR-B-OCD-013
- **Supersedes:** Lysis sections of former broad pre-space Cycle/daily-orchestration document (previously numbered 029)

## Context

Lysis is a bounded domain transformation: it consumes one sealed WorldwideDay's
authenticated Tribute population, allocates a supplied Gratis budget, creates the
surviving Nod representation and records Intex contributor provenance. Its
all-or-nothing conservation and authenticated-state obligations deserve a separate
architecture review boundary.

## Decision

Lysis accepts a typed day and exact budget only from the Metadosis READY command.
It opens a checkpoint and:

1. requires the Tribute partition to be initialized and sealed;
2. reads all authenticated Tribute bodies in canonical paginated key order;
3. verifies body count and nominal sum against sealed `DayTotals`;
4. groups records by current Fidelity league;
5. computes deterministic fixed-point allocation fractions;
6. requires one strictly positive allocation per Tribute, never exceeding the
   remaining budget;
7. creates exactly one Nod for each `(owner, day)` identity with canonical Oracle
   prices, floor, league and cost;
8. records contributors in canonical address order for the Intex series;
9. decrements Tribute supply and zeros the verified DayTotals only after all Nods
   exist; and
10. returns the exact unallocated remainder to Metadosis.

Metadosis owns terminal day status and retirement request. Lysis owns transformation
correctness. Physical compressed-entity persistence follows ADR-B-CNS-003 and the CE
series; Lysis must not claim storage durability before end-block commit.

NodFactory owns later Nod mining, not Lysis. Mining requires Nod owner, qualified
state and valid PoW; any cost payment, Nod deletion and matching Gratis mint share
one transaction.

## Invariants

- The input partition is sealed and its authenticated bodies exactly match totals.
- Each consumed Tribute maps to exactly one unique Nod and no pre-existing Nod is
  overwritten.
- Sum of Nod Gratis loads plus returned remainder equals the supplied budget.
- Every allocation is positive and no intermediate remaining budget underflows.
- Tribute supply/count/nominal are consumed exactly once.
- Contributor owner/nominal entries match the transformed Tribute set and their
  sum; ordering is canonical.
- A failed transformation leaves no Nod, contributor, Tribute-total or retirement
  effect.
- A successful transformation cannot be replayed for the same sealed population.

## Atomicity, determinism and capacity

The entire day is currently one nested checkpoint. Fixed-point constants,
Fidelity league mapping, rounding, dust destination, CE ordering and price inputs
are consensus-critical. No floating point, wall time or database enumeration order
participates.

This all-at-once design is valid only if offer admission, gas, CE reads, memory,
Oracle calls and Nod writes impose a proven maximum Tribute count. Otherwise Lysis
requires an explicit `IN_PROGRESS` FSM with a persisted cursor and conservation
ledger; ad-hoc partial commits are forbidden.

## Failure and recovery

Malformed/missing bodies, totals mismatch, duplicate Nod identity, zero allocation,
arithmetic overflow, unavailable required price, or nested factory error aborts the
checkpoint. Retrying begins from the same sealed partition. Corrupt authenticated
state is an invariant failure, not a skippable Tribute.

A diagnostic emitted inside a reverted frame is not durable failure state.
Operational reporting must observe the propagated error without changing consensus
state.

## Security, compatibility and evidence

Only the intended Metadosis internal seam may invoke Lysis. The supplied budget and
day cannot be user-selected through a public bypass. Tribute bodies are trusted only
after compressed-entity proof/index checks; Oracle and Fidelity assumptions are
imported from their ADRs.

Body codec, identity `(owner, day)`, allocation math, fixed-point scales, grouping,
Nod schema and contributor encoding require fork activation for changes.

Inspected tests cover arithmetic examples, count/total mismatch, uniqueness,
rollback, retirement preconditions and Nod mining guards. A maximum-domain property
model and production ABI/caller closure remain missing.

## Consequences

Lysis has one crisp outcome: a fully conserved Tribute-to-Nod transformation or no
change. Cycle and Metadosis can be reviewed without inheriting its allocation
algorithm, while CE audits can point to one consumer boundary.

## Rejected alternatives

- **Skip malformed or tiny Tributes:** totals and one-to-one provenance would no
  longer close.
- **Commit Nods incrementally without an FSM:** retry could duplicate or omit value.
- **Delete bodies before all Nods exist:** recovery would lose authoritative input.
- **Let Cycle invoke Lysis directly:** it bypasses the WorldwideDay FSM and limit.

## Open questions and technical debt

1. The implementation reads and transforms every Tribute in one block. Establish a
   hard cardinality/work bound from admission and gas, or design a durable paginated
   `IN_PROGRESS` FSM.
2. Formalize maximum nominal, budget, league population and product bounds; replace
   unchecked fixed-point intermediates with checked arithmetic where required.
3. Small budgets/rounding may produce a zero allocation and block the whole day.
   Define minimum allocation, eligibility and dust policy.
4. Nod identity `(owner, day)` assumes at most one Tribute per owner/day. Prove and
   structurally test the invariant across all Tribute mutation/import paths.
5. `consume_lysis_partition` zeros totals while physical bodies disappear at CE
   persistence. Prove no public read exposes a logically consumed body and that all
   owner/day indexes retire atomically.
6. Define exact retry behavior when end-block CE persistence fails after execution
   produced a retirement intent.
7. The error path's FAILED event/state is reverted with the transaction. Add durable
   non-authoritative diagnostics and remove misleading writes.
8. Prove contributor aggregation when several Tributes resolve to the same owner;
   specify whether entries are per Tribute or per unique owner.
9. Price snapshots used for Nod cost/floor must be pinned to a precise block/day;
   current Oracle availability/finality assumptions need ADR-S-ORC-001 closure.
10. Nod mining accepts an asset whose binding to `reference_currency` is unfinished.
    Close this before cost-bearing mining is enabled.
11. ERC-20 transfer/approve return handling and nonstandard-token behavior require
    safe-call and adversarial-token tests.
12. A Desis clearing remainder must never exceed supplied Promis. Enforce the
    conservation guard at the owning boundary before Metadosis commits it.
13. Add generated tests spanning all leagues, permutations, duplicate identities,
    rounding extremes, every injected nested-call failure and replay.
14. Add structural caller tests proving no user ABI or unrelated module can invoke
    raw Lysis/consume mutators.
