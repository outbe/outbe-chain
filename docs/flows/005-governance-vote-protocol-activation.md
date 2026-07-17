# PFS-005: Executable governance activates a protocol version

- **Status:** Draft
- **Actors:** active validator proposer, eligible validator voters, Vote, Update,
  upgrade handlers and node startup gate
- **Trigger:** an active validator submits an Update-targeted executable proposal
- **Topology/services:** validator network with identical Vote/Update handler
  registries; no off-chain service is authoritative
- **Referenced ADRs:** ADR-B-CNS-003, ADR-S-VAL-001, ADR-S-GOV-001, ADR-S-GOV-002, ADR-S-GOV-003
- **Supersedes:** The deleted pre-space governance aggregate narrative

## Outcome

A target-validated executable proposal reaches the active-validator yes quorum,
schedules exactly one supported version, executes its migrations once at the
declared height, and leaves every continuing node compatible with the active
on-chain version. An editorial OIP/GIP may link to the proposal, but never grants
execution authority.

## Preconditions and canonical inputs

- All validators run identical target and upgrade-handler registries.
- The proposer is active; each voter is pending or active and has not voted.
- The Update JSON payload has a canonical supported version, activation height and
  info satisfying the chain-specific buffer.
- No waiting update conflicts at the activation height and capacity remains.

## Success sequence

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | optional Governance | publish/link an editorial OIP/GIP | editorial id/hash only |
| 2 | Vote | validate target payload and create `Pending` proposal | proposal and pending index |
| 3 | validators/Vote | record at most one ballot per eligible address | ballot list and composite index |
| 4 | Vote begin-block | after deadline, filter against active set and reach 2/3 yes | terminal Vote receipt/event |
| 5 | Update target | atomically create the scheduled record | record and waiting index |
| 6 | Update begin-block | at height, run every version handler | migration state in checkpoint |
| 7 | Update | commit active version/history and cancel stale schedules | activation event and reads |
| 8 | node startup | refuse binaries older than active on-chain version | readiness/startup result |

## Boundaries and conservation

Proposal creation and every ballot are separate user transactions. Tally/target
dispatch and activation are ordered begin-block system effects. Approved Vote and
schedule creation share one checkpoint; migration and active-version publication
share another activation checkpoint.

```text
one executable proposal id -> at most one Update schedule
one voter + proposal id     -> at most one ballot
one activation height       -> at most one waiting update
Activated(version)          -> all waiting versions <= version are terminal
```

## Observable completion contract

The creation and ballot receipts succeed; after the deadline the Vote proposal is
`Approved`; the Update record is `Scheduled`; at/after activation it is `Activated`;
`activeVersion`, `activeVersionHeight` and `versionAtHeight` agree; all migration
postconditions hold; and a deliberately old binary refuses startup. Editorial
status is shown separately and is never accepted as execution evidence.

## Replay, retry, restart and partial failure

A duplicate ballot or duplicate schedule fails without mutation. Insufficient yes
quorum expires the proposal and creates no schedule. Target validation failure
prevents allocation; approved-target handling failure records `Rejected`. Migration
failure is block-fatal and rolls back version/state publication. Restart re-reads
waiting schedules and active version; terminal records prevent repeated migration.

## E2E scenario matrix

| Id | Scenario | Minimum topology | Required assertions | Automated by |
|---|---|---|---|---|
| PFS-005-01 | approve, schedule and activate version-only update | 4 validators | every state/height/event boundary | partial update feature |
| PFS-005-02 | migration handler succeeds once | 4 validators | migration plus active version atomic | GAP |
| PFS-005-03 | below-quorum expiry | 4 validators | no Update record | GAP |
| PFS-005-04 | duplicate ballot and duplicate proposal dispatch | 4 validators | no duplicate effects | GAP |
| PFS-005-05 | membership changes during voting | 4+ validators | documented snapshot semantics | GAP |
| PFS-005-06 | handler failure | 4 validators | fatal block/rollback and recovery | GAP |
| PFS-005-07 | old binary after activation | mixed binaries | incompatible node refuses startup | GAP |
| PFS-005-08 | restart with overdue schedules | 4 validators | deterministic order, exactly once | GAP |

## Open questions and technical debt

- Define and encode the binding between an optional OIP/GIP and executable proposal
  without giving Governance execution authority.
- Decide whether membership/quorum snapshots occur at creation or tally.
- Decide whether zero-buffer same-block scheduling/activation is a supported
  localnet contract or merely a test shortcut.
- Add stable PFS scenario tags to the update e2e feature and close the seven gaps.
- Define operator recovery when a scheduled handler is permanently fatal.
- Add a registry fingerprint/readiness check so mixed handler tables fail before
  consensus execution.
