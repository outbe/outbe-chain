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
Plans describe future work; no implementation or executed acceptance is claimed.
