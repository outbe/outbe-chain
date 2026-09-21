# Offline validation: data sufficiency and corrections

Audit completed 2026-09-20 against Outbe `e5d545cedc872c0e579fd3535e7638d883d5b1f5`
and pinned Reth `5a6940e351fed80458fe6c9da8581cbe4b8bd036`.
This audit preceded implementation authorization on2026-09-20. It checks the
actual inputs and feasible algorithms, not just whether task files mention tests.
It does not certify a real donor dataset or claim an executable validator exists.

## Result

The copied native stores contain the inputs for signed-file integrity, retained
header continuity, complete current EVM state/code verification, current CE tree
reconstruction, projection body/index scans and current OCOMP artifact relations.
These checks can be implemented with the existing formats and planned read-only
adapters. No vendor patch, live capture or new node startup mechanism follows
from this audit.

**Corrections incorporated into the current specifications on 2026-09-20.**
Tasks01–06 and08–09 now contain the corrected paths, lifecycle rules, current-E
authority, canonical obligation algorithms and acceptance cases below. The
audit itself provides no compiled or runtime acceptance claim. The user subsequently authorized implementation of the corrected eight-task plan.

The feature remains: a stopped producer creates a necessarily signed snapshot;
any new node can place its ready native files, optionally validate them,
and use ordinary startup to continue from stored progress and catch up. The
operator chooses validation. A validation report never becomes startup authority.

## Incorporated corrections and required acceptance cases

| Finding | Exact task correction | Required regression/acceptance case |
|---|---|---|
| NOD materialization paths were flattened incorrectly | Task01/data-layout and task06 must use `O/supervisor-v1/materialization-references/<job>/<first_nod_ordinal>/<job>.materialization-refs-v1.json`; enumerate both directory levels and bind the inner job identity. | A real nested record is copied and visited; wrong inner job/ordinal is detected; a flat-only scan cannot pass. |
| Normal GC removes records that the draft treats as universal evidence | Tasks05/06 must distinguish active leases, shared leases, partial/empty GcPending residuals, Released/pruned pins, retired discovery records and incomplete local work. Check present rows and current obligations; do not require deleted history. | Valid partial GC, absent retired offer/ACK/tombstone and pruned Released pin pass their applicable checks; removing data still required by an active lease is reported. |
| Historical EVM rewind is unnecessarily coupled to ordinary OCOMP checks | Tasks04/06 should use immutable canonical JobIntent/finality fields from verified current E plus retained header B for base relations. Do not require historical B-state reconstruction for these checks. Arbitrary historical-state proof is not needed for the agreed scenario. | Current bindings remain verifiable after historical changeset pruning; a modified immutable field/header binding is rejected. |
| Enumerating only present files/pins misses an entirely absent required job | Task06 must define independent discovery of current canonical obligations and derive required public data from them. Specify pending NOD queue scope and unpaid certified day enumeration; then compare required and present sets. Current task06 specifies live aggregate/scheduler, all NOD FIFO entries and the permanent Intex series index with full payout bitmaps. | Remove a whole required job directory together with its local references: selected continuation-data validation reports missing input, not success. A retired/nonrequired historical job is not demanded. |
| Stopped CE and projection are not proven to have the same frontier | Keep task05's Q=P height **and hash** condition in its result/DoD. When they differ, structural checks remain available but full live-body/CE equality is incomplete. Do not fabricate a common cursor or modify shutdown. | A stable Q!=P dataset gets structural results and an explicit unavailable cross-store comparison; matching views detect missing/extra/mutated leaves/bodies. |
| Historical partition root must come from the right canonical commitment | Tasks05/06 must use canonical JobIntent collection root/count/WWD with lease/finality binding. CandidatePin.ce_sealed_root alone is the wrong root for a collection. Payouts use current certified generation and canonical active job directly. | Wrong partition with otherwise valid body hashes fails; a certified payout file remains checkable without already retired intermediate Lysis data. |
| Current Reth state and CE genesis need explicit boundary cases | Task04 must inspect Finish and partial-state/unwind metadata before accepting a complete current-root view. Task05 needs a genesis branch because the reused point-proof header helper rejects height zero. | Masked partial-state fixture is not called a complete root; ordinary complete head passes; native empty CE genesis is handled explicitly. |
| Some stated checks imply more than their inputs prove | Workflow/task04 must say header continuity, not verification of every transaction/receipt or independent finality. Task05 body commitments do not authenticate event provenance metadata. Task06 catalog consistency is not reexecution of Lysis. | Report names the exact checked scope, missing inputs and omitted checks; no all-data-valid conclusion from only a subset. |

These are incorporated planning corrections and test specifications, not
implemented tests. The canonical obligation algorithms and their exact existing
APIs/new owner reader are specified in tasks04/06 and summarized below.

## What a result means

- **Passed:** the selected comparison ran over its declared complete input set and matched its expected commitment or relation.
- **Failed:** available bytes contradict a checked commitment, identity or structural invariant. Signed inventory detects missing/changed files relative to that inventory.
- **Incomplete:** an essential selected input/view is unavailable, so that comparison cannot establish its promised result; for example different CE/projection frontiers, missing required frame or partial execution state. This is not automatically proof of corrupt canonical state.
- **Not requested:** the operator did not select this check. It cannot contribute to a general success claim for all checks.

Failed or incomplete selected checks return nonzero as already planned. File placement
and ordinary startup do not consume this status. A producer signature identifies
and authenticates the signing key's statement; the signature alone does not prove
honesty, active validator membership or computation correctness.

