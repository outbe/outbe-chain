# PFS-002: WorldwideDay transforms sealed Tributes into Nods

- **Status:** Draft
- **Actors:** Cycle scheduler, Metadosis, Lysis, Tribute/CE, NodFactory, Oracle,
  Fidelity, Desis/Intex contributor sink
- **Trigger:** Due midnight/noon Cycle command advances a WorldwideDay to READY
- **Topology/services:** Finalizing validator network with CE persistence and valid
  Oracle inputs
- **Referenced ADRs:** ADR-B-CNS-003, ADR-S-CYC-001, ADR-S-ORC-001, ADR-C-MET-001, ADR-C-LYS-001
- **Supersedes:** None

## Outcome

A sealed READY WorldwideDay consumes its exact Tribute population and economic
budget once, creates one conserved Nod result per Tribute, commits contributor
provenance and reaches a terminal day state with authenticated Tribute retirement.

## Acceptance contract

- **Source:** Cycle scheduler processing a due canonical slot.
- **Trigger:** Cycle executes the due canonical slot for a sealed READY WorldwideDay.
- **Environment:** Finalizing network at the due slot with parent accounting complete, CE persistence available and canonical Oracle/Fidelity observations.
- **Canonical inputs:** READY Metadosis record, sealed Tribute partition and totals, canonical Cycle slot/parent accounting, Lysis budget/type/limit, Oracle and Fidelity values, and the ordered Tribute population.
- **System under test:** Cycle, Metadosis, Lysis, Tribute/CE, NodFactory, contributor sink and end-block CE persistence.
- **Expected response:** Terminal Metadosis/Cycle records, one Nod per Tribute, contributor records, remainder disposition, authenticated Nod proofs and Tribute-retirement evidence.
- **Response measures:** The day and slot execute once; Nod count equals consumed Tribute count; Nod loads plus remainder equal the budget; nominal totals agree; every Nod proof verifies and the Tribute partition is retired.
- **Failure guarantee:** Failure keeps day, cursor, Tributes, contributors and Nods unchanged and leaves the same READY slot retryable.

## Preconditions and canonical inputs

- Cycle has a due canonical slot and parent-accounting prerequisite is satisfied.
- Metadosis record exists, windows/type/limit are fixed, status is READY, and the
  Tribute partition is sealed.
- Authenticated bodies exactly match sealed count/nominal totals.
- Oracle/Fidelity values are available at the specified canonical context.
- Nod identities do not already exist and total work fits the block/CE budget.

## Success sequence

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | Cycle | invoke due handler without advancing cursor | system-tx trace/checkpoint |
| 2 | Metadosis | select canonical READY day and branch on limit/type/count | typed record |
| 3 | Lysis | verify sealed CE population/totals and calculate allocations | no durable partial state |
| 4 | NodFactory | create one Nod per Tribute | Nod records/events/CE intents |
| 5 | Lysis | record contributors; consume Tribute totals/supply | closed equations |
| 6 | Metadosis | commit returned remainder, terminal status and retirement intent | terminal record |
| 7 | Cycle | commit scheduled cursor/event | trigger record |
| 8 | end-block/CE | persist Nod bodies and Tribute collection retirement | roots/proofs/checkpoint |

## Boundaries and conservation

Steps 1–7 share the Cycle trigger checkpoint; Lysis additionally isolates its
transform. Any propagated failure restores the prior READY day and unchanged Cycle
cursor. CE disk persistence is a later block-lifecycle boundary that must agree with
the committed root or fail the block.

```text
sum(Nod gratis_load) + returned_remainder = supplied Lysis budget
created Nod count = consumed Tribute count
consumed nominal = sealed Tribute nominal
```

## Observable completion contract

The day is COMPLETED (or its explicitly specified non-Lysis terminal branch), no
longer active, and retained in terminal history. Each expected Nod is readable and
proof-verifiable. The Tribute collection is authentically absent/retired, not merely
missing from Mongo. Contributor totals match the transformed population. Promis or
auction remainder equals the branch equation. Cycle cursor names the executed slot.

## Replay, retry, restart and failure

Failure before terminal commit retries the same READY record and Cycle slot with no
Nods. A successful terminal day cannot be selected again. Restart between EVM commit
and CE persistence follows block atomicity/recovery ADRs; restart after finality
reconstructs projections without rerunning Lysis.

## E2E scenario matrix

| Id | Scenario | Minimum topology | Required assertions | Automated by |
|---|---|---|---|---|
| PFS-002-01 | GREEN day with several Tributes | 4 validators, CE, Oracle | all conservation and proof assertions | in-process `test_runtime_e2e_green_then_red_wwd_lysis_nod_mine_gratis` (state/conservation; live finality/proofs GAP) |
| PFS-002-02 | no-Tribute terminal branch | same | no Nod; exact remainder; retired empty partition | documentation-only until the in-process fixture supports an empty sealed CE partition |
| PFS-002-03 | zero limit | same | specified FAILED/auction outcome; no transform | GAP |
| PFS-002-04 | totals/body mismatch | same | full rollback; cursor/day remain retryable | documentation-only: production mismatch requires a corrupt parent-body adapter/fault injection seam |
| PFS-002-05 | duplicate owner/day identity | same | admission prevents it or Lysis atomically rejects | GAP |
| PFS-002-06 | injected Nod creation failure | same | no partial Nods/contributors/consumption | module integration `later_nod_failure_rolls_back_the_complete_lysis_attempt`; full lifecycle fixture GAP |
| PFS-002-07 | restart at CE persistence boundary | same | deterministic recovery and proofs | documentation-only until harness exposes a deterministic end-block persistence failpoint |
| PFS-002-08 | long timestamp jump/backlog | same | canonical ordered processing per accepted policy | GAP |

## Open questions and technical debt

- This flow has in-process cross-module automation but lacks live-node finality,
  persistence, projection and proof coverage.
- READY selection/backlog order and Cycle long-gap policy are unresolved.
- One-block Lysis capacity is not proven; a paginated FSM may be required.
- Define durable historical queries after terminal FIFO eviction and Tribute
  retirement.
- Make Desis/Promis remainder conservation an enforced assertion, not a TODO.
- Add independent proof verification for all created Nods and retired collection.
