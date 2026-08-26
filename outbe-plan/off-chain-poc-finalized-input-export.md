# Off-chain PoC: finalized input retention and authenticated export

Status: **resolved decision asset for ticket #5**

Scope: the disposable-devnet `LysisProgramV1` PoC only

Date: 2026-07-23

This asset selects the concrete PoC authority, retention and export path over
the repository as it exists today. It does not implement that path, choose the
final process/crate layout or introduce a generic snapshot framework.

Normative inputs:

- [`ADR-S-OCM-002`](../docs/adr/system/ADR-S-OCM-002-finalized-input-export-and-content-addressed-artifacts.md);
- [`off-chain-poc.md`](../off-chain-poc.md), especially sections 6 and 7;
- [`PFS-002`](../docs/flows/002-off-chain-poc-protocol-flow.md), especially
  protocol step 5 and `PFS-002-04`, `PFS-002-10`, `PFS-002-11`,
  `PFS-002-21` and `PFS-002-23`;
- the ticket-2
  [`current-code map`](off-chain-poc-current-code-map.md);
- the ticket-4
  [`protocol-byte freeze`](off-chain-poc-protocol-freeze.md).

## 1. Decision capsule

One finalized OCOMP input is a **composite authenticated checkpoint**, not a
copy of one database:

```text
Commonware finalization record + canonical request header
  -> request block hash and state root
       |
       +-> JobIntent storage proof
       +-> historical consensus-committee storage proof
       +-> Fidelity account/storage proof
       +-> Oracle account/storage proof
       |
       +-> exact CE FinalizedMarker
             -> sealed catalog root
             -> sealed WWD collection root
             -> canonical (TreeKey, body commitment) set
                   -> canonical Tribute bodies transported by Mongo
```

The selected PoC implementation has four source mechanisms:

1. the existing `CertifiedParentProofRecord` supplies the byte-identical
   Commonware finalization certificate and its exact epoch/view/committee
   metadata;
2. the existing Reth historical `StateProvider` supplies header-bound account
   and storage proofs at the request block hash;
3. the existing CE MDBX read transaction supplies an immutable collection view
   opened at the exact finalized marker;
4. the existing Mongo projection supplies candidate IDs and raw canonical body
   bytes, while CE commitments and a rebuilt collection root prove whether
   those bytes are complete and correct.

The node hands only a bounded lease descriptor and typed proof requests across
its control boundary. It does not stream Tribute bodies, expose a writer, accept
a database path or perform Lysis. The exporter reads CE through a fixed
read-only environment and Mongo through fixed read-only credentials, verifies
everything, and materializes immutable digest-addressed input chunks before any
worker runs.

The one-entry PoC pin is durable before a validator emits a positive vote.
Consensus and block finality continue if the pin, exporter, proof source, Mongo
or CAS is unavailable; that validator becomes an OCOMP abstainer.

No product choice requires grilling. The only newly found bounds are mechanical
closure of the already-selected bounded profile: the Oracle WWD array and
active S-curve range must be capped because current Oracle reads scan them.
Ticket #4 is amended, not redesigned, with candidate caps of `256` and the
normal generator may only reduce them.

## 2. Facts established from current production code

