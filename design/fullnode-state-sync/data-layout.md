# Native dataset and copy boundary

Baseline: main `e5d545cedc872c0e579fd3535e7638d883d5b1f5`.
This is the input contract for tasks 01–06, not a new runtime layout.

## Roots and frontiers

- D: chain-resolved Reth datadir from the ordinary native arguments.
- S: configured consensus directory, default D/consensus.
- O: parent(D)/ocomp/domain-v1, as ordinary launch resolves it.
- P: offchain RocksDB primary from `--projection.storage-config`; relative paths
  resolve against that TOML. The secondary is disposable scratch. Mongo is outside
  this filesystem-only first release.
- Honor explicit static-file and execution-RocksDB path overrides.

H is the exact Reth `ChainStateKey::LastFinalizedBlock` value and its canonical
header/hash. E is actual stored execution state; M is the CE marker; P also names
the observed projection checkpoint when discussing progress; C is native OCOMP
closure. Record each independently. Execution or public local work may be ahead
of H; closure may lag. Copy the native tail and progress unchanged. No truncation,
synthetic C=H, or universal all-markers-equal requirement. Finish alone does not
prove complete execution state: preserve/inspect partial-state and unwind markers.
A stopped donor freezes its actual progress; this does not establish CE=projection.

All outbe-chain, OCOMP and CE writers are stopped by the operator and stay
stopped during creation or offline inspection. Completing Lysis is recommended
timing only, never a precondition; preserve native unfinished work as found. Read-only opens do not prove this.
No runtime lease, capture endpoint or modified shutdown is introduced.

## Included public synchronization data

| Logical domain | Native content | Coupling that must survive |
|---|---|---|
| Execution database | D/db, complete MDBX files | Native settings, state, canonical mappings, progress and available history |
| Static files | Resolved static-files directory | Corresponding headers/bodies/receipts/changesets and indexes |
| Execution RocksDB | Resolved execution RocksDB primary | Complete corresponding native data; not the offchain DB |
| CE | D/compressed_entities/smt | Identity, actual marker, catalog, shard roots, leaves and branches |
| Offchain projection | Whole P primary | Projection identity/checkpoint, Tribute/NOD bodies, secondary indexes and retained-body namespaces |
| Marshal finalizations | S/outbe-marshal-finalizations-{metadata,freezer-table,freezer-key,freezer-value,ordinal} | Keep native paired archive partitions together |
| Marshal blocks | S/outbe-marshal-blocks-{metadata,freezer-table,freezer-key,freezer-value,ordinal} | Keep blocks, indexes and available ranges together |
| Marshal progress/cache | S/outbe-marshal-application-metadata and all native outbe-marshal-cache prefix partitions | Persisted processed cursor and replay cache; Start::Genesis does not mean execute from zero |
| Parent certificates | S/finalized_parent_certs | Public finalization records |
| OCOMP retention | S/ocomp_retention | Registry with matching retained bodies and export generations |
| Closure/discovery | O/exporter-v1/discovery/closure-checkpoint-v1/checkpoint.v1 and per-bundle offers/acks/pending/quarantine/retirements | Actual C, previous/baseline, offer/export authority and retirement state |
| Installed bundles | O/protocol-bundles-v1 | Native bundle identities used by copied jobs |
| CAS | O/cas-v1/objects | Complete committed objects; referenced bytes/kinds/digests remain usable |
| Export public records | O/exporter-v1/input-refs and receipts; O/supervisor-v1/export-bindings | Exact input coverage, receipts, bindings and lease/source generations |
| Job public records | O/supervisor-v1/jobs/<job>/admissions and contributor-payout-v1.bin | Plan/admission artifacts and dense public payout leaves |
| Public NOD references | O/supervisor-v1/materialization-references/<job>/<first_nod_ordinal>/<job>.materialization-refs-v1.json | Pinned CAS dependencies, not transaction submission authority |
| Native local results/progress | O/node-v1/local-results and exex-checkpoint | Completed canonical result bytes and progress |
| Fatal evidence | O/node-v1/fatal-evidence (separate from exex-checkpoint) | Preserve evidence; file placement must not silently clear fatal state |

Preserve nonsecret native lock placeholders required by read-only catalog APIs;
do not invent a common process lock. Temporary/incomplete entries are handled by
their owner's semantics, never indiscriminately deleted. Required pending recovery
state cannot be dropped merely because its name ends in `.tmp`; creation must
report an unsupported/inconsistent stopped dataset if its inclusion is unsafe.
Uncommitted CAS staging and worker replay inboxes are not completed public results.
The exact enumerator is a finite domain allowlist with tests for every class;
copying the entire consensus/OCOMP parent is not the implementation.

## Protected recipient authority/configuration