The detailed evidence below gives each check's purpose, exact stored bytes,
writer/reader, expected commitment, snapshot domain and deletion limitations.
Native stores use the abbreviations in [data-layout](data-layout.md). CE identity
is called Q below (M in that layout); E is actual complete execution state, B a
referenced historical block, H the finalized block and C OCOMP closure.

## Signature, files, chain and execution state

RETH below means the pinned local checkout
`/home/ubuntu/.cargo/git/checkouts/reth-e231042ee7db3fb7/5a6940e`.

## S01 Mandatory producer signature

Purpose: identify the signing public key and bind the exact manifest bytes.
Inputs: raw manifest bytes; format/domain identifier; signature; compressed public
key. Expected signer, if independently selected by the operator, is an additional
trust input, not something inferred from a label in the same snapshot.

Task02 explicitly puts the manifest and mandatory signature envelope in the
artifact. No private key is needed by the recipient. Existing
`crates/blockchain/primitives/src/signer.rs:193` signs a32-byte prehash and emits
64-byte ECDSA plus recovery byte; `tee_signatures.rs:17` recovers the33-byte public
key. Manifest digest/domain, strict envelope and low-S format check are NEW tool
code already assigned to02. Sufficient artifact inputs can be produced without
node/enclave startup. Signature verifies the key's attestation; by itself it does
not independently prove active ValidatorSet membership or honest computation.

## S02 File inventory and integrity

Purpose: reject incomplete/altered bytes relative to the signed inventory.
Inputs: manifest logical-domain/path/length/SHA256 entries, actual local files,
mandatory signature envelope. The manifest is NEW output of creation; it cannot
be reconstructed authoritatively from a received unsigned directory after damage.

Every digest input is a regular file from the included native data domains.
Receiver can stream length/hash checks and verify exact declared membership.
No history/keys/network required. Comparing a re-created manifest against the
same tampered files proves nothing about the original; use the signed original.
The original manifest/signature remain portable sidecars outside native runtime
paths for later operator-selected file/provenance checks. Native-only state validation may omit this check
and must label original-file/provenance checking NotRequested/unavailable.

## S03 Retained header chain

Inputs: canonical headers/hash records from MDBX and corresponding retained
static-file segments, segment metadata/indexes, chain identity and reported H.
Included by full D/db and static-files domains. Expected relations are header
number, its native hash, parent hash and canonical mapping at the same height.

RETH storage/db-api/src/tables/mod.rs:311–335 defines CanonicalHeaders,
HeaderNumbers and Headers. Static header reads are concrete:
storage/provider/src/providers/static_file/manager.rs:2604 and jar.rs:157.
MissingStaticFileBlock becomes None and headers_range skips missing rows, so the
NEW verifier must check expected interval coverage rather than trust list length.
Exact H key is ChainStateKey::LastFinalizedBlock (tables/mod.rs:591–611); do not
seek the next ChainState entry and mistake safe height for finalized height.

Feasible for retained contiguous ranges; missing interior data is a failure.
This verifies HEADER continuity, not EVM execution or an independent finality
proof. It does not rederive every transaction/receipt body commitment. If any
operator wording says all block contents are semantically verified, narrow that
wording or explicitly add such a separate check; current task04 is header-only.
Pruned ranges outside the advertised retained set are reported as unavailable.

## S04 Complete current EVM state root and S05 bytecodes

Inputs: complete authoritative current account/storage tables, storage_settings,
stage metadata, corresponding header/state_root and Bytecodes. All are in copied
D/db and header static files. No prior changesets are required to verify a complete
current state.

RETH storage/db-api/src/models/metadata.rs:16–25 and85–90 selects v1 plain versus
v2 hashed authority. Tables are defined in tables/mod.rs:397–415,481–507.
LatestStateProviderRef uses the same branch at
storage/provider/src/providers/state/latest.rs:69–82. Writer
providers/database/provider.rs:2766–2828 writes plain state only for v1;
write_hashed_state at2831 is the v2 state writer. Bytecodes are separately written
at2583–2586/2823–2826 and must be verified separately: account root only commits
the code hash, not proof that the referenced bytes are present.

Task04's scratch normalization is feasible: v1 hashes address/slot into fresh
hashed tables; v2 copies hashed keys directly. DatabaseStateRoot::from_tx exists
at trie/db/src/state.rs:22–25,152–156, creating native trie/hashed cursors. Empty
scratch trie tables plus all authoritative rows permit independent computation.
Compare with header of the actual complete state; require each referenced code
record and hash original bytes. Do not trust a copied trie root as the result.

MetadataProvider::storage_settings (storage/storage-api/src/metadata.rs:24–28)
returns None on malformed JSON; strict audit must read/parse the raw value so
corruption is not silently classified as legacyv1. This is already in04.

### Partial persistence is a separate, concrete boundary

Finish is not always proof of complete raw hashed state at that height.
providers/database/provider.rs:575–605 reads Finish and partial_state_trie,
775–849 masks state/trie updates covered by a newer suffix and records both
frontiers. Existing source test at4796–4815 has header2, partial frontier1 and
absent masked account/slot rows. Neither choosing the latest header nor simply
renaming the raw table height to the partial frontier fixes that mixed state.

Normal graceful head persistence explicitly chooses both frontiers equal to the
canonical head (engine/tree/src/tree/mod.rs:2249–2264). Ordinary launch detects
partial state/unwind metadata and runs its existing unwind path
(node/builder/src/launch/common.rs:550–605,1399–1444).

For the offline audit, read these markers and require a complete state before
reporting current-root success. Task04 already says recognize incomplete/unwind,
but must name these exact inputs and test the masked-row case. A copied partial
image is a named incomplete/unsupported root check under the present algorithm;
calling ordinary recovery on source or modifying shutdown is not authorized.
No claim that every interrupted/partial copy can pass full root validation.