| Concern | Current fact | Consequence for the PoC |
|---|---|---|
| finality barrier | `ExecutorActor` awaits `FinalizedCeCommitter::commit_finalized` before acknowledging the finalized block | the CE marker after that call is the exact finalized height/hash, but OCOMP failure must not turn that existing barrier into an unbounded wait |
| durable finality bytes | `FinalizationActor` writes `CertifiedParentProofRecord` with exact Commonware `encoded_proof`, epoch/view, ordered committee and bitmap | reuse the record; do not re-encode a certificate or accept certified notarization |
| proof retention | `FinalizedParentCertStore` prunes below a fixed depth of 256 blocks | the 64-block result deadline plus 64-block PoC evidence window fits, but the exact record must be copied into the job handoff before it can age out |
| CE finality | `CeMdbx::open_snapshot` reads marker and sealed catalog root in one MDBX RO transaction | the transaction is a real immutable view and is the selected CE primitive |
| CE process boundary | `MdbxSnapshot` owns an in-process MDBX transaction and cannot be transferred to another process | the exporter must open its own RO transaction while the exact marker is still current |
| CE history | no current API opens an arbitrary historical CE marker | a later “open height H” is forbidden; exact opening is coordinated at finalization |
| CE key identity | collection leaves are addressed by one-way `TreeKey` values derived from entity identity | Mongo may discover original entity IDs, but a rebuilt root—not the Mongo listing—proves completeness |
| Mongo atomicity | body mutations and `ProjectionCheckpoint` are committed in one Mongo transaction | a checkpoint at or beyond request height proves availability progress, not body correctness |
| projection readiness | the existing execution readiness helper requires an exact checkpoint and returns `ProjectionAhead` | OCOMP needs a separate “contains finalized height” availability predicate; changing execution readiness semantics is out of scope |
| Mongo lifecycle | Tribute retirement deletes current body records | a narrow retained-body namespace is required so an active pin survives retirement without keeping the live projection frozen |
| Reth history | `StateProviderFactory::state_by_block_hash`/`history_by_block_hash` and `StateProofProvider::proof` are available | use exact block-hash historical proofs under a PoC archive/no-prune prerequisite |
| committee history | ValidatorSet slots 31..39 and 47 store the epoch/hash-keyed ordered committee, BLS keys and VRF material | prove those exact slots against the request state root and rebuild the existing `CommitteeSnapshot` |
| Fidelity | `compute_rcfi_fp`, `max_rcfi_at` and `league_at` read qualified-start and all active/sold cohorts | prove raw slots for every distinct owner; never export a trusted precomputed league |
| Oracle | current lookup scans the WWD pair array and every active S-curve entry | prove the raw arrays and cap both scanned ranges before allocation |
| body readers | `RuntimeBodyReaders` and typed Tribute repositories expose live projection reads | reuse codecs/repositories, but add a separate read-only retained/current resolver for export |
| missing surface | no OCOMP pin, checkpoint lease, typed historical proof source, exporter, input manifest or CAS exists | these are real implementation tasks; this document does not claim otherwise |

Primary current-code anchors:

- [`FinalizedCeCommitter` and `FinalizedCeBlock`](../crates/blockchain/consensus/src/executor/actor.rs);
- [`RethCeFinalizer`](../crates/blockchain/engine/src/ce_finalizer.rs);
- [`CeMdbx::open_snapshot` and `MdbxSnapshot`](../crates/core/compressed-entities/src/persistence.rs);
- [`AuthenticatedCatalogView`](../crates/core/compressed-entities/src/staging.rs);
- [`ProjectionReadinessHandle`](../crates/blockchain/primitives/src/projection.rs);
- [`OffchainDataProjection::apply_prepared`](../crates/system/offchain-data/src/lib.rs);
- [`MongoStorage`](../crates/blockchain/offchain-storage/src/mongo.rs);
- [`CertifiedParentProofRecord` and `FinalizedParentCertStore`](../crates/blockchain/consensus/src/finalization/parent_cert_store.rs);
- [`CommitteeSnapshot`](../crates/blockchain/consensus/src/proof/committee.rs);
- [ValidatorSet committee snapshot schema](../crates/system/validatorset/src/schema.rs);
- [Fidelity schema](../crates/core/fidelity/src/schema.rs);
- [Oracle schema](../crates/system/oracle/src/contract.rs).

## 3. One authoritative checkpoint identity

The semantic checkpoint is the already-frozen `CheckpointIdentityV1`:

```text
CheckpointIdentityV1 {
  finalized_block_number = H,
  finalized_block_hash   = BH,
  finalized_state_root   = SR,
  finalized_ce_root      = CR,
  ce_schema_version
}
```

The following equalities are mandatory before export:

```text
keccak256(canonical_request_header_rlp) == BH
request_header.number                   == H
request_header.state_root               == SR

CertifiedParentProofRecord.kind         == FINALIZATION
record.finalized_block_number           == H
record.finalized_block_hash             == BH

CE FinalizedMarker.height               == H
CE FinalizedMarker.block_hash           == BH
CE FinalizedMarker.new_root             == CR

JobIntent.ce_sealed_root                 == CR
JobIntent collection root/key/count      == CE authenticated values
JobId                                   == H(IntentId || BH || SR)
```

The request event is only a discovery hint. The Mongo checkpoint, current node
tip, filesystem location and CAS path are not fields in semantic identity.

## 4. Finalized-intent proof source

### 4.1 Exact current sources

The bounded node-side proof source constructs `FinalizedIntentProofV1` from:

1. canonical request header RLP read by exact block hash;
2. the `CertifiedParentProofRecord` whose key was captured with the candidate
   round in the tentative pin;
3. one ValidatorSet account/storage proof for the historical committee snapshot
   selected by `(record.finalized_epoch, record.committee_set_hash)`;
