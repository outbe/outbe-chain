# Snapshot commit necessity and completeness audit

Frozen baseline: `e5d545cedc872c0e579fd3535e7638d883d5b1f5` (`main`).
Frozen implementation HEAD: `66ed773393dd175f9fcf8f363f5f4aaeb129004d`.
Review date: 2026-09-21. Tracking: `outbe-chain-2nvp`.

PR #447 subsequently required Conventional Commit messages. The
[reword mapping](commit-reword-map.tsv) maps every changed commit ID below to its
replacement, including descendants whose IDs changed only through their parents.
Every mapped commit retains the exact original Git tree, author and timestamps.
The audit and execution evidence below refer to the original, frozen IDs; rewording
does not imply a new test run. Tracking: `outbe-chain-44sm`.

This report audits all **62 implementation-branch commits**, in their exact Git
order. Its later documentation commit is outside the frozen implementation range.
The user requested necessity of every commit, unrelated work, and missing required
functionality. No product changes were made during this audit. Keno and Mera completed independent
first passes, then challenged each other's conclusions and the coordinator's 62-row
matrix. Both final dispositions found no remaining actionable scope/specification
finding. Their only initial classification difference, row 41, was resolved against
the original task clause and the exact code preimage.

## Contract

The approved [workflow](operator-workflow.md) and [eight tasks](task-planning/README.md)
require stopped outbe-chain/OCOMP/CE writers; ready native-file creation with metadata,
checksums and a mandatory creator signature; ordinary external transfer/placement;
optional standalone offline validation; ordinary startup, chain/OCOMP catch-up and
later restart with the recipient's own configuration and authority.

Tasks 01–03 deliver creation and direct file use. Tasks 04–06 deliver the explicitly
requested independent native audits. Tasks 08–09 prove existing continuation through
native fixtures and real process acceptance. Optional invocation of validation does
not remove the completeness requirements of a check that the operator selects.

## Standards review

Keno independently reviewed all 62 historical patches and the final scope. No new
actionable documented-scope violation or unrelated whole commit was established.
The historical path union equals the final 116-path diff; every code/test/build path
maps to the approved task inventory. External Cargo name/version/source/checksum
identities remain unchanged at every reviewed revision. No vendor, dependency fork,
Credis modification or cancelled task 07 scheduler change was introduced.

Ordinary node startup gained only explicit `snapshot` command dispatch. Launch,
consensus recovery and ExEx implementation were not replaced. Existing owners gained
observational APIs and narrowly shared decoding/validation: for example, export
binding still passes the runtime's outer cursor independently of its spec, and local
result recovery still precedes its extracted record-validation helper. Common
bounded-read and shared-lock helpers do change handling of malformed input; this is
not a claim that no preexisting function changed. No new live capture, shutdown
controller, startup receipt or mandatory historical-H validation was added.

## Specification review

Mera independently mapped all 62 actual patches to exact task clauses and reviewed
the final diff. No new remaining specification blocker or unrelated whole commit was
found. Required creation, signing, placement, selected native checks, protected
identity boundaries and ordinary continuation have implementation and execution
evidence. Broad CE/OCOMP audits implement tasks04–06; they do not run automatically
when a recipient starts its node. Historical payout-file inspection stays offline
and does not extend the ordinary payout scheduler's date window or submit work.

The prior two final-audit defects are corrected in `58a7316a` and `66ed7733`.
The preceding NodeHost exclusion and independent-validation/export-coverage defects
are corrected in `f25374c1` and `633c6094`; these earlier defects are not silently
counted as having been correct from their introduction.

## Necessary final effects versus avoidable history

All 62 commits have a defensible relationship to the agreed work; this is **not** a
claim that exactly 62 separate commits or every helper/test line was inevitable or
minimal. The matrix distinguishes 40 implementation/integration slices, 8 acceptance
slices, 2 documentation commits, and 12 corrective/completion commits.

