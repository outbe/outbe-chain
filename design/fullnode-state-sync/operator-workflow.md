# Snapshot workflow and responsibilities

## Goal

Let any user start a new node from snapshot height/state instead of replaying
from genesis. The snapshot contains **ready native outbe-chain, OCOMP and CE
files**, with their actual durable progress. It is not a logical data export
that must be imported, recalculated or rebuilt before use.

## Scenario

1. Stop outbe-chain, OCOMP and CE, including their embedded/background writers.
2. Create a snapshot containing the native files, metadata, checksums and the
   **mandatory creator signature** identifying its public key.
3. Transfer it by any means. Hosting, storage lifetime and transport are external.
4. Place the received native files at the ordinary configured filesystem paths.
   If the transport artifact is a tar, this is conventional extraction/copy of
   bytes. There is no snapshot restore command, database import or reconstruction.
5. Optionally run the separate offline validation command.
6. Start the node with the ordinary command. It uses the copied native state and
   progress, continues chain/OCOMP and catches up with the network.

**Finishing Lysis is not a prerequisite.** Choosing a quiet moment outside OCOMP
computation is an operator recommendation for a simpler cut. Creation must not
wait for Lysis, require a completed result, fabricate completion or reject native
unfinished work merely because it is unfinished. Stopping all writers before
copying remains mandatory. Their actual durable records are preserved as found.

## Responsibilities

| Actor | Responsibility |
|---|---|
| Producer operator | Stop all writers, choose the cut, create and sign the snapshot. |
| Recipient operator | Select source/signer, obtain files, place them at configured paths, provision own identity/config/enclave, optionally validate and decide to start. |
| Create command | Package ready native files, record actual metadata, compute file checksums, require the creator signature and publish the complete artifact. |
| Validate command | Observe selected file/provenance/state/OCOMP checks offline and report exactly what passed, failed or lacked input. |
| Running node | Use ordinary startup, native progress and protocol checks; continue processing and catch up. |

The operator's source choice does not change ordinary protocol validation. The
node has no special first-start selector, snapshot restoration path or required
validation receipt. Later restarts use current state K without the original
snapshot, signed inventory or repeated full validation.

## Creation and ready-file layout

Creation reads only stopped files. Record finalized H/hash separately from
execution/partial-state, CE, projection and OCOMP closure frontiers. Do not stamp
H over every marker, align stores through a new shutdown mechanism or truncate a
native tail. Cheap inspection and file checks are not the full semantic audit.

The versioned manifest records chain/genesis, actual frontiers, format versions,
producer/time, finite domain inventory and each relative path/size/digest. It
identifies packaged domain paths and their ordinary target roots/configuration
arguments; donor absolute paths never control recipient placement. See the
[native layout](data-layout.md) for the exact boundary.

Manifest decoding checks its format and consistent inventory fields. It preserves
declared paths and member order; it does not require a path location or lexical
sorting. Signature verification uses the exact original bytes. The optional file
check compares actual files with the declared checksums.

The manifest and signature are separate portable evidence. The mandatory signature
covers exact raw manifest bytes using the fixed domain-separated scheme in
[task02](task-planning/02-create-a-signed-snapshot-for-new-nodes.md). Missing key or
signing failure prevents successful creation. Optional creator/location and
validator-status metadata are signed statements; signature validity identifies
the signing key and its attestation, not an independently verified validator role.
An operator-selected expected public key expresses whose signature is expected.

Native DB files, indexes, OCOMP records and CE trees are already present in the
artifact. Placing one terabyte means transferring/writing its bytes, not rebuilding
state from a hundred million blocks. No full temporary native copy or recipient
recalculation is required merely to use the artifact. Archive packing/extraction
is transport byte handling, not a new synchronization or recovery algorithm.