4. one OCOMP/Metadosis account/storage proof for the exact persisted
   `JobIntentV1` slots;
5. the canonical `JobIntentV1` bytes independently decoded from those slots.

`CertifiedNotarization` is not an accepted fallback. Missing proof bytes,
default/zero VRF fields, a record whose height/hash differs, a committee
snapshot whose key/hash/order differs, or a state proof against any root other
than `SR` makes the local proof unavailable.

### 4.2 Historical committee opening

The proof includes exactly the ValidatorSet storage needed to reconstruct the
existing `CommitteeSnapshot`:

```text
snapshot_key = committee_snapshot_key(
  record.finalized_epoch,
  record.committee_set_hash
)

slot 31: exists[snapshot_key]
slot 32: len[snapshot_key]
slot 33: address[snapshot_key][0..len)
slot 34: pubkey_lo[snapshot_key][0..len)
slot 35: pubkey_hi[snapshot_key][0..len)
slot 36: vrf_material_version[snapshot_key]
slot 37: vrf_group_public_key_hash[snapshot_key]
slot 38: vrf_group_public_key_len[snapshot_key]
slot 39: vrf_group_public_key_chunk[snapshot_key][0..ceil(len/32))
slot 47: vrf_public_polynomial_hash[snapshot_key]
```

PoC requires `len == 4`. The verifier:

1. verifies the Ethereum account/storage proof against `SR`;
2. reconstructs `CommitteeSnapshot`;
3. checks its canonical set hash, VRF version and group-key hash against the
   finalization record;
4. invokes the existing `verify_v2_proof`;
5. checks the proof proposal payload, epoch/view and parent view exactly;
6. rejects trailing proof bytes and any proof kind other than finalization.

The exporter and activation verifier use the same pure decoder/verifier. The
node-side producer is a convenience and availability source, not a trust root.

### 4.3 Bounded adapter instead of broad RPC

The selected PoC seam is a typed read-only proof adapter owned by the node:

```text
BuildFinalizedIntentProof(JobId)
BuildLysisOpenings(JobId, OpeningSubjectsV1)
```

It does not accept:

- an arbitrary block, contract address or storage key;
- a caller-selected proof kind or committee;
- a filesystem/database path;
- an unbounded owner/currency list;
- an EVM call, worker command or Lysis program.

`BuildLysisOpenings` derives contract addresses and storage slots from the
frozen Fidelity/Oracle schemas. It may read count/index slots to construct a
witness, but it performs no league, price, allocation or Lysis calculation.
The exporter independently derives and checks the complete expected slot set
from the returned values.

This avoids adding a new public consensus wire type. Ticket #6 will decide
how the exporter consumes public RPC; the current implementation uses finalized
blocks, receipts, exact-block calls and proofs, with no node-private endpoint.

## 5. One-entry durable pin

### 5.1 State

The PoC journal stores exactly one versioned, checksummed record:

```text
EMPTY

TENTATIVE {
  IntentId,
  candidate_height,
  candidate_block_hash,
  candidate_state_root,
  candidate_ce_root,
  finalization_proof_key = (epoch, view, candidate_block_hash),
  protocol_bundle_hash
}

FINALIZED {
  all TENTATIVE fields,
  JobId,
  deadline_height,
  canonical_finalization_record: bounded bytes,
  finalization_record_hash,
  checkpoint_identity,
  lease_generation
}

EXPORTED {
  all FINALIZED identity fields,
  finalized_intent_proof_hash,
  InputManifestHash,
  source_snapshot_certificate_hash
}

TERMINAL {
  all EXPORTED identity fields that exist,
  terminal_kind,
  terminal_finalized_height,
  terminal_block_hash,
  release_height
}

RELEASED {
  previous_record_hash,
  released_at_finalized_height
}
```

`RELEASED` is retained as a tombstone until the next admissible job replaces it.
The journal update protocol is temporary file, exact length/checksum,
`fsync(file)`, atomic rename and `fsync(parent directory)`.

### 5.2 Write-before-vote ordering

For a candidate block that creates an intent:

1. normal block verification and OCOMP pre-admission succeed;
2. the validator derives the exact intent, header state root and consensus
   proof key from the candidate;
3. the local coordinator durably writes `TENTATIVE`;
4. only after the durable acknowledgement may the consensus application emit a
   positive vote.

Failure to pin drops/withholds that validator's vote; it does not mark an
otherwise valid block invalid. Semantic export, execution and signing are still
forbidden until finality.