The 12 corrective/completion commits are `7484dd49`, `f90f3790`, `987a5e45`, `eef888b5`, `fbffbeac`, `f25374c1`,
`15f7f3a6`, `633c6094`, `fd623965`, `7bfbaeaf`, `58a7316a`, and `66ed7733`.
They repair earlier parser/layout/identity/validation assumptions or test-fixture
observations. Their final effects are necessary; the initial mistakes and the need
for separate repair commits could have been avoided. `f90f3790` repairs missing partial-observation reporting in an internal
component before public command delivery. That reporting was already required by
the original plan; the correction adds no data obligation. This is why the broader
correction/completion category includes it, without labeling it a shipped runtime defect.

No whole commit was identified for removal on functional-scope grounds. This audit
does not request history rewriting or new implementation work.

## Original plan versus later amendments

Necessity was checked against the original agreed workflow and task clauses, not
only the final path inventory. Task 04 and task 05 implementation clauses are unchanged
from the original `ae332eb1` plan. Task 06 changed only its shared-lock safety clause
and report-output isolation clause for the explicitly approved final audit fixes;
its large canonical OCOMP coverage was already planned. Task 01/02 changes include
the user's explicit declared-path/order clarification. Task 09 amendments refine
test observations, intentional donor downtime and native next-day/Oracle timing.
The final ownership map incorporates those bounded corrections; it does not prove
that every later file was named perfectly in the initial plan.

## Every commit

Each row identifies an actual changed file; `git show HASH -- PATH` reproduces the
historical evidence. The row states the necessary final effect, not merely the
commit title. Task numbers refer to the linked task inventory above.