## Historical EVM state is outside this feature

The initial draft unnecessarily required rebuilding old EVM views. Native pruning
can remove before-images: pinned Reth `prune/prune/src/segments/user/account_history.rs:68`
and `storage_history.rs:69`; missing static changeset offsets can also read as
empty (`storage/provider/src/providers/static_file/manager.rs:2357`). Copying the
remaining DB cannot reconstruct erased history.

Current immutable job/finality bindings and retained header identities suffice
for base OCOMP relations. Tasks04/06 therefore remove historical rewind APIs,
descending scratch scheduling and their tests entirely. A missing required header
or replay frame is still a named missing input. Historical EVM proof tooling is
not an optional extra silently retained in this plan.

## CE trees, bodies and indexes

## Inputs, writers, reads and commitments

All paths below are repository-relative current-source anchors. `CE` means crates/core/compressed-entities/src; `T` means crates/core/tribute/src; `N` means crates/core/nod/src; `OD` means crates/system/offchain-data/src/lib.rs; `OS` means crates/blockchain/offchain-storage/src/rocks.rs. `node/src/...` abbreviates crates/blockchain/node/src/.... These abbreviations name paths, not new components.

| Check and status | Exact persisted input / writer | Read / recompute / expected authority | Inclusion and lifetime |
|---|---|---|---|
| CE identity and actual marker: **feasible-with-planned-adapter** | D/compressed_entities/smt. CeMetadata = OutbeCompressedEntitiesMetadataV3, keys environment_identity and last_applied (CE/persistence/tables.rs:7; mod.rs:40). Initialization writes identity and genesis marker (environment.rs:370); each finalized batch overwrites last_applied in the same transaction as trees (environment.rs:327). | CeMdbxReadOnly::open verifies expected environment identity and existing marker/catalog wrapping; marker() reads it (readonly.rs:33,58). audit_exact must enumerate metadata, decode native identity/marker and compare configured chain/genesis/scheme. Source metadata alone is not an independent chain commitment. | data-layout.md:34 includes the complete CE store. Marker is current state, not a historical index. |
| Every leaf, branch and native root: **feasible-with-planned-adapter** | CeLeaves / CeBranches / CeTreeRoots are OutbeCompressedEntitiesLeavesV3 / BranchesV3 / TreeRootsV3 (tables.rs:24,41,58). Keys carry TreeNamespace plus native tree/branch key; root keys are namespaces. apply_raw_tree_changes writes/deletes leaves and branches (environment.rs:49); apply_finalized writes all shard roots (247). | Native namespace/field/branch codecs are internal (codec.rs:33,71,84). Current MdbxSnapshot gives point reads and counts (snapshot.rs:21); the DB owner can use RO table cursors, as collections.rs:252 already does. Planned audit must scan ALL three tables, including unreferenced namespaces, rebuild from leaves with PoseidonSmt on an empty scratch store, and compare branch/root inventories in both directions. Native generic store/update hooks exist (smt/facade.rs:319,377); a scratch store and complete comparison do not yet exist. Expected branch set comes from reconstruction, not from trusting stored roots. | data-layout.md:34 copies all required current bytes. Bound source transaction and scratch/page/run sizes. Branch/root corruption is detectable even with unchanged marker; a leaf/root/header jointly changed requires independent task04 header authority. |
| Collection/catalog representation and sealed root: **feasible-with-planned-adapter** | Every materialized collection gets all K=16 explicit root rows, including zero roots (environment.rs:247). Catalog root row is explicit; initialization writes ZERO (374). Collection retirement deletes roots, branches and leaves (collections.rs:245-280), and the staged catalog changes update/remove catalog leaves (environment.rs:304). | read_collection_roots accepts either no records or exactly K (collections.rs:61-97). Rebuild shard trees, aggregate roots, native collection_root, catalog tree and sealed_root; compare catalog membership in both directions and final marker. collection_root uses active scheme, collection key, shard count and shard-top root (collection.rs:183). All domains currently use K=16 (collection.rs:12-30). | Same CE domain. Absent/retired has no collection records/catalog leaf; materialized zero-body collection can retain all K zero shard roots and nonzero catalog commitment. Missing zero root rows are not valid sparse representation. |
| Invert collection identity/WWD from arbitrary empty catalog key: **impossible-claim** | Persisted namespace stores hashed CollectionKey, not its domain/day preimage (codec.rs:33-43). | Body IDs let the audit derive domain/day keys for nonempty populations. Zero-body materialized collections may have no such body. Structural aggregation is still possible with the native active scheme and common K; do not assert an invented WWD/domain preimage. Task05 step6 already warns against inversion. | Copying more of the same CE tables does not supply the preimage. Keep opaque authenticated collection identity for this case. |
| Bind current CE root to exact header Q: **feasible-with-planned-adapter**, or **missing-input** when Q header unavailable | marker.height/block_hash/scheme/new_root plus exact canonical header bytes from task04. Proof code compares marker/header identity and scheme/root (proof.rs:524); header artifact decode selects tag0x08 (542). | Rebuilt root -> marker -> canonical header artifact -> task04's header authority. Do not replace missing header with marker self-comparison. Existing point-proof header helper rejects height0 (546), so it is not an unconditional genesis audit helper. | CE included at layout:34; available Reth headers/static files at layout:31-32. “Available history” is not proof every requested historical header exists. For Q=0 use native empty genesis identity/root rules explicitly, or name the unsupported case; task05's valid-genesis test needs this distinction. |
| Projection identity/checkpoint P: **feasible-with-planned-adapter** | RocksDB namespace projection_state, singleton offchain_data, schema2, config chain_id/genesis_hash/start_block and optional checkpoint (OD:48-94). Body mutations and state update share one atomic batch (OD:628-654). Actual durable sink acknowledgement occurs only after separate backend write (node/src/projection/sink.rs:343-352). | read_projection_state(config, reader) verifies state and rejects metadata on singleton (OD:289). Use RocksDbReader::open(primary, external secondary); no source initialization, catches up once, checks format (OS:74-115). Do not use writable OffchainDataProjection::open as an inspector. | Whole P copied at layout:35, including format/WAL/native files. Missing/uninitialized checkpoint is an explicit observation, not permission to invent P=Q or P=0. |
| Every live Tribute body: **feasible-with-planned-adapter** at Q=P | tributes key WwdEntityId; value StoredBody envelope with canonical payload and optional provenance metadata. Native encode/decode/key at T/repository.rs:461-496. Projection puts/replaces body at T/projection.rs:206-215; delete at253. | Planned scan_stored_bodies must scan primary namespace with empty prefix, not owner/day index. canonical_body_leaf decodes envelope/domain body, matches exact key/body ID, hashes active scheme+schema+ID+payload (CE/proof.rs:561-590; commitment.rs:100). Derive native collection/shard/tree key and merge all tuples against validated CE shard leaves in both directions. | Whole P layout:35. Ordinary delete destroys live body. Retirement may copy to retained first; no general historical Tribute body archive. |
| Every live Nod item AND bucket: **feasible-with-planned-adapter** at Q=P | nods and nod_buckets, both keyed by WwdEntityId bytes (N/repository.rs:491-510). Separate typed item/bucket decoders (443,475). Item put/delete at N/projection.rs:229,255; bucket put/delete at290,297. | Add independent exhaustive item and bucket scans. canonical_body_leaf uses NodItem or NodBucket decoder and exact entity ID (CE/proof.rs:572-587), then native key/shard derivation and the same bidirectional CE merge. Existing list_all/items cannot establish bucket coverage. | Whole P layout:35. Current versions are overwritten/deleted. No historical item/bucket versions are produced by these primary mutation writers. |
| Every live secondary index: **feasible-with-planned-adapter** | tributes_by_owner = owner address + ID; tributes_by_day = big-endian day + ID; values empty (T/repository.rs:499-516; projection.rs:217-255). nods_by_owner = owner + ID, empty value (N/repository.rs:499; projection.rs:234-258). There is no bucket index in the bucket writer (N/projection.rs:264-303). | Scan each full namespace and reconstruct expected indexes from decoded primary bodies. Compare both directions, validate key/value/metadata shapes with owner rules, reject duplicate/dangling/wrong-owner/day rows. Raw bounded scan exists: OS:188-235, empty prefix, exclusive after, native key ordering, row and byte bounds. New owner audit APIs are needed; bytes are not missing. | Whole P layout:35. Atomic mutation batches maintain both sides; a damaged index must not hide a primary body from the audit. Equality to primary-derived indexes is structurally meaningful even when Q!=P; chain authentication of those bodies still needs the matching CE view. |
| Authenticate event provenance metadata through body leaf: **impossible-claim** | Primary StoredValue may include block/tx/log/emitter metadata (OD:58-65,474). | CE body_commitment includes scheme/schema/identity/payload, not this metadata (commitment.rs:123-130). Codec/shape checks remain possible. Authenticating the source event would require a separate receipt/header comparison and is outside task05's stated body-equality claim. | Metadata is copied with P; its presence does not mean its history is authenticated by the body root. |
| Full live equality when Q!=P, or recreate arbitrary old CE/body view by height: **missing-input** / **impossible-claim** if promised unconditionally | CE current rows/marker and projection current bodies/checkpoint only. CE open_exact opens the current snapshot then checks required identity (readonly.rs:67-79). Updates overwrite/delete (environment.rs:49-79,327); retirement deletes collection tables (collections.rs:245). | Still run independent structural CE and projection/index checks. Report full live equality as incomplete for mismatched height/hash. No cursor rewrite, historical DB open or scratch API name restores erased body/leaf bytes. Optional receipts/history reconstruction is separate work, not an existing task05 capability. | Whole native copy preserves current frontiers, not all historical state. This is already correctly limited by task05 step9; ensure final status/DoD cannot flatten it into complete success. |
| Every retained body and retained-day index: **feasible-with-planned-adapter** | ocomp_retained_tributes and ocomp_retained_tributes_by_day. Same key: input_lease_id + day BE + WwdEntityId + commitment (T/retention.rs:409-435); body value plain StoredBody, index value empty (149-158). plan_retain_stored checks uniqueness/conflicts and native hash (96-160). | Add exhaustive visitor over BOTH namespaces; decode lease/day/ID/commitment and validate stored body hash, metadata absence and index bijection (parse_retained_entry:438-476). Existing list_by_day is index-selected; get_current_or_retained requires already-authenticated expected leaf (167-238), so neither alone proves whole-primary absence/orphans. Key commitment is self-consistency until task06 binds a partition authority. | Whole P layout:35 explicitly includes both namespaces, retention journal at layout:40. Projection retirement retains only when active_pin_for returns a pin; otherwise deletes (OD:492-524). retain_then_delete returns one combined batch (T/projection.rs:143-162), committed with checkpoint by OD:654. |
| Complete selected historical Tribute partition: **feasible-with-planned-adapter** while required bodies/authority remain; otherwise **missing-input** | Eligible live bodies plus exact lease/day retained bodies; canonical JobIntent fields sealed_tribute_collection_key/root, authenticated_day_count and wwd; retained pin and finalized request binding. Export/CAS can separately supply authenticated published input data, per task06. | BoundedTributePartitionVerifier create/push/finish reconstructs root and exact count from ID/commitment tuples (CE/collection_reconstruction.rs:75,126,180-195). It does NOT authenticate its expectation. Native comparisons already bind manifest root/count to canonical JobIntent (bin/outbe-ocomp/src/export_binding.rs:487-500); canonical_finalized_pin checks intent/lease/day/CE root/bundle/request hash/state root (node/src/ocomp/retention/source.rs:389-429). Task06 must supply this authority and select/deduplicate the live-or-retained body population before calling the verifier. | Inputs span whole P + journal + canonical execution state + referenced public artifacts (layout:31-35,40,43-45). Current verified E can supply immutable JobIntent/finality bindings; a mandatory historical B-state rewind is unnecessary for these relations. No historical CE tree is needed merely to compare against this authenticated partition root. |
| Require complete retained partition in GcPending/Released: **impossible-claim** | T/retention.rs:310-342 selects matched body/index pages for deletion; writer commits one page atomically at363-369. Coordinator persists GcPending before deletion, returns PageProgress while incomplete (node/src/ocomp/retention/coordinator.rs:1141,1190-1197), publishes Released only after final page (1221). | For GcPending, surviving body/index rows can be audited structurally and bound to their lease, but absent deleted rows are legal. It may even be empty after final deletion before Released journal publication. Apply complete partition obligations only in lifecycle states that still require preservation, per task06. Released does not require bodies. | Stopping freezes whatever valid lifecycle point exists. Whole P plus journal faithfully preserves partial GC; it cannot resurrect removed rows. |