With a non-identical live `TENTATIVE` record, the one-entry PoC validator
abstains from a conflicting intent candidate. It never overwrites the first pin.
This deliberately trades one validator's liveness for a bounded, unambiguous
PoC journal.

### 5.3 Reconciliation

On every finalized block:

- same height/hash as the pin: wait for the existing CE durable-commit
  notification and durable exact finalization record, persist its bounded
  canonical bytes in the pin, derive `JobId`, and transition to `FINALIZED`;
- same height with another hash, or a finalized chain that has passed the
  candidate height without that hash: write `RELEASED` and revoke all local
  artifacts for the orphan;
- a terminal activation/expiry/conflict/cancel for the same job: write
  `TERMINAL` only after the terminal block is finalized.

On restart:

- a valid `TENTATIVE` is reconciled against canonical/finalized block identity
  before any OCOMP signing;
- a valid `FINALIZED`/`EXPORTED` is reconciled against the canonical job state;
- a corrupt, partially written, contradictory or unknown-version record puts
  only OCOMP into quarantine; consensus may continue;
- quarantine never guesses that retained data may be deleted.

### 5.4 Exact PoC release rule

The fork-pinned PoC evidence window is 64 finalized blocks:

```text
release_height = terminal_finalized_height + 64
```

Release happens only after `release_height` itself is finalized and after the
source manifest/evidence handoff is durable. No worker, supervisor or exporter
request can release the pin. Before release, CAS GC and retained-body GC for
that job are forbidden.

For the expiry path, the expiry transition is the terminal transition. For an
orphaned `TENTATIVE`, no semantic job exists and release is immediate after the
conflicting canonical finality is known.

The 64-block evidence window equals the already-frozen result deadline and
keeps request-to-release retention within 128 blocks in the worst expiry path,
below the current 256-block finalization-record depth. Capacity generation must
still prove disk headroom; configuration may not shorten the consensus profile.

## 6. Composite checkpoint handoff

### 6.1 CE lease

The exporter is a long-running sibling process with read-only access to one
service-manager-configured CE MDBX environment. The supervisor never receives
that path or capability.

The finalizer/exporter coordination is:

```text
node durably commits CE marker H/BH/CR
  -> node publishes fixed-size ArmCeLease(LeaseId, H, BH, CR, schema)
  -> exporter opens its own MDBX RO transaction on the fixed environment
  -> exporter verifies marker and sealed catalog root exactly
  -> exporter acknowledges CeLeaseOpened(same identity, lease generation)
  -> later CE commits may proceed
```

Because current CE has no historical-open API, the writer must not pass marker
`H` before the exporter has either acknowledged or the bounded lease-open budget
has expired. The request block finality acknowledgement is not held for the
bulk export. At most the next CE apply observes this small bounded gate. Timeout,
closed public-RPC connection or wrong identity clears the gate, allows CE/finality to proceed and
marks this validator's input unavailable.

The exporter keeps the one MDBX transaction open for an export attempt. A retry
inside the same exporter process resumes/rebuilds content-addressed chunks from
that immutable view. Once a complete manifest and source snapshot certificate
are durable, workers need only CAS.

PoC does **not** claim recovery from the exporter process dying before
`EXPORTED` after CE has advanced. That domain abstains. Crash-safe named
checkpoint recovery is explicitly BoundedMVP work. Worker restart and
export-attempt retry remain supported because they do not destroy the CE
lease owner.

Implementation must add a true read-only CE open mode. Calling the current
writer-capable `CeMdbx::open` from the exporter under a “please do not write”
convention is not acceptable.

### 6.2 Reth historical state

The PoC devnet runs Reth with the history required by exact
`state_by_block_hash(BH)` and trie-proof generation retained at least through
the release window. Startup and per-job OCOMP readiness probe:

1. exact canonical header by `BH`;
2. historical state provider by `BH`;
3. one bounded known-slot proof whose root verifies as `SR`.

Failure disables local OCOMP work, not the node.

No Reth database path or provider object crosses the process boundary. The
typed proof adapter returns only capped canonical proof artifacts. Full
production pruning coordination is outside PoC; the disposable devnet's
archive/no-prune configuration and the bounded one-job window are explicit
evidence inputs.

### 6.3 Mongo availability and retention

The existing exact readiness helper is left unchanged for EVM execution.
OCOMP adds a separate predicate:

```text
projection_contains(request H/BH) iff
  projection_checkpoint.number >= H
  AND local canonical_hash(H) == BH
  AND the projection checkpoint itself passed existing finalized/canonical checks
```

