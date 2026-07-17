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

## Acceptance contract

- **Source:** Active validator proposer and eligible validator voters.
- **Trigger:** An active validator submits an executable proposal targeting the registered Update handler.
- **Environment:** Finalizing validator network with identical target/handler registries and no conflicting schedule at the activation height.
- **Canonical inputs:** Canonical version/height/info payload, proposer/voter identities, active-set/quorum context, deadline, handler registry and current active/waiting versions.
- **System under test:** Vote, Update, registered migration handlers, begin-block scheduling and node startup compatibility gate.
- **Expected response:** Proposal and ballots, terminal Vote result, at most one scheduled Update, migration effects, activation/history events and node readiness result.
- **Response measures:** One ballot per voter and one schedule per proposal; activation executes and publishes atomically once at the declared height; all version reads agree and incompatible nodes refuse startup.
- **Failure guarantee:** Expiry, duplicate, invalid target or handler failure creates no partial schedule, migration or active-version publication; restart cannot execute a terminal activation twice.

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

| Id | Scenario | Given / canonical inputs | When / trigger | Then / outputs and postconditions | Verification |
|---|---|---|---|---|---|
| PFS-005-01 | approve/schedule/activate | 4 active validators and supported next version/height | propose, cast 3 yes, cross deadline/height | Approved→Scheduled→Activated; version reads and committee roots agree | `@pfs-005-01` live + in-process full flow |
| PFS-005-02 | migration succeeds once | scheduled version with stateful registered handler | activation height executes | migration and active-version publication commit atomically once | version handler covered; stateful migration fixture absent |
| PFS-005-03 | below-quorum expiry | four validators with only two yes votes | tally after deadline | Expired; no schedule/event/version mutation | in-process `full_vote_update_flow_2_of_4_yes_expires_without_update_state_change` |
| PFS-005-04 | duplicate ballot/dispatch | pending proposal and voter already recorded | repeat ballot or target dispatch | rejection/idempotency; one ballot and at most one schedule | in-process duplicate ballot; duplicate dispatch absent |
| PFS-005-05 | membership changes during vote | pending proposal and changing active set | join/exit crosses tally deadline | quorum follows the normative snapshot rule; no node divergence | documentation-only pending snapshot decision |
| PFS-005-06 | migration handler failure | scheduled update with deliberately failing registered handler | activation height executes | fatal rollback; schedule/version/migration pre-state retained; recovery defined | documentation-only: failing handler/recovery absent |
| PFS-005-07 | old binary startup | chain active version newer than binary | node starts/restarts | readiness/startup refuses before participation | in-process compatibility check; mixed-node live gap |
| PFS-005-08 | restart with overdue schedules | durable waiting updates now overdue | restart node | deterministic ordered execution exactly once | documentation-only pending order policy/restart fixture |
| PFS-005-09 | unsupported version activation | approved schedule above binary ceiling | committee reaches activation height | block is fatal/stalls; version unchanged and schedule waiting | `@pfs-005-09` live-node |
| PFS-005-10 | downgrade proposal | active version newer than payload | approve/tally downgrade | Rejected; no schedule or active-version change | in-process downgrade flow |
| PFS-005-11 | conflicting activation height | one waiting update at height | approve another version for same height | second Rejected; first schedule unchanged | in-process conflicting update flow |
| PFS-005-12 | stale activation | newer version already active with older schedule retained | execute older activation | no downgrade; deterministic terminal result | in-process stale activation flow |

## Open questions and technical debt

- Define and encode the binding between an optional OIP/GIP and executable proposal
  without giving Governance execution authority.
- Decide whether membership/quorum snapshots occur at creation or tally.
- Decide whether zero-buffer same-block scheduling/activation is a supported
  localnet contract or merely a test shortcut.
- Keep the oversized-pagination RPC regression intentionally outside PFS; close the remaining migration, membership and restart gaps.
- Define operator recovery when a scheduled handler is permanently fatal.
- Add a registry fingerprint/readiness check so mixed handler tables fail before
  consensus execution.
