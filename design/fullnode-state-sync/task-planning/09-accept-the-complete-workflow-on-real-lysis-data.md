# 09. Accept the complete workflow on real Lysis data

Beads: `outbe-chain-g14b.20`. Baseline: `e5d545cedc872c0e579fd3535e7638d883d5b1f5`.

## Outcome

Prove a new node receives ready native signed snapshot files from the recommended post-Lysis cut, places them at configured paths, optionally validates and starts normally; it catches up, preserves Tribute/NOD/results, continues pending ordinary materialization/payout and restarts at current progress. Cross-identity continuation of donor unfinished computation is not a feature-readiness requirement.

## Dependencies and delivery

Depends on: 06, 08.

Final acceptance PR; test-only process helpers and operator documentation, no production fix hidden in harness work.

## Exact file changes

| File | Action | Responsibility |
|---|---|---|
| `testing/e2e-harness/features/ocomp.feature` | modify | Add a focused snapshot scenario using the real existing Lysis flow, recommended post-Lysis quiet cut and pending ordinary public-action continuation, independent receiver and later restart. |
| `testing/e2e-harness/src/features/ocomp/offline_snapshot.rs` | new | Invoke real public snapshot CLI commands and ordinary node command; inspect exact native/chain evidence and protected identity sentinels. |
| `testing/e2e-harness/src/features/ocomp/mod.rs` | administrative | Register offline snapshot steps. |
| `testing/e2e-harness/src/world/localnet/mod.rs` | modify | Minimal ordinary donor stop/start and saved launch-options helper; join/reap writers, no capture controller. |
| `testing/e2e-harness/src/world/localnet/follower.rs` | modify | Provision/start a distinct recipient in unrelated native directories using ordinary follower startup. |
| `testing/e2e-harness/src/world/ocomp/processes/restart.rs` | modify | Reuse normal client stop/restart ownership so exporter/worker writers are stopped before packaging. |
| `testing/e2e-harness/src/world/state.rs` | modify | Typed snapshot scenario observations: artifact path/digest, native heights/hashes, receiver identity fingerprints and CLI exit reports. |
| `design/fullnode-state-sync/operator-workflow.md` | modify | Document the implemented commands, stopped-writer precondition, direct native file placement, validation results and measured evidence limits. |

## Implementation

1. Use the real Tribute→Metadosis→OCOMP→Lysis→NOD flow with a completed-Lysis quiet cut for primary process acceptance. This is recommended operator timing, not a creation gate. Stop all outbe-chain/OCOMP/CE writers before copying, preserve actual durable records and record H/Finish/partial-state/Q/P/C without forcing equality. Creation unit/native fixtures prove there is no completed-Lysis check; do not make transferring donor unfinished computation a separate E2E acceptance requirement.
2. Normally stop/reap outbe-chain, OCOMP and CE writers. Run create, transfer the artifact by ordinary file tools, place/extract its native payloads directly into recipient configured paths, then use ordinary node startup. No restore CLI, data import, replay, state/index rebuild, live capture, injected completion or mandatory validation is part of file placement.
3. Run snapshot validate in the validating variant; also prove ordinary startup when validation is skipped. Check original signed bytes before native runtime changes them. Record every selected result and do not turn Q!=P into body-equality success. Remove donor/archive/metadata access before node startup; no receipt is passed to the node. Expensive validation scratch never becomes a deployment prerequisite.
4. Observe actual H/hash and native chain/OCOMP progress, continuing finality, saved Tribute/NOD and completed result reuse, plus pending public materialization/payout. Task08 fixtures cover native cursor lag independently of quiet-cut timing. No claim that a new node must resume the donor unfinished Lysis attempt under another identity.
5. Perform a new genuine job/block progression after file placement, reach K>H, stop normally, and restart with ordinary node command. Compare result/NOD/public effects and prove current-K recovery without original artifact. Original-cut file hashes are not current-K invariants; a later normal restart never reads the old manifest/report.
6. Use focused role/protected-key and tamper/partial-transfer variants. Logical time derives from materialized genesis; preserved public artifact availability is explicit. Do not simulate success by rewriting chain state, cursor, donor transaction authority or enclave data. Payout receipt assertion uses an already authorized validator with its own resident TEE/keys, never grants validator behavior to the new FullNode.
7. Finish harness preflight before expensive release build; rebuild only changed runtime inputs. Use current release binaries, sudo, real gramine-sgx, --tee sgx-no-attest, chain_id54322345, no DCAP/QVL. No invented service UID model.
8. Run fmt, workspace unit/doctests, applicable feature lanes and zero-warning Clippy. Independently review diff against main for every retained caller, native data coverage and no live-capture/bootstrap/transport dependencies. Record exact commands/counts/revisions and known baseline failures honestly.

## Tests first

1. Evidence assertion rejects missing CLI phase, wrong hash/height, zero post-placement progress, changed protected identity, copied result mislabeled as new compute, or no second restart.
2. One full release Lysis acceptance plus focused meaningful failure/role variants; no five-run campaign or debug fallback.
3. Complete current workspace checks and independent source/scope review after implementation, not earlier discarded branch evidence.
4. Primary acceptance uses a new recipient with no previous chain/OCOMP history. Creation requires a signature; the recipient can identify/verify its signer using only the artifact. Ordinary startup resumes copied native progress rather than replaying genesis, then catches the advancing network.
5. Negative obligation fixture removes an entire required public job/dependency closure; independent canonical discovery reports missing input. Native-only current-K validation has files/provenance NotRequested without sidecars; lawful GC alone is not a failed current-capability assertion.
6. Primary process proof targets post-Lysis stored data/results and pending public NOD/payout actions. Creation fixtures separately prove no completion gate. Donor unfinished-computation migration is not a hidden delivery criterion.

## Definition of done

1. Real create→transfer/place-files→optional independent validation→ordinary start→new chain/OCOMP work→ordinary restart is demonstrated.
2. Snapshot contains required nonempty native data and excludes donor authority; ordinary lifecycle remains free of snapshot gates/controllers.
3. All required checks pass or concrete preexisting failures are explicitly reported without claiming full green. Source artifacts and instructions are sufficient for another operator to reproduce the workflow.
4. Snapshot acceptance requires no change to payout search windows or scheduling. Any pending payout observation uses the existing runtime selection rules and an eligible recipient; historical payout expansion is excluded.

## Execution discipline

Write the listed failing behavioral unit/fixture tests first, implement the complete slice, then run its integration checks. Task 09 owns real process E2E. Do not count source review or tests from the discarded branch as execution evidence. Keep live progress in Beads.