| # | Commit | Task | Kind | Required final effect | Primary diff evidence |
|---|---|---|---|---|---|
| 01 | `ae332eb1` | plan | Documentation | Freeze the signed native-file workflow, exact owners, tests and excluded runtime work. | `design/fullnode-state-sync/task-planning/tasks.json` |
| 02 | `c9c588b1` | 01 | Implementation | Read saved OCOMP closure without creating or repairing its journal. | `bin/outbe-ocomp/src/discovery_spool.rs` |
| 03 | `4ff19427` | 01 | Implementation | Separate native payload, identities and output so creation cannot package secrets or write into source. | `crates/blockchain/snapshot/src/layout.rs` |
| 04 | `04b789b9` | 01 | Implementation | Represent independent saved frontiers and signed inventory; retain declared paths/order and validate format/totals. | `crates/blockchain/snapshot/src/manifest.rs` |
| 05 | `d62c7920` | 01 | Implementation | Reuse ordinary configuration to locate native stores without launching a node. | `bin/outbe-chain/src/snapshot/config.rs` |
| 06 | `7484dd49` | 01 | Correction | Correct ordinary inline-genesis support: JSON is not a configuration file path. | `bin/outbe-chain/src/snapshot/config.rs` |
| 07 | `b97fc1ae` | 01 | Implementation | Observe persisted execution/finality/stage/storage-version markers before packaging. | `bin/outbe-chain/src/snapshot/native.rs` |
| 08 | `4a3cd0ff` | 01 | Implementation | Observe CE, projection and OCOMP markers independently without equating their heights. | `bin/outbe-chain/src/snapshot/native.rs` |
| 09 | `fe5b06af` | 01 | Implementation | Enumerate public native files and retained state while excluding local signing authority. | `bin/outbe-chain/src/snapshot/inventory.rs` |
| 10 | `388fc7f4` | 02 | Implementation | Sign and verify exact manifest bytes with a mandatory creator identity. | `crates/blockchain/snapshot/src/provenance.rs` |
| 11 | `d3c0b706` | 02 | Implementation | Expose offline creation, copy native payload, hash it, sign it and publish a complete archive. | `crates/blockchain/snapshot/src/fs.rs` |
| 12 | `c06970e1` | 03 | Acceptance | Prove conventional transfer/extraction/placement and reuse CLI fixtures; add no restore operation. | `bin/outbe-chain/tests/snapshot_files.rs` |
| 13 | `d1b1e3ce` | 04 | Implementation | Expose existing canonical live OCOMP jobs through immutable owner decoding for optional audit. | `crates/core/metadosis/src/ocomp/views.rs` |
| 14 | `0f43603a` | 04 | Implementation | Audit retained headers and rebuild execution roots in external scratch for optional validation. | `bin/outbe-chain/src/snapshot/validation/headers.rs` |
| 15 | `cf10658a` | 04 | Implementation | Read canonical job/NOD/Intex observations from the root-verified execution view. | `bin/outbe-chain/src/snapshot/validation/canonical_state.rs` |
| 16 | `56d879d0` | 05 | Implementation | Audit every persisted CE tree record against rebuilt commitments without modifying the source. | `crates/core/compressed-entities/src/persistence/audit/store.rs` |
| 17 | `953d6ec0` | 05 | Implementation | Compare complete CE/body populations, including missing and extra records, with bounded scratch. | `crates/core/compressed-entities/src/persistence/audit/store.rs` |
| 18 | `ba7b0b9f` | 05 | Implementation | Read native Tribute/NOD primary and index populations for complete optional body validation. | `crates/core/tribute/src/repository.rs` |
| 19 | `6cc3eac3` | 05 | Implementation | Bind CE audit results to the matching retained header and saved frontier. | `bin/outbe-chain/src/snapshot/validation/ce.rs` |
| 20 | `d41cc220` | 05 | Implementation | Join CE and projection body checks at their actual independent checkpoints. | `bin/outbe-chain/src/snapshot/validation/bodies.rs` |
| 21 | `bd1cc180` | 06 | Implementation | Read public local Lysis results without invoking ordinary startup reconciliation. | `crates/blockchain/node/src/ocomp/local_result.rs` |
| 22 | `e3f8a87e` | 06 | Implementation | Read public spool, receipt, materialization and payout artifacts without repair or submission. | `bin/outbe-ocomp/src/discovery_spool.rs` |
| 23 | `b9141669` | 06 | Implementation | Discover current OCOMP obligations from canonical state rather than only existing local files. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 24 | `102f05d8` | 06 | Implementation | Share owner binding/plan validation with immutable admission and export readers. | `bin/outbe-ocomp/src/export_binding.rs` |
| 25 | `614d9732` | 06 | Implementation | Compare required public payout artifacts with current certified OCOMP generation data; no scheduling. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 26 | `dd70efca` | 06 | Implementation | Give each requested audit its own status and bounded diagnostics. | `bin/outbe-chain/src/snapshot/validation/report.rs` |
| 27 | `5ab678e3` | 06 | Implementation | Resolve selected checks and prerequisites without opening unrelated stores. | `bin/outbe-chain/src/snapshot/validation/run.rs` |
| 28 | `782ce27a` | 06 | Implementation | Check all remaining canonical NOD queue inputs, not only the first batch. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 29 | `89b58c11` | 06 | Implementation | Verify retained block/receipt inputs needed by existing OCOMP continuation. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 30 | `1822d5d7` | 06 | Implementation | Check saved closure identity and actual replay coverage without rewriting cursors. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 31 | `46ec0c6e` | 06 | Implementation | Bind retention pins to the canonical request, generation and finality identities. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 32 | `146563fb` | 06 | Implementation | Verify complete currently required Tribute lease populations across live/retained data. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 33 | `0ccc59f0` | 06 | Implementation | Verify exported manifests, input catalogs and every required chunk with existing owners. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 34 | `e14374d1` | 06 | Implementation | Validate surviving admission/plan/local result artifacts according to their native lifecycle stage. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 35 | `ac728d1f` | 06 | Implementation | Check retained materialization references against the correct job using external scratch. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 36 | `3058fb70` | 06 | Implementation | Find immutable historical job authority in retained request frames without historical EVM rewind. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 37 | `e6fcf9cf` | 06 | Implementation | Separate signature/provenance validation from payload correctness and native semantic checks. | `bin/outbe-chain/src/snapshot/validation/run.rs` |
| 38 | `de64fd51` | 06 | Implementation | Compare placed native files with the original signed inventory without repair or relocation. | `bin/outbe-chain/src/snapshot/validation/run.rs` |
| 39 | `4f43a7af` | 06 | Implementation | Authenticate every retained local result against its own canonical job evidence. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 40 | `f31a89dd` | 06 | Implementation | Compose independent active/NOD/payout inventories so deleting whole local job directories cannot hide obligations. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 41 | `f90f3790` | 06 | Correction/completion | Preserve visited counts and partial observations when a later audit relation fails. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 42 | `364edf5b` | 06 | Implementation | Inspect surviving receipts and present CAS objects even if other local discovery records are absent. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 43 | `a003cf5a` | 06 | Implementation | Prepare the standalone validation CLI and no-clobber report output before command activation. | `bin/outbe-chain/src/cli/snapshot/validate.rs` |
| 44 | `6a1c4d3f` | 06 | Implementation | Activate the complete optional validator and exercise native/artifact check selection together. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 45 | `f70a8164` | 08 | Acceptance | Prove ordinary copied-store OCOMP restart while preserving saved processing positions. | `bin/outbe-chain/src/ocomp_exex/tests/recovery.rs` |
| 46 | `e6136d8c` | 08 | Acceptance | Prove copied public data neither imports resident authority nor demands legitimately retired records. | `bin/outbe-chain/src/ocomp_exex/tests/materialization.rs` |
| 47 | `36a08a62` | 08 | Acceptance | Prove ordinary CE/projection recovery with unequal saved checkpoints. | `bin/outbe-chain/src/launch/tests/state_sync.rs` |
| 48 | `3a330f1e` | 08 | Acceptance | Exercise the existing ExEx initialization route on copied native data. | `bin/outbe-chain/src/ocomp_exex/tests/recovery.rs` |
| 49 | `4086f827` | 08 | Acceptance | Prove pending OCOMP work dispatch after ordinary copied-store restart. | `bin/outbe-chain/src/ocomp_exex/tests/materialization.rs` |
| 50 | `87fb8c5b` | 08 | Acceptance | Exercise existing consensus/CE recovery from copied stores with real engine fixtures. | `crates/blockchain/engine/src/stack/tests/recovery.rs` |
| 51 | `c19246ad` | 09 | Acceptance | Add real stopped-donor signed transfer, optional validation, catch-up, new work and second-restart acceptance. | `testing/e2e-harness/src/features/ocomp/offline_snapshot.rs` |
| 52 | `987a5e45` | 09 | Correction | Correct E2E pending-work expectations to the copied execution frontier. | `testing/e2e-harness/src/features/ocomp/offline_snapshot.rs` |
| 53 | `eef888b5` | 06 | Correction | Accept the native exporter work directory instead of misclassifying it as a job ID. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 54 | `fbffbeac` | 09 | Correction | Correct fixture-only downtime budgets and file-backed CLI watchdogs for intentional offline work. | `bin/outbe-chain/tests/common/snapshot.rs` |
| 55 | `f25374c1` | 01 | Correction | Correct the protected root list so NodeHost identity cannot leak via an execution-root override. | `bin/outbe-chain/src/snapshot/config.rs` |
| 56 | `15f7f3a6` | 09 | Correction | Correct E2E next-day timing by waiting for the native protocol-created schedule. | `testing/e2e-harness/src/features/ocomp/offline_snapshot.rs` |
| 57 | `633c6094` | 06 | Correction | Correct missing export requirements and independent EVM/body-structure report results discovered in audit. | `bin/outbe-chain/src/snapshot/validation/ocomp.rs` |
| 58 | `fd623965` | 09 | Correction | Correct E2E observations across advancing public NOD heads rather than read mixed checkpoints. | `testing/e2e-harness/src/features/ocomp/offline_snapshot.rs` |
| 59 | `7bfbaeaf` | 09 | Correction | Refresh E2E Oracle cohort/publication observations after restarting donor processes. | `testing/e2e-harness/src/features/ocomp/offline_snapshot.rs` |
| 60 | `18fde618` | 09/docs | Documentation | Record executed acceptance, exact artifacts and final task ownership. | `design/fullnode-state-sync/operator-workflow.md` |
| 61 | `58a7316a` | 06 | Correction | Correct the read-only input-ref lock open so a FIFO cannot hang optional validation. | `bin/outbe-ocomp/src/input_ref_catalog.rs` |
| 62 | `66ed7733` | 06 | Correction | Correct report publication on provenance-only and early-config-error paths; retain independent stdout results. | `bin/outbe-chain/src/cli/snapshot/validate.rs` |