An ahead projection is allowed because it is not authority. Every accepted
body must still match the CE leaf at `H`. The sealed Tribute binding and live
activation preconditions reject a changed or retired source partition.

Mongo supplies two read-only locations behind one typed resolver:

```text
current canonical Tribute repository
OR
OcompRetainedTribute(JobId, WWD, EntityId, BodyCommitment)

candidate identity discovery =
  current TributeByDay(WWD) index
  UNION OcompRetainedTributeByDay(JobId, WWD) index
```

When projection applies a pinned partition retirement, it atomically copies the
exact canonical body, identity, day and commitment plus its retained day-index
entry to the retained namespace before deleting the current record and current
day-index entry. The retained record is immutable; a different byte sequence
for the same key is corruption. The active job journal is the only PoC
retention selector. Release GC deletes only that exact job namespace and index
after the node-owned release rule. Deletion is cursor/page bounded across the
parent job namespace: reaching the end of one worker-shard-sized page continues
with the next page. It is never interpreted as “too many Tribute for this job”
and never causes partial deletion on a failed page.

The exporter may read current or retained bytes. A concurrent atomic move is
safe because either copy must decode to the same identity and commitment.
Missing both copies causes local abstention. No Mongo count, page boundary,
metadata timestamp or query order is ever accepted as completeness evidence.

The exporter uses read-only Mongo credentials limited to the required current
Tribute indexes and OCOMP retained namespace. It has no projection writer lease.

## 7. Exact export algorithm

For one `FINALIZED` pin, the exporter performs these steps in order:

1. decode the canonical job spec and enforce chain, genesis, fork, bundle,
   attempt and deadline;
2. verify `FinalizedIntentProofV1`, derive `IntentId`, `JobId`, `BH`, `SR`, `CR`
   and compare every value with the lease;
3. open `AuthenticatedCatalogView` over the held CE transaction with the exact
   `ExactParentIdentity`;
4. authenticate the sealed Tribute WWD collection key/root and verify the
   expected 16-shard topology;
5. wait for `projection_contains(H/BH)` under the local export deadline;
6. page the union of current and job-retained Tribute-by-WWD indexes only to
   discover candidate entity IDs;
7. reject an invalid ID, duplicate, non-canonical order after normalization,
   cap overflow or pagination cycle;
8. for each canonical sorted ID, load current-or-retained body bytes, enforce
   byte cap before decoding, canonical-decode, and verify identity and WWD;
9. compute the frozen body commitment and `TreeKey`, read the exact CE leaf from
   the held view, and require byte equality with that commitment;
10. insert every `(TreeKey, commitment)` into a fresh profile-pinned 16-shard
    fold and rebuild the collection root;
11. require rebuilt root, body count and checked nominal sum to equal
    `JobIntentV1`; omission, addition, mutation or wrong-day substitution fails;
12. derive the canonical distinct owner set and reference-currency set from the
    now-authenticated bodies and enforce their caps;
13. partition sorted unique owners into consecutive batches of at most 256,
    request typed Fidelity/Oracle state proofs at `BH` for every batch, verify
    the Ethereum proofs against `SR`, verify the complete expected raw slot set,
    and decode canonical openings; owner 257 starts the next batch and no total
    owner/Tribute admission cap is introduced; if a canonical response exceeds
    the bundle-pinned control-body cap, accept only typed `LimitExceeded`,
    bisect that exact owner batch at `floor(len / 2)` left-first while
    preserving the complete ISO set, and abstain if one owner's proof still
    cannot fit;
14. independently reproduce Fidelity/Oracle read semantics at the intent's
    `logical_evaluation_height/time`; no live call or wall clock is allowed;
15. encode Tribute, Fidelity and Oracle chunks in frozen canonical order;
16. publish each chunk through the CAS protocol and keep the exact open stream
    used for hashing/validation;
17. construct `InputManifestV1`, recompute all list/opening roots, counts and
    byte totals, decode it again under caps, and publish it by digest;
18. write the source snapshot certificate binding lease identity, proof hash,
    manifest hash and ordered chunk digests;
19. ask the node to transition the exact live pin to `EXPORTED`;
20. only after the durable acknowledgement may the supervisor discover the
    manifest and schedule semantic work.

Mongo can therefore omit every second ID, return duplicates or return a valid
body under the wrong day: the rebuilt CE root/count/nominal will not close.
Mongo is useful for discovery and byte availability but has no way to authorize
a different input.

## 8. Fidelity and Oracle openings

### 8.1 Fidelity

For each distinct owner in ascending address order, the opening proves:

