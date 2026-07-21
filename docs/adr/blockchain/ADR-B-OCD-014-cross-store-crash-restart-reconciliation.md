# ADR-B-OCD-014: Cross-store crash and restart reconciliation

- **Status:** Proposed; current implementation partially satisfies the decision
- **Date:** 2026-07-17
- **Decision owners:** Blockchain Space, node, consensus, execution and persistence maintainers
- **Scope:** restart reconciliation across Reth, consensus archives, CE MDBX and Mongo projection
- **Depends on:** ADR-B-GEN-001, ADR-B-CNS-002, ADR-B-CNS-003, ADR-B-OCD-002, ADR-B-OCD-004, ADR-B-OCD-005, ADR-B-OCD-006
- **Related:** ADR-B-OCD-008, ADR-B-OCD-010, ADR-B-OCD-011

## Context

One node persists one finalized chain boundary in several independently committed
stores. Reth owns canonical blocks, receipts and execution state; Commonware/marshal
owns certified consensus history; the compressed-entity tree owns an authenticated
`FinalizedMarker`; Mongo owns derived documents plus a projection checkpoint. A
process or machine crash can occur between any two commits.

Equal heights do not prove equal histories. Recovery must compare hashes and, where
applicable, CE roots and commitment versions. A store that is behind may be rebuilt
only from authenticated canonical durable input. A store that is ahead or conflicts
must never silently become authority.

This ADR owns restart classification and convergence. Snapshot acquisition and new
node bootstrap belong to ADR-B-OCD-008; service health presentation belongs to
ADR-B-OCD-010.

## Decision

### One recovery gate before participation

Node startup constructs a typed `RecoveryVectorV1` before consensus participation,
proposal building, transaction admission or authoritative RPC readiness:

| Component         | Required durable identity                                               |
| ----------------- | ----------------------------------------------------------------------- |
| Chain environment | chain id, genesis hash and ADR-B-OCD-006 manifest identity              |
| Consensus         | certified/finalized height, block hash and certificate/archive identity |
| Reth              | canonical/finalized height and hash, with receipts/body availability    |
| CE tree           | scheme version, height, block hash, parent hash/root and new root       |
| Mongo projection  | height, block hash, schema/network identity and writer epoch            |

The recovery coordinator owns the startup gate. Individual actors may validate their
local store, but they cannot independently declare the node ready.

### Authority and classification

A valid consensus certificate establishes finality; the matching canonical Reth
block, receipts and execution outputs are the durable replay source. Neither Mongo
documents, a CE candidate nor a height-only marker can select canonical history.
The coordinator first proves consensus and Reth agree at the exact finalized hash.

Each derived store is then classified against that boundary:

- **Equal:** exact height, hash, version and root/identity agree; no write is needed.
- **Behind:** its marker is an ancestor and every missing canonical replay input is
  durable; replay sequentially and verify every resulting checkpoint.
- **Ahead:** it contains effects beyond certified durable finality; quarantine and
  roll back through a store-owned, authenticated rollback operation, or fail closed
  if that operation is not implemented.
- **Conflict:** equal height with a different identity, a non-ancestor marker, wrong
  root/version/environment, or discontinuous replay; fail closed and require an
  explicit repair/import procedure.
- **Unavailable/corrupt:** do not participate or advertise readiness. Retry only
  failures classified transient under ADR-B-OCD-010.

Finality never regresses merely to accommodate a derived store. Operator deletion,
checkpoint editing and transaction resubmission are not recovery algorithms.

### Ordered convergence

Recovery proceeds in the following order:

1. acquire exclusive writer leases/fencing epochs for every mutable derived store;
2. validate one chain environment identity across all stores;
3. reconcile consensus certified finality with Reth canonical history;
4. discard non-authoritative speculative CE candidates;
5. replay missing CE blocks from canonical receipts/events, verifying parent hash,
   parent root, computed root and final marker at every height;
6. replay missing Mongo projections in canonical receipt/log order, committing
   documents, indexes and checkpoint atomically per block;
7. re-read the complete recovery vector and require exact convergence; and
8. publish readiness and start consensus participation.