The immutability basis for the current-E JobIntent route is explicit: crates/core/metadosis/src/ocomp/transitions.rs:132-161 rejects an existing intent record before creation; finality rebinding conflicts are rejected at244-251; completion changes quorum/status/terminal fields at600-612 while retaining the intent. Canonical state verification and complete job discovery remain task04/task06 responsibilities.

## Why stopped writers do not prove Q=P

The two persisted frontiers have separate writers and transactions. The finalized CE owner applies its tree batch in crates/blockchain/engine/src/ce_finalizer.rs:507-553. The unified OCOMP loop independently projects finalized frames through spawn_blocking in bin/outbe-chain/src/ocomp_exex/run.rs:470-479; the sink publishes durable P only after its backend batch write in crates/blockchain/node/src/projection/sink.rs:343-352. Retention readiness or retry can delay a projection frame (run.rs:462-466).

Reviewed ordinary stop code drains Radicle/follower/consensus and observes engine teardown (bin/outbe-chain/src/launch/node.rs:171-174,211-235,1302-1420; crates/blockchain/node/src/shutdown.rs:115-157). These inspected paths do not compare actual CE marker and persisted projection checkpoint or establish a Q=P barrier. This is bounded source evidence, not a claim to have verified every dependency shutdown path. The plan's stop precondition therefore establishes stable reads, not a demonstrated cross-store equality guarantee. Preserve both identities and report incomplete live equality when they differ. No capture service, shutdown modification or runtime alignment is proposed.