- `qualified_start[owner]`;
- `first_qualified_start[owner]`;
- `active_count[owner]`;
- every active cohort's `size` and `acquired_at`;
- `sold_count[owner]`;
- every sold cohort's `size`, `acquired_at` and `sold_at`.

The exporter first verifies the count slots, requires both counts to be at most
64, derives the exact remaining mapping slots, and rejects an extra, missing or
duplicate slot. It then invokes the same frozen integer semantics as
`compute_rcfi_fp`, `max_rcfi_at` and `league_at` using the logical request time.

The authenticated value is the raw opening plus its deterministically derived
league inputs. A node-returned or Mongo-stored “league” is never authority.

Each consecutive owner batch is encoded as one source-specific
`AuthenticatedOpeningV1`. Its subject is the canonical ordered owner vector,
its value is the canonical ordered raw `(slot,value)` vector, and its opening
is the canonical nested `RawContractOpeningProofV1`. The batch size is a
request/allocation bound only; the complete job may contain any number of
batches. The manifest Fidelity opening root uses the registered
`FIDELITY_OPENINGS(9)` ordered-list kind.

### 8.2 Oracle

For mandatory ISO `840` plus every distinct Tribute reference currency, the
opening proves:

- settlement ISO-to-pair hash and pair-hash-to-ID mappings;
- WWD VWAP existence, pair count, every pair ID and every VWAP value for the
  requested WWD;
- global active S-curve `count` and `oldest`;
- every active S-curve entry's pair ID, peak day and peak price.

The exporter proves and scans the same raw arrays as current Oracle logic. It
does not accept a node-returned final price as authority.

The complete sorted ISO set, including 840, is encoded once as the job-wide
Oracle `AuthenticatedOpeningV1`. If bounded Fidelity requests repeat the Oracle
proof, every copy must be byte-identical; disagreement aborts export and the
single canonical copy is published once. The manifest Oracle opening root uses
the registered `ORACLE_OPENINGS(10)` ordered-list kind.

Two bounds were missing from the first ticket-4 draft:

```text
max_oracle_wwd_pair_entries = candidate 256
max_active_scurve_entries   = candidate 256
```

Both counts are checked immediately after the small count proof and before
allocating detail slot lists. The capacity generator may reduce either value
but may not raise it.

The Metadosis auction/day entry value already frozen inside `JobIntentV1` is
authenticated by the intent storage proof. It is not recomputed from a later
Oracle view.

## 9. CAS publication and consumption contract

Ticket #6 owns the final directory layout and UID assignment, but the following
integrity rules are already fixed:

- CAS root is service-manager supplied, never caller supplied;
- only regular files beneath that root are accepted;
- temporary objects are private, exclusive-create files with no symlink
  following;
- encoded length is capped before allocation or cryptography;
- the producer hashes the exact bytes it writes, `fsync`s the file, atomically
  publishes under the lowercase digest, and `fsync`s the containing directory;
- an existing digest object is accepted only after hashing its exact open
  stream and matching length;
- objects are never modified in place;
- a consumer opens once, verifies file type/ownership/link policy, and hashes
  the exact stream while decoding it;
- a worker resolves the canonical `ProtocolBundleV1` only from the fixed
  service-owned `/etc/outbe/ocomp/protocol-bundle-v1.ocb1` path and requires its
  hash to equal the control-session bundle hash;
- “hash, close, reopen, consume” is forbidden;
- manifest membership, kind, ordinal, semantic digest, transport digest,
  encoded length and record count must all match;
- missing, duplicate, overlapping, reordered, trailing and unused chunks fail;
- a corrupt object may be rebuilt from a live source lease, but no corrupt
  result reaches the sign-once gate.

Filesystem permissions reduce attack surface; digests and authenticated roots
provide correctness.

## 10. Ownership and minimum interfaces

These are logical deep-module boundaries, not a decision to create one crate per
row:

| Owner | Narrow responsibility | Must not own |
|---|---|---|
| node `OcompRetentionCoordinator` | one-entry pin FSM, finality reconciliation, release authority, lease generation | bulk body transfer, Lysis, worker scheduling |
| consensus vote hook | durable tentative-pin acknowledgement before positive vote | semantic export or signing |
| CE finalizer lease hook | publish exact marker and bound the next-apply open gate | export traversal or CAS writes |
| node `FinalizedInputProofSource` | exact header/finality/typed state proofs at one `JobId` | arbitrary state queries or trusted derived values |
| offchain projection retention seam | atomically retain/delete pinned raw bodies | input completeness or job authority |
| exporter | verify authority chain, full-fold input, publish manifest/chunks | private OCOMP key, result signing, canonical state writes |
| CAS adapter | durable digest-addressed bytes and job capability | semantic truth, release policy |