A crash during recovery is safe to retry. Every per-block operation is idempotent or
detects an already committed exact checkpoint. The coordinator never advances its
reported boundary before the corresponding store commit is durable.

### History retention and repair

Reth retains bodies, receipts and Outbe execution artifacts required to rebuild CE
and Mongo through the advertised recovery window. Pruning cannot cross the oldest
unreconciled durable checkpoint. If required history is unavailable, startup reports
the exact missing height and uses the authenticated snapshot/import flow in
ADR-B-OCD-008; it must not synthesize events from current state.

An ahead/conflicting store is preserved for diagnosis before destructive repair.
Repair tooling takes an expected chain identity and target checkpoint, produces an
audit report, and cannot run while a live writer lease exists.

### Evidence

Crash-point tests stop the process before and after every consensus, Reth, CE, Mongo
document and checkpoint commit. On restart they prove exact convergence or the
specified fail-closed class. Tests cover equal, behind, ahead, same-height conflict,
missing history, corrupt body/event/root, wrong chain identity, lost lease and a
second writer. The matrix runs validator and certified-follower roles against real
Reth, marshal, MDBX and Mongo stores.

## Authoritative interfaces

| Responsibility                | Authority                                                         |
| ----------------------------- | ----------------------------------------------------------------- |
| Finalized chain selection     | verified consensus certificate plus matching Reth canonical block |
| Canonical replay input        | durable Reth block, receipts and execution artifacts              |
| CE durable progress           | exact `FinalizedMarker`                                           |
| Mongo durable progress        | exact `ProjectionCheckpoint` plus environment/writer identity     |
| Startup convergence/readiness | node recovery coordinator                                         |
| Snapshot/import repair        | ADR-B-OCD-008                                                     |

## Invariants

- No two stores are considered equal by height alone.
- Derived state never changes canonical/finalized chain selection.
- Replay is contiguous and verifies the exact parent identity before every write.
- CE computed roots must equal canonical durable roots before publication.
- Mongo checkpoint advances atomically with all effects of that block.
- Speculative CE candidates have no restart authority and are discarded first.
- Only the current fenced writer epoch may mutate a derived store.
- Consensus participation begins only after a second full-vector equality check.
- Recovery is idempotent under a crash at every durable write boundary.

## Atomicity, replay and failure

There is no distributed transaction across the four stores. Safety comes from one
authoritative finalized boundary, exact checkpoints, per-store atomic block commits,
ordered replay and a startup barrier. A transient outage pauses convergence without
advancing readiness. Deterministic mismatch, missing canonical history, corruption
or lease loss is fatal for the current process.

Replay uses original canonical block/receipt order and production decoders. A replay
implementation cannot call externally mutable services or re-evaluate wall-clock
policy. Store-specific rollback must be bounded by a verified ancestor and must
remove state and indexes atomically with checkpoint movement.

## Compatibility and migration

`RecoveryVectorV1` and every stored checkpoint are versioned. Adding an identity
field requires a migration that can derive and verify it from retained canonical
history; absence cannot silently mean “compatible”. Old height-only checkpoints are
untrusted until upgraded against an exact canonical hash. Changing chain identity or
commitment scheme uses ADR-B-OCD-006 and ADR-B-OCD-015 rather than ordinary restart replay.

## Production-interface verification evidence

Inspected projection readiness/checkpoint comparisons and ExEx finalized-block
stream, Mongo writer lease supervision, consensus execution recovery seeding, and
the CE startup recovery coordinator. CE currently discards speculative candidates,
classifies exact markers and replays contiguous canonical blocks while checking
parent hashes/roots and computed roots. Projection readiness compares exact height
and hash and exposes deterministic failure classes. For the bounded case where Reth's
canonical head leads marshal's initialized durable tip, startup first selects the
lower height. Once marshal is running, its archived finalization record supplies the
certificate proposal payload: if that exact digest equals Reth's canonical head hash,
startup reconciles Executor and FinalizationView at the head even when marshal
initialization lagged its archive by one delivered block. If the head record is absent,
the execution-only suffix remains speculative and may be replaced when the network
finalizes forward; if an archived record names a different same-height digest, startup
fails closed. These are strong component contracts, but no inspected production
interface assembles and reconciles one complete cross-store vector across consensus,
Reth, CE and Mongo. Status remains Proposed.