The operator places data while recipient writers are stopped. Own keys,
configuration, enclave identity and signing-safety records remain separate;
producer authority is not imported. Existing DBs must not be mixed with an
unrelated cut. This feature supplies no multi-root installer, force replacement,
restore/resume/rollback journal or automatic evidence merge. Ordinary filesystem
placement/permissions and new-node provisioning stay operator actions.

### Create and place the files

After stopping all donor writers, use the donor's ordinary node configuration:

```sh
outbe-chain snapshot create \
  --output /srv/snapshots/snapshot.tar \
  --signing-key /etc/outbe/snapshot-creator.hex \
  --creator "operator label" --source "donor label" \
  -- \
  --chain /etc/outbe/genesis.json \
  --datadir /srv/outbe/chain \
  --consensus.storage-dir /srv/outbe/consensus \
  --projection.storage-config /etc/outbe/offchain.toml
```

The signing key must already exist. The output's parent directory must exist and
the output file must be absent and outside the native stores and protected paths.
Pass any donor static-file/execution-RocksDB overrides after `--` as well.
Creation reports the archive path, finalized height/hash, file count/bytes and
creator public key. This authenticates the recorded cut; it is not a full data audit.

Transfer the archive using any existing file-transfer method. On the recipient,
extract it into a separate directory with ordinary `tar`, then copy each payload
domain's contents into the corresponding root from the
[destination table](data-layout.md#portable-archive-paths-and-ordinary-destinations):

```sh
mkdir -p /srv/received/snapshot
tar --no-same-owner -xpf /srv/received/snapshot.tar -C /srv/received/snapshot

# Example: D is the recipient's configured chain directory.
mkdir -p /srv/new-node/chain
cp -a --no-preserve=ownership -- /srv/received/snapshot/payload/execution-db/. /srv/new-node/chain/
cp -a --no-preserve=ownership -- /srv/received/snapshot/payload/ce/. /srv/new-node/chain/
```

The two copy commands illustrate the D domains only. Place **every supplied
domain with recorded entries** using the destination table, including separately configured roots, while
recipient writers remain stopped. Use the intended node filesystem owner and
preserve native modes. On a new node, use empty data destinations; do not merge a
different existing database into the received cut. Keep recipient keys, configuration,
enclave identity and signing journals separate from these copy operations.
An empty entries list records an absent optional population: its empty archive
wrapper does not require creating a missing store. Preserve explicitly recorded
empty native directory entries.

No snapshot-specific command is required after placement. The optional validation
command is independent; ordinary startup consumes the placed native files and the
recipient's normal configuration. Starting and subsequent restarts do not consume
the archive, manifest, signature or validation report.

## Optional offline validation

The independent command can inspect received native files or an existing stopped
node. It starts no node/enclave and changes no authoritative data. The operator
chooses checks. Expensive state/tree verification may traverse the full dataset
and use disposable scratch to recompute commitments for comparison. **That work
belongs only to validation; it does not rebuild installed data and is never a
required placement/start step.** No numeric speed/resource guarantee is made.

| Check | What is checked and why |
|---|---|
| Files | Actual native payload paths, lengths and hashes against the original signed inventory; detect changed, missing or extra data. Identity/configuration are outside payload domains. |
| Provenance | Exact original manifest signature and, if supplied, expected signer; identify who signed the artifact. |
| Headers | Retained canonical header hashes, numbers and parent links, including exact H. This is not EVM replay or an independent finality proof. |
| EVM state/code | Complete current E state root from authoritative v1/v2 tables, compared in disposable scratch with header E; verify referenced bytecode. No installed-state rebuild or historical EVM rewind. |
| CE and bodies | Current CE leaves/branches/roots, primary bodies and indexes. Full CE/body equality needs matching actual height/hash; stopping alone does not prove equality. |
| OCOMP | Actual-stage public records, immutable current-state jobs/finality, retained headers and required input/output artifacts; detect absent whole required jobs through native indexes. No Lysis reexecution. |

Results are Passed, Failed for contradictory bytes, Incomplete for unavailable
required input/view, or NotRequested. Selected Failed/Incomplete returns nonzero.
Native-only all includes native checks and adds artifact checks when metadata is
supplied; explicitly requested files/provenance without metadata is Incomplete.
A report is optional evidence and never startup authority.

Each native check requires its own header: an unavailable historical CE header
does not prevent checking an available current EVM state. When CE and projection
have different saved heights or hashes, `body_structure` records a completed
primary/index and retained-body audit at the projection checkpoint; the overall
`bodies` result remains Incomplete because CE/body equality is unavailable.
A null `body_structure` makes no completed structural claim.

With the recipient writers still stopped, validate the placed files using the
recipient's ordinary configuration. Keep the report outside the native stores:

```sh
outbe-chain snapshot validate \
  --manifest /srv/received/snapshot/manifest.json \
  --signature /srv/received/snapshot/signature.json \
  --expected-signer "$SNAPSHOT_CREATOR_PUBLIC_KEY" \
  --checks all --report /srv/received/validation.json \
  -- \
  --chain /etc/outbe/genesis.json \
  --datadir /srv/new-node/chain \
  --consensus.storage-dir /srv/new-node/consensus \
  --projection.storage-config /etc/outbe/offchain.toml
```

`SNAPSHOT_CREATOR_PUBLIC_KEY` is the public key whose signature the operator
expects. Use the creator-key spelling reported by `snapshot create`. The command
checks the placed native files; unpacked transport directories are not the node's
data paths. Preserve any configured static-file/execution-RocksDB overrides.

After an ordinary later stop at current height K, native checks can run without
the original archive or metadata:

```sh
outbe-chain snapshot validate \
  --checks headers,evm,ce,bodies,ocomp --report /srv/received/current-k.json \
  -- \
  --chain /etc/outbe/genesis.json \
  --datadir /srv/new-node/chain \
  --consensus.storage-dir /srv/new-node/consensus \
  --projection.storage-config /etc/outbe/offchain.toml
```

In this invocation files/provenance are NotRequested. Read each selected result
in the report; an unavailable comparison remains Incomplete rather than Passed.
Neither invocation is part of the ordinary node command.

Current OCOMP obligations come from the validated live scheduler, every pending
NOD FIFO entry and permanent Intex series index for open unpaid payout rounds.
Check all required remaining batches/bitmap words, not just present directories.
This is optional offline data inspection, not a runtime payout scheduler or
guarantee that the node will select every historical round. The existing payout
search window, retries and transaction behavior remain unchanged.
This completeness claim follows ordinary native production transitions/indexes;
arbitrarily injected out-of-index state is outside that claim. Future certification
without an open payout round is not presently payable work.

Apply native lifecycle rules: unfinished admissions/results, sparse closure,
retired spool records, Released pins and partial GcPending bodies are not forced
into a completed-state shape. Compare a local result to terminal authority only
when that authority exists. Do not demand deleted nonrequired history.

Keep original manifest/signature only if later provenance/file checking is wanted.
Original file hashes describe the original cut before runtime writes, not databases
after normal progress to K. Native-only validation and startup need no sidecars.
The [validation audit](validation-audit.md) records exact inputs and limitations.
Ordinary startup prerequisites and continued processing are tested separately in
tasks08/09; a passed subset of offline checks is not a universal startup guarantee.

## Implementation boundaries

Only create and optional validate are new product commands. Preserve ordinary
node startup/shutdown and chain/OCOMP recovery. No live capture, mandatory
completed-Lysis gate, state reconstruction on receipt, original-H restart check,
transport service, vendor patch, dependency upgrade or Credis change.

Tests precede implementation. Main E2E uses the recommended post-Lysis cut and checks saved Tribute/NOD,
results and remaining ordinary actions. Creation fixtures prove there is no
completion gate; transferring donor unfinished computation is not required.
The create command and conventional file placement have process-level native-store
coverage, including byte preservation and opening copied stores without the donor or
artifact sidecars. Optional semantic validation and ordinary continuation have separate
acceptance tasks; passing creation and placement tests does not claim those have passed.

## Executed acceptance (2026-09-21)

The complete release scenario passed **21/21 steps** at source commit
`7bfbaeaff84f2636f6c1345c73d0484de2133214`, with a clean checkout. It used four
validators, a separately provisioned FullNode, real Gramine SGX with
`sgx-no-attest`, and chain ID `54322345`. Scenario duration was 1,700,344 ms.
This is a functional result from one local run, not a throughput or resource SLA.

| Observation | Executed result |
|---|---|
| Signed native artifact | H=119; execution/CE/projection=120; 194 files; 13,140,174,951 bytes. Mandatory signature verified. |
| Transfer | Archive SHA-256 `345d3e8b0fbc240f807539f5c7e72586aedfb8c6f919a8ee8155b2a193e8f8f8` preserved. |
| Optional first validation | All seven checks Passed: files, provenance, headers, EVM, CE, bodies and OCOMP. |
| Both placement variants | Ordinary startup from native position 120 passed with validation performed and with validation omitted. No restore command or startup receipt. |
| Copied public work | All 257 NODs materialized; all contributor payouts and recipient balances/NOD bodies matched the canonical observations. |
| New recipient computation | New JobIntent at 685, canonical result at 693. Recipient worker admitted and executed new work; local and canonical result digest matched. |
| Later ordinary restart | Stopped native state K=695; replacement process resumed 695 and finalized through 699. Recipient-owned identity/configuration fingerprints remained unchanged. |
| Optional current-K audit | Headers, EVM, CE, bodies and OCOMP Passed; files/provenance NotRequested, without original sidecars. |

The exact runtime identities and release build commands are retained in
`/tmp/snapshot-09-artifacts-v7.json`. The final rebuild changed only the harness
and verified eight unchanged runtime/input artifact identities. The invocation was:

```sh
sudo -n env RAYON_NUM_THREADS=4 GIT_CONFIG_GLOBAL=/home/ubuntu/.gitconfig \
  ./target/release/outbe-e2e --repo /home/ubuntu/outbe-chain \
  --tee sgx-no-attest --sudo --all --tags @offline-snapshot \
  --concurrency 1 --fail-fast --no-cleanup \
  --data-dir /tmp/snapshot-09-e2e \
  --artifact-manifest /tmp/snapshot-09-artifacts-v7.json \
  --scenario-timeout-secs 3600
```

Retained local evidence:

- `/tmp/snapshot-09-e2e8.log`: terminal success and all 21 steps.
- `/tmp/snapshot-09-e2e/evidence/run-1789989397-3204436/scenario-001.json`: source, profile, process and Oracle evidence.
- `/tmp/snapshot-09-e2e/run-1789989397-3204436/scenario-1/offline-snapshot-evidence.json`: native placements, identities, new worker admission/result and current-K restart.
- The same scenario's `offline-snapshot/` directory: create/validation output, `public-effects.json`, `worker-native-evidence.json`, and `validate-current-k.stdout`.

Workspace verification ran `cargo nextest run --workspace --build-jobs 2
--test-threads 2 --no-fail-fast` and workspace doctests. The initial workspace run
reported 6,582 passes and two CLI fixture watchdog failures. After sharing the
existing file-backed 600-second watchdog, the affected create/files suites passed
3/3; there were no other workspace failures. Both compile-fail API suites passed.
Doctests passed (nine tests, one ignored). Workspace all-target Clippy and the
final affected harness all-target Clippy passed with `-D warnings`; fmt passed.
The final harness regression set passed 86/86. Test-first regression evidence,
independent reviews and exact check logs are recorded in task 09 and its Beads
dependencies. The conditional repository `native-dcap` Clippy lane also compiled;
the acceptance runtime remained `sgx-no-attest` throughout.