## Evidence and limits

- New audit: all 62 `git show` patches, final baseline-to-HEAD diff, historical and final
  ownership inventories, existing-owner/corrective code paths, and independent
  standards/spec reviews. Large new modules and fixtures were reviewed by their
  responsibilities and material branches, using the preceding full source audit;
  this is not a claim of a fresh line-by-line formal proof of every historical line.
- Current correction checks: snapshot unit suite 245/245 passed, public OCOMP reader
  suite 24/24 passed, actual report CLI integration 2/2 passed, `cargo fmt --all -- --check` passed, and all-targets Clippy for outbe-chain/outbe-ocomp with `-D warnings`
  passed. Logs: `/tmp/snapshot-audit-chain-tests.log`, `/tmp/snapshot-f1-green.log`,
  `/tmp/snapshot-audit-cli-tests.log`, `/tmp/snapshot-audit-fmt-check.log`,
  `/tmp/snapshot-audit-clippy.log`. RED logs for both corrections were inspected.
- Existing full process acceptance: 21/21 steps at source `7bfbaeaf`, using release
  artifacts, real SGX/no-attestation, signed creation, validated and unvalidated
  recipient placement, new work and later normal restart. The [operator record](operator-workflow.md)
  contains its exact paths/frontiers. That E2E was **not rerun at 66ed7733**; the later
  isolated offline fixes have the current unit/CLI evidence above.
