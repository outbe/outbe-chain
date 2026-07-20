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

| Step | Owner        | Command/effect                                                   | Durable evidence              |
| ---: | ------------ | ---------------------------------------------------------------- | ----------------------------- |
|    1 | Cycle        | invoke due handler without advancing cursor                      | system-tx trace/checkpoint    |
|    2 | Metadosis    | select canonical READY day and branch on limit/type/count        | typed record                  |
|    3 | Lysis        | verify sealed CE population/totals and calculate allocations     | no durable partial state      |
|    4 | NodFactory   | create one Nod per Tribute                                       | Nod records/events/CE intents |
|    5 | Lysis        | record contributors; consume Tribute totals/supply               | closed equations              |
|    6 | Metadosis    | commit returned remainder, terminal status and retirement intent | terminal record               |
|    7 | Cycle        | commit scheduled cursor/event                                    | trigger record                |
|    8 | end-block/CE | persist Nod bodies and Tribute collection retirement             | roots/proofs/checkpoint       |

## Boundaries and conservation

Steps 1–7 share the Cycle trigger checkpoint. Lysis isolates its transform in a
separate checkpoint. Any propagated failure restores the prior READY day and unchanged Cycle
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

| Id         | Scenario                     | Given / canonical inputs                                               | When / trigger                 | Then / outputs and postconditions                                           | Verification                                                                           |
| ---------- | ---------------------------- | ---------------------------------------------------------------------- | ------------------------------ | --------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| PFS-002-01 | populated day transformation | READY sealed day, canonical Tributes/budget/Oracle                     | Cycle executes due slot        | day completes; one Nod per Tribute; budget/totals conserve                  | in-process `test_runtime_e2e_green_then_red_wwd_lysis_nod_mine_gratis`; live proof gap |
| PFS-002-02 | empty Tribute branch         | READY day with sealed empty partition and full remainder               | Cycle executes due slot        | no Nod; exact remainder; terminal day and defined retirement result         | documentation-only: fixture cannot form empty sealed CE partition                      |
| PFS-002-03 | zero limit                   | READY day with zero Lysis limit                                        | Cycle executes due slot        | normative FAILED/auction branch; no Tribute/Nod mutation                    | documentation-only pending branch decision                                             |
| PFS-002-04 | totals/body mismatch         | READY record totals disagree with authenticated bodies                 | Cycle executes due slot        | complete rollback; cursor/day/Tributes unchanged and retryable              | documentation-only: corrupt parent-body injection seam absent                          |
| PFS-002-05 | duplicate owner/day identity | duplicate is attempted before sealing or appears in corrupt population | admit offer or execute Lysis   | admission rejects duplicate; corrupt sealed population must atomically fail | `@pfs-001-05` covers admission only; Lysis fault injection absent                      |
| PFS-002-06 | Nod creation failure         | valid READY population with injected later Nod failure                 | Lysis transforms population    | no partial Nods/contributors/consumption; day stays READY                   | module integration `later_nod_failure_rolls_back_the_complete_lysis_attempt`           |
| PFS-002-07 | CE persistence restart       | EVM outcome prepared but CE end-block persistence interrupted          | restart/reconcile node         | either semantic pre-state or full roots/proofs; never partial population    | documentation-only: persistence failpoint absent                                       |
| PFS-002-08 | long timestamp backlog       | multiple overdue canonical Cycle slots                                 | process a large timestamp jump | slots execute in defined order once; no skip/duplicate                      | documentation-only pending backlog policy                                              |

## Open questions and technical debt

- This flow has in-process cross-module automation but lacks live-node finality,
  persistence, projection and proof coverage.
- READY selection/backlog order and Cycle long-gap policy are unresolved.
- One-block Lysis capacity is not proven; a paginated FSM may be required.
- Define durable historical queries after terminal FIFO eviction and Tribute
  retirement.
- Make Desis/Promis remainder conservation an enforced assertion, not a TODO.
- Add independent proof verification for all created Nods and retired collection.
