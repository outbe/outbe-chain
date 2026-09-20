# Snapshot implementation tasks

**Audit corrections incorporated:** the [validation audit](../validation-audit.md) maps findings to updated task bodies, file ownership, tests and DoD. Tasks04/06 now specify independent canonical obligation discovery, including entirely missing jobs. This is a corrected implementation plan, not executed validation evidence. The user authorized implementation on2026-09-20, strictly within these eight tasks: tests first, commits after meaningful verified changes, Telegram status every30minutes. An unresolved required deviation must be analyzed and sent to the user before proceeding with affected work.

Contract: [operator workflow](../operator-workflow.md). Data: [native copy boundary](../data-layout.md).
Baseline: main `e5d545cedc872c0e579fd3535e7638d883d5b1f5`; branch `feat/offline-snapshot`.

Beads owns live status. These specifications replace the coarse R01–R04 exports.
Each task names exact files, interfaces/algorithms, dependencies, failing tests
to write first, integration coverage and DoD. No production implementation or
new tests have been run at this planning checkpoint.

## Work sequence

| Task | Beads | Depends on |
|---|---|---|
| [01: Resolve native stores and inspect the stopped dataset](01-resolve-native-stores-and-inspect-the-stopped-dataset.md) | `outbe-chain-g14b.15` | none |
| [02: Create a signed snapshot for new nodes](02-create-a-signed-snapshot-for-new-nodes.md) | `outbe-chain-g14b.16` | 01 |
| [03: Prove direct use of received native snapshot files](03-prove-direct-use-of-received-native-snapshot-files.md) | `outbe-chain-g14b.17` | 02 |
| [04: Verify retained headers and the current EVM state](04-verify-retained-headers-and-the-current-evm-state.md) | `outbe-chain-g14b.21` | 01 |
| [05: Audit every CE tree record, body and projection index](05-audit-every-ce-tree-record-body-and-projection-index.md) | `outbe-chain-g14b.22` | 04 |
| [06: Validate read-only OCOMP relations and expose the standalone audit command](06-validate-read-only-ocomp-relations-and-expose-the-standalone-audit-command.md) | `outbe-chain-g14b.23` | 02, 03, 04, 05 |
| [08: Prove ordinary restart from copied chain and OCOMP stores](08-prove-ordinary-restart-from-copied-chain-and-ocomp-stores.md) | `outbe-chain-g14b.19` | 03 |
| [09: Accept the complete workflow on real Lysis data](09-accept-the-complete-workflow-on-real-lysis-data.md) | `outbe-chain-g14b.20` | 06, 08 |

There are eight active implementation tasks. Number07 is retired; the unrelated payout scheduler expansion is cancelled, not implemented. Existing task numbers and Beads IDs are retained. No task depends on the cancelled work.

## Delivery boundaries

1. **Create:** tasks01+02. Usable archive of ready native files with mandatory signature; no producer dependency on semantic validation or receiver, and no Lysis-completed gate.
2. **Direct file use:** task03 with the create delivery proves conventional extraction/copy and ordinary native opens. It adds tests/docs, not a restore command or reconstruction phase.
3. **Validate:** tasks04+05+06. Complete optional offline command; internal algorithms are developed/tested independently but shipped together.
4. **Ordinary continuation:** task08 proves native copied-store restart using existing chain/OCOMP behavior. No payout scheduler change or new startup lifecycle.
5. **Acceptance:** task09 real stopped-donor Lysis workflow, normal second restart and final workspace checks.

Within each delivery: write failing unit/native fixture tests, implement, pass focused checks, then integration/E2E at the specified boundary. Do not create one massive unverified implementation followed by tests. Every delivered slice must compile and preserve ordinary behavior; no runtime is partially switched to an incomplete path.

Tasks04–06 may develop after01 using native fixtures, but validation ships only when create, direct file placement and full composition work. Run one heavy Cargo lane. Parallel work is limited to disjoint owners and read-only reviews. [File ownership](file-ownership.md) distinguishes semantic from registration-only changes; no existing hot file has more than two planned semantic passes.

## Fixed interfaces and failure behavior