## OCOMP data and continuation obligations

## Canonical authority available at E

`crates/core/metadosis/src/ocomp/request.rs:317` constructs JobIntent with CE sealed root, sealed Tribute collection key/root, authenticated count and nominal total (325–329). `ocomp/transitions.rs:132` rejects an existing IntentId; 154–161 stores the intent and intent height. Finalization derives JobId from intent and finalized request block hash/state root (223–239), rejects changed finality binding (244–248), and stores it (250–251). Completion adds quorum, status and terminal binding (600–612); it does not replace the intent. The terminal binding contains result digest and embedded terminal receipt (574–583).

`ocomp/store.rs:43` reads the canonical job by intent storage key, bounded-decodes it, and recomputes its IntentId (51–68). `retention/source.rs:403` already expresses exactly the needed comparison: intent ID, input lease, WWD, CE sealed root, bundle, request height and finalized request hash/state root, then JobId/open/deadline/finality height (403–429). `export_binding.rs:471` compares manifest job/bundle/attempt/WWD, collection key/root, count/nominal and finalized block/state/CE identities (471–513). These comparisons can consume the job at E. The existing production historical reader `retention/source.rs:164` is an acquisition choice; using it unchanged is not proof that B-state is necessary for those comparisons.

Day retirement is not synonymous with deleting this authority: `crates/core/metadosis/src/state.rs:224` deletes the day's terminal index, day receipt and day metadata; completion separately persists the canonical job (`ocomp/transitions.rs:612`). The public terminal-receipt getter reads the receipt from the job's completed binding (`ocomp/views.rs:56`). This finding is bounded to these examined lifecycle paths, not an exhaustive claim that all canonical historical records are retained forever.

Classification: **feasible-with-planned-adapter**, conditional on task04's verified current E view and required retained header. If a required canonical job/header is actually unavailable, report that specific **missing-input**; do not silently substitute directory labels. A historical assertion that the entire original EVM/CE view at B is reproducible remains a separate data-dependent check.

## Relation-by-relation byte and lifecycle map