- Earlier workspace run had 6582 passing tests and 2 CLI watchdog failures; the affected
  retry passed after test-watchdog correction. Do not label the original full run
  clean or claim that the whole workspace was rerun after the final two fixes.
- Git commits containing both tests and implementation cannot prove RED-before-code
  chronology. This order was directly observed for the latest corrections and is
  supported by historical logs for sampled earlier work, but was not reconstructed
  for every one of 62 commits. Every intermediate revision was not rebuilt/rerun.
- Graph project outbe-chain, generation 2026-09-04T15:12:00Z: coverage checked for all 116
  paths; 49 excluded/not-tracked, 48 not-tracked, 19 metadata-changed. Findings rely on
  direct Git/source fallback; graph metadata is not completeness evidence.
- Required limits remain explicit: operators stop writers; this is the filesystem/
  RocksDB release; unavailable required retained history yields an incomplete audit;
  unequal Q/P does not establish body equality; canonical obligation completeness
  uses ordinary native indexed transitions. One successful supported workflow does
  not prove universal startup readiness, every adversarial filesystem case, minimum
  implementation size, or measured network-wide uptime/performance improvement.

Independent first-pass and challenge records are retained locally as
`/tmp/keno-snapshot-commit-audit.md` and `/tmp/mera-snapshot-commit-audit.md`.
The coordinator's patch/file inventory is `/tmp/snapshot-commit-audit-metadata.json`.