- Ordinary native arguments follow `--` for offline commands. No unified Outbe TOML is invented.
- Create writes a versioned manifest-first tar with exact payload inventory and mandatory domain-separated signature. Filesystem transport is external.
- Ready native files are placed at ordinary configured paths using conventional transfer/copy/extraction. No restore/install command, --force subsystem, reconstruction, staged swaps or rollback/resume journal. Own keys/config/signing-safety remain separate.
- Validate supports selected checks. It opens only their actual prerequisites. Current state is checked at actual E; retained headers cover actual available intervals. Base OCOMP relations use immutable jobs in verified current E plus retained headers; historical EVM rewind is outside scope. Canonical live-job/FIFO/series indexes drive complete current-obligation discovery within ordinary production transitions. Failed/incomplete selected checks return nonzero.
- Ordinary ExEx cursor handling, payout date selection, retries and transaction submission remain unchanged. Snapshot preserves their required native data; optional validation does not add runtime work.
- Manifest format/data consistency, signatures and actual-file checksums remain required by their respective operations. Declared paths and entry order are preserved, without extra path-location restrictions or a sorted-member requirement.
- A report is an observation, never runtime authority. Ordinary start/restart reads native current stores; the archive/manifest/report can be removed.
- Native progress H/E/CE/projection/C remains distinct. Creation follows normal stop of outbe-chain/OCOMP/CE writers; avoiding active computation is recommended only; no live request, temporary consensus halt or cursor fabrication.

## Checks to execute during implementation

Use the pinned toolchain and the existing CI lanes. Planned commands below are not previously passed results. Each Cargo build/test uses `RAYON_NUM_THREADS=4` and `-j4`; repository wrapper tasks additionally receive `CARGO_BUILD_JOBS=4`. Subagents use at most2 and do not run a second heavy lane.

```sh
cargo fmt --all --check
RAYON_NUM_THREADS=4 SOURCE_DATE_EPOCH=0 cargo test -j4 -p outbe-snapshot
RAYON_NUM_THREADS=4 SOURCE_DATE_EPOCH=0 cargo test -j4 -p outbe-chain
RAYON_NUM_THREADS=4 SOURCE_DATE_EPOCH=0 cargo test -j4 -p outbe-ocomp
RAYON_NUM_THREADS=4 SOURCE_DATE_EPOCH=0 cargo test -j4 -p outbe-compressed-entities -p outbe-tribute -p outbe-nod -p outbe-metadosis -p outbe-node -p outbe-engine
RAYON_NUM_THREADS=4 CARGO_BUILD_JOBS=4 SOURCE_DATE_EPOCH=0 mise run test
RAYON_NUM_THREADS=4 CARGO_BUILD_JOBS=4 SOURCE_DATE_EPOCH=0 mise run lint
```

The package commands belong to their changed task; do not rerun all packages after every small edit. Final `mise run test` covers workspace nextest and doctests; lint includes the configured native-dcap compile lane when available. Required compile-time API tests from CI remain included in final verification. Do not use indiscriminate --all-features. Native-dcap compilation is separate from the no-DCAP E2E runtime profile.

E2E uses current release binaries, sudo, real gramine-sgx, `--tee sgx-no-attest`, materialized testnet chain_id54322345, no DCAP/QVL and no invented service users. Task09 records the exact existing harness invocation/feature filter and artifact hashes before execution. Complete the harness preflight before expensive builds; only changed/missing runtime artifacts need rebuilding.

## Source review and remaining proof

- [Offline APIs and validation route](replan-artifact-source-review.md).
- [Native recovery and direct-copy evidence](replan-runtime-source-review.md).

Two agents reviewed source and task maps independently and exchanged bounded findings. The latest user correction removes the recipient restore subsystem and makes quiet Lysis timing a recommendation. Optional root recomputation is validation-only, never file placement or startup. Planning corrections include stable Marshal fixture prefixes, read-only catalog bridges, selected-check prerequisites GC-aware requirements, nested NOD paths, current-E bindings and independent canonical obligation discovery.
Source reviews establish concrete implementation routes. Exact Rust generic compilation, data-corruption regressions, fresh file-placement/start and release E2E are still required; this plan does not claim they already passed.

There is no unresolved product question requiring a new design interview. Unsupported real data or a failing native fixture must be reported with the exact task/file/invariant, rather than hidden by a startup subsystem or third-party patch. The user-approved non-goals remain binding.
