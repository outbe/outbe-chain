# ADR-C-LYS-001: Lysis atomically transforms a sealed Tribute day into Nods

- **Status:** Proposed; OCOMP PoC target defined, current implementation is synchronous
- **Date:** 2026-07-26
- **Decision owners:** Tribute/Nod economics and authenticated-state maintainers
- **Scope:** `crates/core/lysis` and its direct Tribute, Nod, Fidelity, Oracle,
  Intex-contributor and compressed-entity seams
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-S-OCM-002 through
  ADR-S-OCM-004, ADR-C-GRT-001, ADR-C-MET-001, ADR-C-FID-001,
  ADR-C-TRB-001, ADR-C-NOD-001
- **Related:** ADR-S-OCM-001, ADR-B-OCD-001 through ADR-B-OCD-015,
  PFS-002
- **Supersedes:** Lysis sections of former broad pre-space Cycle/daily-orchestration document (previously numbered 029)

## Context

Lysis is a deterministic domain transformation executed through bounded work:
it consumes one sealed WorldwideDay's complete authenticated Tribute
population, allocates a supplied Gratis budget, creates the surviving Nod
representation and records Intex contributor provenance. Its all-or-nothing
conservation and authenticated-state obligations deserve a separate
architecture review boundary.

## Decision

Lysis V1 is a deterministic typed OCOMP program. Metadosis creates its exact
request; the authenticated input/export contract is ADR-S-OCM-002; independent
execution/evidence is ADR-S-OCM-003; and full-result quorum apply is
ADR-S-OCM-004.

The semantic program accepts a typed frozen job and authenticated input bundle
and, without chain writes:

1. requires the Tribute partition to be initialized and sealed;
2. reads all authenticated Tribute bodies in canonical paginated key order;
3. verifies body count and nominal sum against sealed `DayTotals`;
4. groups records by current Fidelity league;
5. computes deterministic fixed-point allocation fractions;
6. requires one strictly positive allocation per Tribute, never exceeding the
   remaining budget;
7. derives exactly one typed Nod action for each `(owner, day)` identity with
   canonical Oracle prices, floor, league and cost;
8. derives canonical contributor and Tribute-retirement effects;
9. streams bounded `ResultChunkV1` objects and builds their canonical catalog
   root, output roots, counts, totals and conservation commitments; and
10. returns constant-size `LysisResultV1` plus exact `unused_lysis`.

Metadosis splits the daily limit before Lysis:

```text
day_limit = lysis_budget + auction_base
unused_lysis = lysis_budget - used_by_nods
```

For a GREEN day, Metadosis sends `auction_base` to Desis before the OCOMP
job starts. A RED day sends no auction supply.

Desis is not a Lysis input or quorum-apply effect. `unused_lysis` is credited to
Promis carry-over only when the q-forming vote atomically applies the certified
result.

On-chain quorum apply does not rerun these steps or iterate every result action.
The closed Lysis verifier checks the typed result commitment, bindings, live
target preconditions, root transition and equations.

Only then may it create the private in-frame apply capability (currently named
`CertifiedLysisActivation` internally). Its apply path calls domain-owner
certified methods inside one outer checkpoint.

Metadosis owns terminal day status. Lysis owns transformation/result
correctness and its certified effect contract. Physical compressed-entity
persistence follows ADR-B-CNS-003 and the CE series; Lysis cannot claim storage
durability before end-block commit.

NodFactory owns later Nod mining, not Lysis. Mining requires Nod owner, qualified
state and valid PoW; any cost payment, Nod deletion and matching Gratis mint share
one transaction.

## Invariants

- The input partition is sealed and its authenticated bodies exactly match totals.
- Each consumed Tribute maps to exactly one unique Nod and no pre-existing Nod is
  overwritten.
- Sum of Nod Gratis loads plus returned remainder equals the supplied budget.
- `auction_base` never enters the Lysis input or result.
- `unused_lysis` is credited once to carry-over and never tops up a live auction.
- Every allocation is positive and no intermediate remaining budget underflows.
- Tribute supply/count/nominal are consumed exactly once.
- Contributor owner/nominal entries match the transformed Tribute set and their
  sum; ordering is canonical.
- `LysisResultV1` is bound to one exact `JobId`, attempt, request-time
  logical context and protocol bundle.
- Pure execution has no storage writer, system clock, network or configuration
  input.