Required conceptual methods:

```text
record_tentative(candidate) -> DurablePinAck
observe_finalized(block, proof_key) -> FinalizedJobOrOrphan
open_ce_lease(finalized_job) -> CeLeaseAck
build_finalized_intent_proof(JobId) -> bounded proof
build_lysis_openings(JobId, subjects) -> bounded raw proofs
resolve_tribute_body(JobId, WWD, EntityId, commitment) -> canonical bytes
record_exported(JobId, lease_generation, manifest_hash, certificate_hash)
observe_terminal_finality(JobId, terminal)
release_due(finalized_height) -> Released
```

Every state-changing method uses compare-and-set identity including record hash
and lease generation. A stale exporter cannot mark a replacement job exported
or release it.

## 11. Failure semantics

| Failure | Required result |
|---|---|
| pin journal fsync fails before vote | local validator withholds positive vote |
| conflicting second tentative intent | local validator abstains; first record is not overwritten |
| request candidate orphaned | tentative pin becomes released; every derived artifact is non-signable |
| finality record missing/wrong kind | local OCOMP unavailable; consensus continues |
| CE marker already ahead before lease opens | local OCOMP unavailable; never read live CE as if it were `H` |
| exporter misses bounded CE-open gate | next CE apply continues after timeout; local OCOMP abstains |
| exporter process dies before export closes | local OCOMP abstains; no claimed PoC crash recovery |
| Worker restarts | resume from durable manifest/CAS; node finality is unaffected |
| Mongo checkpoint behind | bounded wait, then local abstention |
| Mongo checkpoint ahead | allowed after canonical containment check |
| Mongo body missing/mutated/extra | leaf/root/count/nominal closure fails; no manifest/signature |
| pinned body retires | atomic retained copy remains available until node release |
| historical Reth state/proof pruned | local OCOMP unavailable; no live-state fallback |
| Fidelity/Oracle count above cap | pre-admission/export rejection before detail allocation |
| state proof/root/slot set mismatch | no authenticated opening, manifest or signature |
| CAS full/corrupt/TOCTOU mutation | local retry from live lease or abstention; no signature |
| pin journal corrupt after restart | OCOMP quarantine; consensus continues; no automatic GC |
| terminal state finalized | retain until exact release height, then node-owned GC |

There is no synchronous Lysis fallback in any row.

## 12. Mandatory implementation evidence for this decision

Ticket #9 will assign final test IDs and commands. The implementation plan must
still contain at least these tests before this decision can be called
implemented:

### 12.1 Fast and component tests

- pin journal golden bytes, torn write, checksum/version failure and
  compare-and-set generation;
- positive vote cannot be emitted before durable tentative acknowledgement;
- conflicting candidate does not overwrite the one-entry pin;
- exact finalization, orphan, exported, terminal and release model sequences;
- existing `MdbxSnapshot` remains immutable while the writer advances;
- separate read-only exporter open rejects writer operations and wrong marker;
- next-apply lease gate succeeds, times out boundedly and never blocks the
  request finality acknowledgement;
- finality proof producer/verifier positive vectors and mutations of every
  header, epoch/view/hash, committee, bitmap, VRF and storage-proof binding;
- `projection_contains` behind/exact/ahead/conflict cases without changing
  execution readiness behavior;
- atomic current-to-retained body move and restart-safe paged release GC,
  including `max_tributes_per_work_shard + 1` records with the final record
  removed only as part of complete parent-job release;
- Mongo omitted/duplicate/reordered/wrong-day/changed-body cases all fail root
  closure;
- Fidelity count `0`, `64`, `65` and raw-slot omission/mutation;
- Oracle WWD/S-curve cap `cap-1`, `cap`, `cap+1` and raw-slot
  omission/reordering/mutation;
- CAS create/publish/reopen, symlink/hardlink/non-regular-file, truncation,
  replacement-during-read and existing-object collision tests.

### 12.2 Real-boundary integration

- real Reth historical state proof at exact block hash verifies against the
  canonical request header and fails for the adjacent block;
- real ValidatorSet committee storage proof reconstructs the exact snapshot
  accepted by `verify_v2_proof`;
- real CE writer plus sibling exporter opens height `H`, writer advances, and
  exporter still folds `H`;