| Relation / exact stored bytes | Writer → reader | Expected authority and layout inclusion | Lifecycle effects and classification / minimal correction |
|---|---|---|---|
| Closure C: `O/exporter-v1/discovery/closure-checkpoint-v1/checkpoint.v1`; native magic/version, baseline/previous/current number+hash and checksum | `bin/outbe-ocomp/src/discovery_spool.rs:912` sparse compare-and-advance persists the state at 933; codec 1082/1094. Task01 planned checkpoint inspector must reuse decoder, without `open` recovery at 831. `bin/outbe-chain/src/ocomp_exex/discovery.rs:504` determines actual closure target. | Native checkpoint rules plus canonical retained headers; copied by data-layout closure row. Native advancement permits sparse jumps (909–920), so previous is not necessarily C−1. | **feasible-with-planned-adapter** for recorded checkpoints. Missing selected header is **missing-input**. Requiring contiguous predecessor or complete historical job witnesses behind C is **invalid-claim**. C is a local checkpoint, not a permanent archive of processing proof. |
| Discovery offers, ACKs, pending/quarantine/retirement records under per-bundle discovery spool | Owner record transitions/retirement preparation in `discovery_spool.rs:356`; closure driver `ocomp_exex/discovery.rs:535`; planned reader shares native record/filename codecs. | Surviving record identity, current canonical job, export and generation relations; explicitly included closure/discovery subtree. | `discovery_spool.rs:479` removes ACK, pending, offer **and retirement record** through 490. `ocomp_exex/discovery.rs:575` then drops in-memory closed-request maps; `embedded.rs:310` prunes only process-local job state. **feasible-with-planned-adapter** for surviving records; demand for an old ACK/offer for every retained export/pin is **invalid-claim**. If native load_exact needs a deleted DiscoveryRecord, factor its canonical comparisons or reconstruct the required typed inputs from verified canonical job plus surviving native binding; do not call an absent spool record mandatory historical authority. |
| Pin journal `S/ocomp_retention/pin.v1`: registry generation, timestamp, keyed PinRecord generation and state-specific candidate/job/export fields | `crates/blockchain/node/src/ocomp/retention/journal/mod.rs:148` atomic persist; `retention/inspection.rs:5` nonmutating native decoder; schema `retention/types.rs:5`, 39, 54, 105, 114. | Candidate block/hash/state, intent, WWD, bundle, CE root and lease bind to canonical job at E; ExportAuthority carries source/lease generation and manifest hash. Included retention root. | Terminal→GcPending→Released in `coordinator.rs:1090`; partial page deletion at 1190–1199; Released install 1222. New-key pressure removes old Released entries at 1354–1363. **feasible-with-planned-adapter** for present records and their actual state; requiring a complete historical pin population is **invalid-claim**. Source/lease generations are native local authority; the contract does not independently certify the donor's entire journal history. |
| Retained Tribute bodies and indexes associated with input lease; current/retained union as native exporter input | Storage writer/index details belong to task05/valo. OCOMP consumer is `bin/outbe-ocomp/src/exporter.rs:60`: current pager + retained pager, authenticated partition, exact count and nominal; reconstruction entry at 89. Release owner `retention/coordinator.rs:1190`. | Exact partition key/root, count and nominal come from **canonical JobIntent at E**, not solely CandidatePin.ce_sealed_root: `request.rs:325–329`; `export_binding.rs:487–500`. Included task05 native Tribute partitions plus retention. | A GcPending lease may have only a suffix/subset left after a page. A shared lease can still serve another live reference (`coordinator.rs:1107` branch / `lease_has_other_references` at 1455); evaluate obligations per lease, not independently per old pin. **feasible-with-planned-adapter** for a required complete live union. For GcPending validate surviving body/index consistency, not an already released full root. Demanding complete released/partly-GC historical bodies is **invalid-claim**. |
| Export receipt/preparation and binding locators: `O/exporter-v1/receipts/<job>/{prepared.ref,receipt.ref,...}`, `O/supervisor-v1/export-bindings/<job>/binding.ref`, plus their referenced CAS objects | Receipt prepare `export_receipt.rs:391`, atomic persist 1015; receipt reader 258–287 requires prepared+receipt and canonical manifest. Binding seal `export_binding.rs:180`, load_exact 259, atomic persist 744. | Canonical current job/finality, exact source+lease generations and manifest hash, pinned bundle, exact input catalog. Runtime roots `embedded_runtime.rs:132–158`; included public-record rows. | A partial prepare is a native stage, not a completed export. Validate presence according to stage. Existing binding load_exact needs DiscoveryRecord; closed spool deletion means unconditional reuse needs the adapter qualification above. **feasible-with-planned-adapter** with surviving or canonically reconstructed authority. Missing required referenced CAS/catalog is **missing-input**; do not label all historical missing spool records corruption. |
| Input refs: `O/exporter-v1/input-refs/<job>/catalog.header`, prepared/staging/abstained, required `catalog.lock`, numbered `.input-ref` entries and referenced manifest/chunks in CAS | `input_ref_catalog.rs:246` prepare, 493 admit, 1366 atomic persistence; `reopen` 430/458 and exact verified cursor. Native file names 33–39. | Manifest authority and declared exact ordinals/counts, bundle kind/limits, canonical intent closure via binding. Whole root included; preserve existing lock placeholder. | **feasible-with-planned-adapter**. Current input-ref read path has shared read-only existing lock. Missing middle reference/chunk is detectable; missing entire job needs reverse obligation discovery. Partial stage must not be reported as complete export. No need to recover original B-state merely to verify manifest/count/root bindings. |
| Plan/admissions and result chunks: `O/supervisor-v1/jobs/<job>/admissions/{catalog.header,catalog.lock,*.admission,...}` and CAS plan/unit/reducer/result objects | `admission_catalog.rs:107` open, 178 admit, 871 atomic persist. Planned AdmissionCatalogReader bridge feeds `LocalLysisPlanAuditV1`; `lysis_result_catalog.rs:3` discovers exact result chunks via admitted plan-derived ROOT_REDUCE leaves. | Pinned manifest/plan, topology, exact job/bundle, declared result catalogs; included job public records and CAS objects. | **feasible-with-planned-adapter** for present stage-complete records; pending computation may have partial admissions. `lysis_plan_audit.rs:4` explicitly does not validate phase payload semantics or finalized authority; `lysis_result_catalog.rs:5` Complete is evidence, not a finalizer. Reporting computation correctness/reexecution from cursors alone is **invalid-claim**. Require catalog Complete only for an obligation requiring completed outputs. |
| CAS objects `O/cas-v1/objects` addressed by transport digest; referenced kind and byte count in native refs | `cas.rs:135` publish; reader open 340 and read_verified 353; bounded file identity, length, Keccak transport digest, OCB1 kind checks 369–428. | Full native reference from an authenticated job-bound manifest/catalog; a valid digest alone does not prove same-job membership. Complete committed objects included. | **feasible-with-planned-adapter**. Verify references and membership, not merely every object individually. This audit does not assert a repository-wide absence of CAS GC. The necessary criterion is current obligation closure over the actually copied referenced objects. |
| Local result `O/node-v1/local-results/<job>.lysis-result-v1.ocb1` | `local_result.rs:72` canonical immutable commit, 100 load, 373 path. `embedded_runtime.rs:443` commits locally completed result. Planned reader must avoid mutating startup open/reconciliation at local_result.rs:48. | Canonical job's completed result digest (`transitions.rs:577`), native JobId/encoding; included local-results subtree. | **feasible-with-planned-adapter** when present. Canonical Completed does not guarantee this file: discovery.rs:436–458 observes completion, then only FullNode branch restores/starts compute. Missing restore returns without fabricating a result (`compute.rs:148`). Result absence for unfinished local work is not corruption; missing result/outputs needed by a selected continuation capability is **missing-input**. Do not demand every historical completed job's local result. |
| Public NOD reference JSON, version/job/dependencies, **actual** `O/supervisor-v1/materialization-references/<job>/<first_nod_ordinal>/<job>.materialization-refs-v1.json` | Runtime constructs nested root `embedded_runtime.rs:781–784`; owner appends filename `nod_materialization.rs:412–417`; pin_exact 340, load_exact 370, release 403. Runtime builds batch from audit 818 and pins 824. | Job-bound plan/reducer/result membership; current NOD head where checking a current batch. Builder checks job/program semantics/WWD/count/cursor at `nod_materialization.rs:58–62`. Protected signed journal is excluded; public ref is included in principle but current layout specifies wrong flat path. | Flat task06 clause6/data-layout row is **invalid-claim** and would miss actual files. Change inventory and reader to bounded job→ordinal→file traversal, preserving both identities. On finalized submission runtime releases refs (`embedded_runtime.rs:842–851`); startup reconciliation can release surviving refs using excluded signed journals (`nod_materialization_submitter.rs:354–416`). A leftover ref can be legitimate after cursor advancement; absent ref is not proof that pending NOD lacks reconstructible outputs. Current head + public plan/output closure suffices to build public batch evidence without importing signing authority. |
| Payout file `O/supervisor-v1/jobs/<canonical-job>/contributor-payout-v1.bin`: concatenated 84-byte owner/TributeID/nominal leaves | `payout_artifact.rs:74` writes leaves, 85–102 finalizes atomic file; 108–132 consumes exact result catalog. `supervisor_job.rs:308–332` finalizes verified local result and writes payout file before returning local completion. Reader comparison already exists `payout_submitter.rs:935–994`. | Current Metadosis active generation identifies job (`payout_submitter.rs:997–1007`; `metadosis/ocomp/store.rs:108`). Current Intex certified generation supplies count/root/nominal (`intex/api.rs:436`); payout round 448 supplies current obligation. Public file explicitly included. | **feasible-with-planned-adapter** without original B state, old CE bodies, all admissions, or old transaction journal when the certified artifact is present. Reader checks length=count×84, checked nominal sum and native root (960–987). Missing file is deliberately skipped by production (946–952): a healthy donor is not thereby a payout-capable donor. For an unpaid required day, report **missing-input**; for a fully paid/nonrequired historical day, do not require it. |