- Only `CertifiedLysisActivation` can reach Lysis-owned effect methods.
- A failed transformation leaves no Nod, contributor, Tribute-total, retirement
  or carry-over effect.
- A successful transformation cannot be replayed for the same sealed population.

## Atomicity, determinism and capacity

Pure execution is partitioned into fork-pinned `UnitSpecV1` variants and a fixed
reduction order. Fixed-point constants, Fidelity league mapping, rounding, dust
destination, CE ordering, planner/reducer rules and price inputs are
consensus-critical. One, two and four workers plus arbitrary retry/completion
order must produce byte-identical result bytes. No floating point, wall time,
database enumeration order, network response or worker identity participates.

The PoC bounds one work shard, input/result chunks, concurrent workers and the
constant-size activation envelope; it does not cap total Tribute. Arbitrary `N`
is represented by committed counts and roots and processed with bounded
cursors. The PoC proves the architecture on a multi-shard fixture and proves
non-proportional plan construction for 10,000 and 1,000,000,000 records; it does
not claim billion-record throughput, storage capacity or operational readiness.

## Failure and recovery

Malformed/missing bodies, root/totals mismatch, duplicate Nod identity, zero
allocation, arithmetic overflow or unavailable required opening makes the local
execution invalid and produces no signature. Corrupt authenticated state is an
invariant failure, not a skippable Tribute.

During certified apply, a nested owner error or receipt mismatch aborts the outer
checkpoint. Retrying begins from the same live job. A diagnostic emitted inside a
reverted frame is not durable failure state; operational reporting cannot change
consensus state.

## Security, compatibility and evidence

Only OCOMP may execute/verify the pinned Lysis program, and only its private
certified capability may apply effects. The supplied budget/day cannot be
user-selected through a public bypass. Tribute bodies are trusted only after the
ADR-S-OCM-002 completeness contract; Oracle and Fidelity assumptions are imported
from their ADRs.

Body codec, identity `(owner, day)`, allocation math, fixed-point scales, grouping,
Nod schema and contributor encoding require fork activation for changes.

Inspected tests cover current synchronous arithmetic examples, count/total
mismatch, uniqueness, rollback, retirement preconditions and Nod mining guards.
No storage-independent executor, deterministic planner/reducer corpus, typed
result verifier, certified apply path or production OCOMP caller closure exists.

## Consequences

Lysis has one crisp semantic outcome: exact bounded result chunks and one
constant-size typed result commitment for the complete authenticated job,
followed by either fully conserved certified root application or no state
change. OCOMP can evolve operationally without owning its economics.

## Rejected alternatives

- **Skip malformed or tiny Tributes:** totals and one-to-one provenance would no
  longer close.
- **Commit Nods incrementally without an FSM:** retry could duplicate or omit value.
- **Delete bodies before all Nods exist:** recovery would lose authoritative input.
- **Let Cycle invoke Lysis directly:** it bypasses the WorldwideDay FSM and limit.
- **Keep synchronous on-chain Lysis as fallback:** compute outage would reintroduce
  unbounded consensus work and change failure semantics.
- **Apply generic calls/write sets:** domain invariants and private mutation
  authority would be bypassed.
- **Expose Lysis as a public generic `TaskAdapter`:** one program does not prove a
  safe common wire abstraction.

## Open questions and technical debt

1. The implementation still reads and transforms every Tribute synchronously in
   one block. Replace that production path at the PoC fork; no synchronous fallback
   may remain.
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
12. Prove `day_limit = lysis_budget + auction_base` and
    `lysis_budget = used_by_nods + unused_lysis` with checked arithmetic.
13. Add generated tests spanning all leagues, permutations, duplicate identities,
    rounding extremes, every injected nested-call failure and replay.
14. Add structural caller tests proving no user ABI or unrelated module can invoke
    raw Lysis/consume mutators.
15. Freeze `UnitSpecV1`, planner/reducer semantics, typed result/action codecs and
    an independent reference corpus.
16. Implement certified Nod, contributor, Tribute, carry-over and Metadosis
    methods/receipts. Desis must remain outside activation.
17. Prove 1/2/4-worker byte equality, Supervisor-submitted validator-ZeroFee
    result votes only after finality+4, public q=3 vote/quorum binding, separate
    fourth-validator accountability and the full PFS-002 activation/output
    path.