## Consequences

Restart becomes a defined consistency protocol rather than an order of component
initializers. Operators receive a precise failed store/boundary instead of being
asked to delete data, while module audits gain one place to prove that partial
cross-store effects cannot become legal state.

## Rejected alternatives

- **Use the minimum stored height:** it can hide forks and regress certified finality.
- **Trust the most advanced store:** a derived or partially committed store is not
  consensus authority.
- **Compare only heights:** equal heights may name different blocks or CE roots.
- **Delete Mongo/CE automatically on mismatch:** it destroys evidence and may erase
  recoverable state under the wrong chain identity.
- **Re-submit user transactions:** replay must reproduce finalized execution, not
  create new transactions with new ordering or context.

## Open questions and technical debt

1. **Critical:** there is no production coordinator that captures and validates one
   `RecoveryVectorV1` across consensus/marshal, Reth, CE and Mongo before enabling
   participation.
2. **Critical, partially closed:** the bounded execution-ahead-of-consensus path no
   longer promotes an execution-only head to finalized state. Startup anchors at the
   durable marshal height unless the marshal archive returns an exact head
   finalization whose certificate payload digest equals the canonical Reth head hash.
   Complete the ancestry proof and define behavior for leads outside the bounded
   in-flight window; this exact local certificate/hash check is still not the complete
   cross-store reconciliation protocol.
3. **Critical:** Mongo `ProjectionAhead` is surfaced as an error, but no inspected
   rollback/quarantine API restores it to the certified canonical boundary.
4. CE recovery deliberately fails on `MarkerAhead` and `MarkerConflict`; define the
   authenticated operator repair/import path rather than requiring ad-hoc MDBX
   deletion.
5. The recovered marshal certificate payload is now compared with the canonical Reth
   hash before it seeds `LastCanonicalized`; prove the archive/certificate trust chain
   as part of the complete recovery vector rather than treating this local comparison
   as sufficient global authentication.
6. The current CE recovery seam accepts `consensus_finalized_height`; make the exact
   consensus hash/certificate identity an explicit input instead of discovering it
   indirectly through Reth.
7. Cross-store commits have no common recovery session/epoch journal. Add a durable,
   diagnostic recovery report without turning it into a second finality authority.
8. Define the exact Reth retention/pruning contract for historical receipts, bodies,
   CE events, roots and retirements needed by both replay pipelines.
9. Projection failure already distinguishes `HistoricalReceiptsUnavailable` and
   `CorruptBody`; connect those classes to ADR-B-OCD-008 import eligibility and prohibit
   endless transient retry.
10. Prove Mongo block projection atomically covers every document, secondary index
    and its checkpoint, including process death and duplicate replay.
11. Bind Mongo checkpoint metadata to chain/genesis identity, projection schema and
    writer fencing epoch; height/hash alone is insufficient across database reuse.
12. Prove lease loss interrupts an in-flight Mongo block before checkpoint publish,
    and that an expired writer cannot commit after a successor acquires authority.
13. Specify CE rollback or immutable-generation replacement semantics for an ahead
    marker. Discarding speculative candidates handles only unfinalized staging.
14. Add equivalent exact checkpoint/hash/root reconciliation for every durable
    sidecar discovered by the full codebase audit, including Mongo and CE.
15. Add real crash-injection tests at every durable boundary. Current component tests
    cover classifications and replay but not a whole-node multi-store restart matrix.
16. Define operator commands for inspect, quarantine, replay and verified repair with
    dry-run output and an audit artifact; manual checkpoint editing must be rejected.
17. Re-read all store identities after replay before readiness. Component-local
    success must not allow a race or second writer to invalidate global convergence.
18. Decide the maximum automatic replay window and the exact threshold at which a
    node must switch to ADR-B-OCD-008 authenticated snapshot/bootstrap.