## Current continuation versus historical proofs

1. **Current job bindings, input/lease/manifest relations:** current root-verified canonical job at E contains the authority. Retained B header binds hash/state identity. Do not rewind to B by default.
2. **Current pending NOD:** independently obtain canonical pending work and selected head/generation, then verify that public input/plan/output CAS closure needed by the native builder exists. A public reference declaration is an optional surviving pin, not the source of the obligation. Existing JSON alone cannot prove the exact submitted transaction; that is neither needed nor authorized here.
3. **Current unpaid certified payout:** independently obtain candidate canonical days/rounds; for nonzero unpaid work derive current canonical job and certified root/count/total and validate the corresponding 84-byte leaf file. This is sufficient public data for the artifact relation. It is not necessary to re-prove historical Lysis computation from retained Tribute bodies.
4. **Resuming discovery or exact FullNode local comparison:** the ordinary path can require retained frames in addition to state E. `compute.rs:204–219` obtains the quorum block/frame and extracts the canonical result vote; run.rs:377–443 reads finalized frames from C+1. Root's native history audit owns whether those selected ranges are present. A digest-only local artifact check is narrower than proving ordinary replay can read every needed frame. Name that missing frame input if testing continuation; do not claim that validating state E proves replay availability.
5. **Historical evidence:** no forensic EVM reconstruction is implemented. A complete currently required retained partition uses its immutable canonical JobIntent commitment. Deleted nonrequired historical intermediates are not demanded.

## Independent canonical obligation inventory — specified correction