- real Mongo transaction applies bodies plus checkpoint atomically; an ahead
  projection remains usable only through commitment/root verification;
- real retirement atomically moves pinned bodies to retained storage while the
  live projection continues;
- stop Workers during an active CE lease: blocks still finalize
  and export can be rescheduled;
- terminate exporter before `EXPORTED`: blocks still finalize, local signature
  remains absent and the test does not claim recovery;
- restart an export attempt inside the live exporter, reuse/rebuild partial CAS
  objects and produce the identical manifest;
- mutate Mongo and CAS independently and prove no sign-once record is created.

### 12.3 Retained evidence

Each run retains:

- request/finality block number, hash, header RLP and state root;
- finalization-record key, byte hash and verifier outcome;
- historical committee proof and reconstructed snapshot hash;
- pin-journal transition hashes and fsync/ack ordering trace;
- CE marker, lease generation, open/timeout timestamps and collection roots;
- Mongo projection checkpoint, current/retained source selection and injected
  mutation;
- Fidelity/Oracle proof roots, exact slot-plan hash and observed counts;
- every chunk/manifest/source-certificate digest and exact byte length;
- terminal block, release height and GC evidence;
- explicit absence of result signature for every negative scenario.

Logs alone are not the oracle. Root/proof verification and absence/presence of
the durable sign-once record are the allowed local correctness assertions.

## 13. Scope and evolution

This decision adds only the seams needed by one bounded Lysis job:

- one pin record;
- one exact CE lease;
- one typed finalized-intent proof;
- one typed Fidelity/Oracle proof plan;
- one retained Tribute namespace;
- one authenticated manifest.

It does not add:

- a generic checkpoint service or `ProgramRegistry`;
- arbitrary historical CE queries;
- a second projection database;
- worker access to Reth/Mongo/CE;
- remote mTLS, distributed CAS or custody protocol;
- production pruning reconciliation, exporter-process crash recovery, named
  checkpoint restore or multi-job fairness;
- proof-carrying execution or TargetLarge streaming.

BoundedMVP may replace the archive prerequisite, one-entry journal, CE-open
gate, local retained namespace and filesystem CAS with crash-safe production
stores. It must preserve:

```text
finalized JobId/state root
-> exact CE collection commitment
-> commitment-verified bodies
-> state-root-verified openings
-> canonical manifest/chunks
-> digest-verified worker consumption
```

Therefore the transition changes operational durability and capacity, not
`JobIntentV1`, `FinalizedIntentProofV1`, `CheckpointIdentityV1`,
`InputManifestV1` or Lysis input meaning.

## 14. Rejected alternatives

- **Let workers query Mongo:** broad authority and mutable availability leak
  into semantic execution.
- **Trust the Mongo WWD list/count:** an omitted ID could silently change Lysis.
- **Require Mongo checkpoint exactly `H` forever:** the projection would have to
  stop and would soon stall the node's existing execution readiness.
- **Stream CE/Mongo through node RPC:** bulk work and backpressure enter the
  consensus process.
- **Open current CE after finality and assume it is historical:** current code
  has no such guarantee.
- **Copy the entire CE/Reth database inside the node:** violates the O(1)
  handoff and adds consensus-process I/O.
- **Use `eth_call` or a precomputed league/price:** returns a value, not a proof
  of the exact raw inputs and complete current semantics.
- **Expose arbitrary `eth_getProof` storage keys to the exporter:** broader than
  the one typed program boundary and easier to misuse.
- **Freeze the Mongo projection until terminal:** later execution would wait for
  missing parent checkpoints, coupling exporter health to finality.
- **Rely on read-only file permissions instead of rehashing CAS streams:**
  permissions do not close mutation/TOCTOU correctness.

## 15. Ticket #5 closure

Ticket #5 is resolved because this asset now fixes:

- the exact finalized authority chain and equality checks;
- the current finality record and historical committee sources;
- the current CE MVCC primitive and the only safe cross-process open window;
- the exact relation between an ahead/behind Mongo projection and request
  finality without making Mongo authoritative;
- raw-body retention across Tribute retirement;
- typed Fidelity/Oracle state-proof construction and verification;
- the one-entry pin FSM, orphan behavior and exact release rule;
- CAS publication/consumption integrity;
- local abstention behavior for every unavailable/ambiguous source;
- the PoC/MVP boundary and mandatory verification evidence.

Ticket #6 may select process names, UIDs, public-RPC limits, filesystem layout and
cgroup values. It may not replace this authority chain, move bulk bytes through
the node or weaken any failure to a best-effort live read.