Do not export or replace recipient P2P/JWT/private keys, native configuration,
configured BLS/EVM/DKG key files, DKG public-polynomial/output configuration,
configured enclave sealed identity/root/authorization, or donor signing authority.
Resolve overrides and aliases, not only D/keys. The effective validator EVM key
may be a sibling of the BLS key.

Protected OCOMP paths include O/ocomp-evm-key.hex, ocomp-key-v1.hex,
supervisor-v1/sign-once, vote-submissions, materialization-submissions and
payout-submissions. These signed/local records differ from the public NOD/payout
artifacts above. Recipient file placement must preserve its own signing-safety records.

Exclude donor outbe-simplex-* signing/replay authority and configured/legacy DKG
own-key/pending-share/dealer/player retry material. An excluded signing journal
must not be silently regenerated as donor authorization. Recipient provisioning
and current ValidatorSet role continue to follow ordinary rules.

If required data roots overlap protected files/config or each other, refuse the
layout rather than silently omitting data or overwriting secrets. Store donor
source labels as metadata only; absolute donor paths never authorize extraction.

## Read-only routes and normal recovery

- Use pinned Reth RO MDBX and RO static-file readers. State/header verification
  needs no execution RocksDB provider. Reth RocksDBBuilder(read_only) creates a
  sibling secondary under the source parent, so it is not this tool's source path.
- Outbe RocksDbReader accepts an explicit external secondary scratch directory.
- CeMdbxReadOnly already exists. Exhaustive tree/body audit is new work in task05.
- Reuse `inspect_retention_journal`. Add a closure inspector and owner-local
  public artifact readers where current open methods create/recover records.
- Validator native recovery uses its persisted finalized/execution anchor;
  follower uses archive/execution/processed progress. Exact recovered H/hash is
  ACKed without executing EVM again. OCOMP scans canonical blocks/receipts from
  C+1, using copied retention/discovery/results. Missing history remains an
  ordinary recovery prerequisite, not a reason to manufacture cursors.
- Preserve available native history. Do not promise an arbitrary H-only/pruned
  dataset can satisfy genesis-state, DKG freeze-state or upstream epoch recovery.

Current source owners: `bin/outbe-chain/src/launch/node.rs`,
`crates/blockchain/engine/src/stack/{follower.rs,epoch/run.rs,recovery/anchor.rs}`,
`bin/outbe-chain/src/ocomp_exex/run.rs`,
`bin/outbe-ocomp/src/{embedded_runtime.rs,discovery_spool.rs,nod_materialization.rs}`,
`crates/blockchain/offchain-storage/src/{config.rs,rocks.rs}` and native CE,
projection and retention modules. The task file maps specify actual additions.
Source reviews found these routes; real copied-store startup remains acceptance
work in tasks08–09, not an already executed result.

Snapshot creation copies donor fatal evidence unchanged. Recipient identity and existing evidence are not automatically overwritten or merged; direct file placement is an operator action, and ordinary fatal startup behavior is retained.

## Validation inputs and lifecycle

The signed inventory authenticates the exact original file set. Preserve the raw
manifest and signature outside native runtime stores after transfer if the operator
wants later file/provenance checks. Ordinary restart does not consume them. Once
the node advances, original file hashes describe the old cut, not current stores.

Current EVM verification needs a complete authoritative current state and its
header, not old changesets. Current OCOMP intent/finality relations use immutable
job fields from that verified E state plus referenced retained headers. Original
B-state reconstruction is not part of this feature. Native startup can separately
need retained frames/genesis/freeze history; tasks08/09 prove ordinary recovery.

Current CE reconstruction and projection/index integrity are independently
checkable. Full live-body/CE equality requires matching actual height and hash.
Do not claim that a normal stop aligns asynchronous store writers. An unavailable
comparison is reported as Incomplete without changing the data or shutdown.

Copy native lifecycle records as found: sparse closure, surviving discovery
records, stage-specific receipts/catalogs and partially collected retention data.
Closed offers/ACKs/tombstones and Released pins may legitimately be absent.
GcPending may have a partial or empty body residual; another live reference to
the same lease still imposes its full obligation. Canonical Completed alone is
not proof that this donor completed local computation.

Required public data is derived from canonical current obligations as well as
from present files. Task06 specifies independent queue/day scans so deleting an
entire required job cannot become an empty successful scan. Retired historical
intermediate artifacts are not permanent snapshot requirements. Exact evidence
and corrected task mapping are in [validation audit](validation-audit.md).

## Ready native files, not a reconstruction input

Every included row names an existing native file domain. Creation packages those
files unchanged; the manifest maps payload domain to ordinary configured root.
Receipt uses conventional transfer/copy or archive extraction. There is no DB
import, snapshot restore command, state/root/index rebuild or OCOMP materialization
step between file placement and ordinary start. Task03 proves direct native opens;
tasks08/09 prove process continuation. Root/tree recomputation exists only in the
optional validator and writes disposable scratch, never installed state.