Tasks04/06 now inventory obligations from one immutable root-verified current E
view **before** scanning local files. A whole missing job can no longer disappear
from consideration merely because no local record was found.

1. **Live computations:** new owner `read_live_ocomp_jobs` in Metadosis views,
   re-exported through `outbe_metadosis::api`, preflights native set/deque and index/FSM lengths before allocation, rejects
   foreign scheduler days before following their FSMs, then calls
   `ValidatedWwdAggregate::load_and_validate` and existing private
   `live_ocomp_fsm_states`. It validates exact active OffchainPending membership,
   scheduler/record consistency and native capacity, then returns typed
   `(intent_id, OcompJobRecordV1)` observations. Task04 owns the reader, re-export,
   owner tests and current-E adapter; task06 evaluates local stage/source/export
   availability. AwaitingFinality/VotingOpen do not require completed outputs.
2. **All pending NOD:** read public typed head/tail/queue fields, validate
   `1 <= head <= tail`, visit every occupied sequence and next-free tail, and
   bind each unique day/job to its certified projection. Head-only is insufficient.
   For each pending job reopen its public input/plan/output closure and run the
   existing pure materialization builder through **all remaining batches** using
   a local projection copy. Later FIFO entries are future input evidence, not
   currently submittable transactions. No canonical cursor/ref/transaction writes.
3. **Open unpaid payouts:** freeze native `total_series`, visit every
   `series_id_at` and `read_series` entry, validate encoding/identity/padding,
   reject holes/duplicate IDs and deduplicate days in scratch. Read each day's
   certified generation and payout round. No round means no presently open payout
   obligation. For open rounds read all paid bitmap words, reject out-of-range
   tail bits and require popcount equals paid_leaf_count. Any remaining leaf
   requires the canonical active job's complete payout file, independently of
   local directories. Fully paid retained rounds require no file. No date cutoff.

Finite bounds are the native validated active population, tail-head, series
count, bitmap word count and remaining NOD ordinal count. Memory is bounded by
pages/scratch; an interrupted traversal reports exact coverage and Incomplete.
No completion is inferred from a resource cap or the first valid batch.

The payout index completeness claim follows ordinary production transitions:
series creation/indexing precedes arming proceeds, which precedes round opening.
It does not claim to find arbitrary hidden map entries manually injected into
genesis/storage or armed through the test-only bypass without a series. Known
injected fixtures must identify that unsupported scope; normal E2E uses the real
issuance/proceeds path. No new consensus index or all-key brute-force scan is added.

Source anchors: Metadosis `ocomp/store.rs:156`, `aggregate.rs:224`,
`aggregate.rs:421`; NOD `schema.rs:288`, `schema.rs:484`, `schema.rs:610`,
NodFactory `certified.rs:104`, `materialization.rs:222`; Intex `api.rs:568`,
`state.rs:34`, `schema.rs:315`; IntexFactory `runtime.rs:77`, `runtime.rs:123`,
`runtime.rs:367`, `runtime.rs:402`. Paths are under `crates/core/`.
Materialization pure builder: `bin/outbe-ocomp/src/nod_materialization.rs:53`.
Test-only exception: `crates/core/intexfactory/src/precompile.rs:191`.

The existing `ocomp` module is private; the new owner reader needs the explicitly
planned public api.rs re-export and an external compile consumer. Native codec/index
length constants get sibling-only visibility for this reader; values, formats and
runtime writers stay unchanged. The response-index bound follows its u16 wire
count because completed jobs can retain response windows. Returning a
new reader name in documentation does not make it callable before implementation.

## Direct-file workflow correction

The user clarified that received payloads are ready native files. There is no
recipient restoration/reconstruction phase: conventional file placement (or tar
extraction) followed by optional validation and ordinary start. The earlier
production restore/force/staging/rollback machinery and its tests are removed
from the plan; task03 now proves direct file usability only.

Stopping all outbe-chain/OCOMP/CE writers is required. Finishing Lysis is only a
recommended choice of cut, not a creation gate. Preserve unfinished native state;
creation does not compute missing results or wait for completion. Task09 checks the recommended post-Lysis cut and pending ordinary public
actions. Creation fixtures, not a donor-job migration subsystem, prove no gate.

State-root/tree recomputation described above belongs only to optional validation
and writes disposable scratch. It never reconstructs the files used by the node.
Local results before terminal confirmation remain valid native stages. Original
signed file hashes describe the initial cut, not later current-K databases.

## Verification boundary

The coordinator checked signature/files/headers/execution state; two independent
reviewers checked CE/projection and OCOMP, then exchanged findings on current-E
bindings, retained partitions and GC. Each used actual writer and reader paths;
path composition includes the caller, not just the leaf filename helper.

Graph project `outbe-chain`, generation `2026-09-04T15:12:00Z`, Tier2. Coverage was
checked for cited source paths: bin is excluded, several split modules are
untracked and multiple indexed files have changed metadata. Direct current-source
reads covered these gaps. Pinned Reth was examined directly in its dependency
checkout. No negative or exhaustive claim relies on clean graph coverage alone.
The source audit is bounded to the checks and lifecycle paths documented here.

No validator was implemented or run, no donor snapshot was certified and no
Cargo/E2E result is claimed. Real fixture, corruption and stopped-writer workflow
tests remain required implementation acceptance work. Production, tests and
Cargo files remain identical to `origin/main`; only planning documents changed.

Beads audit: `outbe-chain-g14b.24`. Implementation tasks remain open and stopped.
The findings are incorporated into task bodies, file ownership, tests/DoD,
operator workflow and Beads. Source evidence is not a substitute for the unit,
native fixture and real Lysis acceptance checks explicitly required there.
