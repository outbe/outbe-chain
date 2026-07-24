# Off-chain PoC deterministic execution and quorum evidence

Status: **RESOLVED — decision ticket #7**

This decision maps the frozen Lysis semantics and protocol bytes to one exact
deterministic work graph, one node-owned result signature per validator domain,
a durable sign-once store, a `3-of-4` certificate and an untrusted relay.

It depends on:

- the [Lysis V1 semantic baseline](off-chain-poc-lysis-v1-semantics.md);
- the [protocol-byte freeze](off-chain-poc-protocol-freeze.md);
- the [finalized input/export decision](off-chain-poc-finalized-input-export.md);
- the [process/artifact topology](off-chain-poc-process-and-artifact-topology.md);
- `ADR-S-OCM-003` and `PFS-002`.

It does not decide consensus activation/apply ownership; that remains ticket #8.

## 1. Authority and result story

For each validator domain independently:

```text
finalized JobId + verified InputManifestV1
  -> LysisPlannerV1 commits the primary work-unit count/root in PlanCommitmentV1
  -> bounded cursors derive immutable UnitSpecV1/UnitId values by ordinal
  -> local scheduler runs them in any order allowed by the derived DAG
  -> phase verifier accepts only exact producer-bound artifacts
  -> fixed streaming reducers publish bounded ResultChunkV1 objects
  -> ROOT_REDUCE produces constant-size LysisResultV1
  -> supervisor sends the result commitment with constant-size chunk count/root
  -> node reloads the finalized job/export binding and reconstructs ResultDigest
  -> node durably installs one SignOnceRecordV1
  -> node releases one validator-index signature
  -> external relay collects three distinct matching announcements
  -> relay builds one ordinary public activation transaction
```

The four domains do not exchange plans, manifests, artifacts or results before
signing. The relay is the first inter-domain convergence point. Four worker
processes inside one domain still produce at most that domain's single
validator-index signature.

The safety claim is intentionally narrow:

- independent domains plus `q=3` tolerate one faulty/unavailable domain;
- exact deterministic semantics make honest results byte-identical;
- sign-once prevents an honest domain from authorizing two results for one
  attempt;
- the separate reference corpus catches shared semantic mistakes;
- quorum does not prove implementation diversity, data availability or general
  program correctness.

## 2. Current code reuse and required seams

| Existing evidence | Reuse | Required change |
|---|---|---|
| `crates/core/lysis/src/runtime.rs::lysis_inner` | semantic oracle for order, integer arithmetic, two Fidelity observations, Oracle rules and first failure | extract storage-independent `program_v1` functions; workers must not call the mutating runtime |
| `BTreeMap` ordering and checked/wrapping `U256` operations in current Lysis | exact baseline selected by ticket #3 | pin every order/arithmetic operation in the independent corpus and work-output schemas |
| workspace `k256 = 0.13` and `OutbeEvmSigner` prehash use | library/profile precedent | add a separate node-only OCOMP key type; do not reuse the EVM signer, address or recoverable 65-byte format |
| Commonware committee/index/certificate code | precedent for distinct indexed participants and duplicate rejection | OCOMP remains a separate four-member secp256k1 snapshot/certificate |
| `outbe_getFinalization(height)` | exact Commonware finalization and block byte transport | relay verifies it; never trusts RPC JSON as authority |
| Reth standard Ethereum RPC | exact block/header and EIP-1186 state-proof transport | PoC node retains request history and relay uses exact block-hash `eth_getProof` |
| slashing/governance JSONL journals | operational diagnostics only | not reusable for sign-once: they use JSON, only flush, swallow write errors and have no recovery authority |
| E2E harness child guards/restart controls | real process kill/restart ownership | add worker schedule, node signer fault points and cross-domain evidence |

The current storage-bound Lysis function remains available only for the
pre-fork/compatibility paths selected by ticket #8. It is not called by a worker,
attestation gate, activation verifier or PoC post-fork success path.

## 3. Complete deterministic plan

### 3.1 Canonical source partition

The exporter has already authenticated and sorted Tribute by raw 36-byte
`EntityId`. The planner independently external-sorts the bounded stream and
requires byte equality with exporter order. This is a concrete Lysis-specific
sort, not a reusable distributed-sort framework:

- primary work-shard size: exactly 256 Tribute except the last;
- comparison key: raw `EntityId36`;
- duplicate key: reject;
- merge: stable two-way merge, lower run ordinal first only for an impossible
  equal-key diagnostic;
- compression: none;
- any number of bounded runs;
- all individual run bytes/counts and simultaneously open merge readers are
  bounded before allocation/spill.

The second sort is deliberate defense against depending on Mongo/chunk order.
The deterministic merge hierarchy uses fixed binary fan-in and ascending
run ordinals. It is derived lazily from the committed run count; no generic
shuffle engine or external service is required.

For sorted IDs `id[0..T)` and `K = ceil(T/256)`, range `j` is:

```text
start = id[256*j]
end   = id[min(256*(j+1), T)] when another record exists, else none
```

Ranges are non-empty, start-inclusive, end-exclusive, adjacent and cover the
complete source exactly once. Raw ordinal is the position in this one stream.

### 3.2 Plan-wide rules

`LysisPlannerV1` is a pure function of:

```text
ProtocolBundleV1
JobId / attempt
InputManifestV1 and authenticated source roots/counts
the canonical sorted EntityId stream
generated capacity constants
```

It performs no CAS discovery, wall-clock read, randomness, network request,
worker-count query or local tuning. It emits constant-size
`PlanCommitmentV1` before the first worker runs. A unit spec is a pure
derivation from that commitment and its ordinal, so the supervisor never
allocates or persists a vector proportional to `K`.

The protocol-freeze amendment replaces the unusable “future semantic root”
input with a producer binding:

- authenticated input refers to an exact semantic root;
- derived input refers to a producer `UnitId`;
- a padded empty input refers to a registered canonical empty ID.

This removes a plan/execution cycle: downstream `UnitId`s no longer require
unknown future artifact digests.

The exact plan order is phase numeric order, then interval order:

```text
ENUMERATE
FIDELITY_MAP
FIXED_REDUCE levels bottom-up, index ascending
AMOUNT_MAP
GRATIS_PREFIX
GRATIS_PREFIX_DOWN
OUTPUT_FINALIZE
OWNER_SHUFFLE
BUCKET_SHUFFLE
ROOT_REDUCE
```

`PlanHash` hashes `PlanCommitmentV1`, which commits the exact primary work-unit
count and ordered primary work-unit root. Scheduler order is not plan order and
has no semantic effect. A primary unit is verified against that root; every
secondary phase unit is verified against the frozen derivation rule and its
position-bound producer commitments.

### 3.3 Exact DAG

For `K` source ranges:

```text
K x ENUMERATE
  -> K x FIDELITY_MAP
  -> padded binary FIXED_REDUCE tree
  -> K x AMOUNT_MAP
  -> padded binary GRATIS_PREFIX summary tree
  -> padded binary GRATIS_PREFIX_DOWN propagation tree
  -> K x OUTPUT_FINALIZE
  -> bounded OWNER_SHUFFLE run/merge tree
  -> bounded BUCKET_SHUFFLE run/merge tree
  -> padded binary ROOT_REDUCE tree
  -> LysisResultV1
```

The Fidelity tree uses:

```text
P = max(2, next_power_of_two(K))
leaf 0..K-1 = FIDELITY_MAP UnitIds
leaf K..P-1 = EmptyUnitInputId(FIDELITY_PARTIALS)
level 1 combines adjacent leaves
level h combines adjacent level h-1 nodes
root = (level=log2(P), index=0)
```

Thus a one-range job still has one explicit reducer/root unit and fraction-table
production. No data-dependent tree shape or “reduce as completed” path exists.

`GRATIS_PREFIX` is the bottom-up half of a deterministic parallel scan. Every
unit combines at most two child segment summaries. `GRATIS_PREFIX_DOWN` is the
top-down half: every unit receives one bounded parent prefix plus at most two
child summaries and emits child prefixes. Leaves therefore learn their exact
incoming remaining Gratis without any unit reading all `K` segments.

`ROOT_REDUCE` uses the same fixed padded binary topology. Each unit consumes at
most two child commitments; only the final root unit emits
`LysisResultV1`. No input vector grows with `K`.

`OWNER_SHUFFLE` and `BUCKET_SHUFFLE` first emit one bounded sorted run per
primary range, then merge adjacent run spans in a fixed binary hierarchy.
Every merge consumes at most two runs and produces bounded output chunks plus a
catalog commitment. Run count may be arbitrary; open readers, output chunk
bytes and resident memory remain bounded.

The primary unit catalog is committed by count/root. The remaining phase units
are a pure function of that commitment, `K`, planner version and reducer
version; they are enumerated through bounded cursors and are never stored as
one vector. `unit_artifact_root` in the final result commits the executed
artifact catalog. Generated capacity may lower a per-unit/per-chunk bound but
cannot add a complete-job cap or change graph formulas without a new bundle.

## 4. Phase contracts

All phase output bytes start with the frozen `WorkOutputHeaderV1`. The
phase-specific nested schemas live in one machine-readable
`LysisWorkOutputV1` registry. Its hash is included in both
`lysis_program_semantics_hash` and `object_codec_registry_hash`; there is no
runtime registry or dynamic dispatch.

### 4.1 `ENUMERATE`

Inputs, in exact position:

1. `INPUT_MANIFEST/AUTHENTICATED_ROOT`;
2. `TRIBUTE_STREAM/AUTHENTICATED_ROOT`.

Output records:

```text
EnumeratedTributeV1 {
  raw_ordinal: u32,
  tribute_id: EntityId36,
  owner: Address20,
  wwd: u32,
  nominal_amount_minor: U256,
  tribute_price_minor: U256,
  issuance_currency: u16,
  reference_currency: u16,
  exclude_from_intex_issuance: bool
}
```

The unit re-decodes canonical bodies, validates its half-open range and emits
gap-free source ordinals. Source and output coverage roots are equal.

### 4.2 `FIDELITY_MAP`

Inputs:

1. matching `ENUMERATED_TRIBUTES/UNIT_OUTPUT`;
2. `FIDELITY_OPENINGS/AUTHENTICATED_ROOT`.

For every Tribute it independently derives both logical Fidelity observations
used by legacy Lysis from the raw request-pinned opening:

```text
FidelityObservationV1 {
  raw_ordinal: u32,
  tribute_id: EntityId36,
  pre_distribution_league: u16,
  issuance_league: u16,
  nominal_amount_minor: U256
}

FiPartialV1 {
  ordered_observations: Vec<FidelityObservationV1>,
  ordered_league_partials: Vec<(league_id:u16, count:u32, nominal:U256)>,
  checked_total_nominal: U256
}
```

One owner's complete opening is consumed by one unit. Current Tribute identity
makes owner unique within WWD, but the verifier enforces this rule rather than
assuming it from chunk boundaries.

Its interval is `FidelityIndexHalfOpenRange{start=256*j,
end=min(256*(j+1),T)}`. The canonical Fidelity opening index is the
`raw_ordinal` of the unique Tribute obtained after mapping each owner opening
back to its Tribute. The planner and verifier reject missing, duplicate or
non-gap-free indexes; worker chunking or database order never defines them.

### 4.3 `FIXED_REDUCE`

Inputs are exactly two `FIDELITY_PARTIALS` producer/empty refs for a non-root
node, or two prior reducer outputs for later levels.

Each node checked-adds counts/totals and merges strictly ordered league
partials. The root additionally executes the exact legacy
`compute_fi_fraction_map` arithmetic and emits:

```text
FiFractionV1 { league_id:u16, fraction_fp:U256 }

FiFractionTableV1 {
  tribute_count: u32,
  total_nominal: U256,
  ordered_fractions: Vec<FiFractionV1>
}
```

The last truncated share is assigned to the highest sorted league exactly as in
the baseline. All wrapping/checked/division points come from ticket #3; no
floating point is introduced.

### 4.4 `AMOUNT_MAP`

Inputs:

1. matching `ENUMERATED_TRIBUTES/UNIT_OUTPUT`;
2. root `FI_FRACTION_TABLE/UNIT_OUTPUT`;
3. `ORACLE_OPENINGS/AUTHENTICATED_ROOT`.

Output:

```text
AmountRecordV1 {
  raw_ordinal: u32,
  tribute_id: EntityId36,
  owner: Address20,
  wwd: u32,
  league_id: u16,
  nominal_amount_minor: U256,
  gratis_fraction_fp: U256,
  gratis_load_minor: U256,
  entry_price_minor: U256,
  floor_price_minor: U256,
  cost_amount_minor: U256,
  issuance_currency: u16,
  reference_currency: u16,
  exclude_from_intex_issuance: bool
}

AmountRunV1 {
  ordered_records: Vec<AmountRecordV1>,
  checked_segment_gratis_total: U256
}
```

Oracle values are reconstructed from raw pinned openings. The unit performs no
live `get_pair_id`, VWAP or S-curve call.

### 4.5 `GRATIS_PREFIX`

Leaf units read one `AMOUNT_RECORDS/UNIT_OUTPUT`. Internal units read at most
two child summaries in fixed left/right order:

```text
GratisSegmentSummaryV1 {
  checked_segment_gratis_total: U256,
  first_non_positive_load_ordinal: Option<u32>,
  coverage_root: Hash
}
```

The root summary proves the exact checked total/coverage without reading all
segments in one unit.

### 4.5.1 `GRATIS_PREFIX_DOWN`

The root starts with the frozen `lysis_budget`. Each top-down unit reads one
parent prefix and at most two child summaries, then emits the exact incoming
remaining amount for each child. A leaf emits:

```text
GratisLeafPrefixV1 {
  segment_ordinal: u32,
  incoming_remaining: U256,
  outgoing_remaining: U256,
  first_error_ordinal: Option<u32>,
  coverage_root: Hash
}
```

For a successful result `first_error_ordinal` must be `none`. A semantic failure
is retained as local evidence but produces no `LysisResultV1` and no
signature.

### 4.6 `OUTPUT_FINALIZE`

Inputs:

1. matching `AMOUNT_RECORDS/UNIT_OUTPUT`;
2. matching leaf `GRATIS_PREFIX_DOWN/UNIT_OUTPUT`.

Starting from the segment's committed incoming amount, the unit replays the
legacy sequential consumption and emits:

```text
FinalizedOutputRecordV1 {
  raw_ordinal: u32,
  nod_action: NodActionV1,
  contributor_action: Option<ContributorActionV1>,
  bucket_key: Hash
}
```

There is exactly one Nod action per Tribute. Contributor action is absent only
for `exclude_from_intex_issuance=true`. IDs, prices, logical issue time and
bucket key are pure functions of pinned inputs.

### 4.7 `OWNER_SHUFFLE`

Inputs are every `FINALIZED_OUTPUT_RECORDS/UNIT_OUTPUT` in range order.

It filters present contributor actions and stable-sorts by
`(owner, raw_ordinal, source_tribute_id)`. Current uniqueness means aggregation
usually has one record per owner; checked aggregation is still performed and
the final canonical output is ordered by `(owner, source_tribute_id)`.

The artifact carries both raw-ordinal coverage and the eligible-subset
commitment, so omission cannot masquerade as an excluded Tribute.

### 4.8 `BUCKET_SHUFFLE`

Inputs are the same finalized outputs. It stable-sorts bucket records by
`(bucket_key, raw_ordinal)` and emits the exact grouped bucket leaves used by
`bucket_root`. The complete raw-ordinal coverage must equal the source.

### 4.9 `ROOT_REDUCE`

Leaf units consume one finalized-output/result chunk plus the matching
owner/bucket commitments. Internal units consume at most two child root
artifacts. All inputs are position-bound by the derived `UnitSpecV1`.

Across the fixed tree the phase:

- verifies producer artifacts and gap-free source/output coverage;
- streams the ordered unit-artifact and result-chunk catalog roots;
- constructs output roots, counts and conservation totals;
- reconstructs the semantic event summary;
- computes the arithmetic commitment;
- emits the constant-size canonical `LysisResultV1` only at the root.

The result's `unit_artifact_root` excludes the final `ROOT_REDUCE` carrier to
avoid a self-reference. `PlanHash` still commits its derivation rule and primary
unit catalog.

## 5. Scheduler and local verification

The supervisor scheduler owns only operational state:

```text
PENDING -> LEASED(slot, lease_generation) -> VERIFIED
              | crash/timeout/malformed
              v
            PENDING
```

It may choose any ready unit, run 1, 2 or 4 workers, kill a worker and retry the
same `UnitId`, or consume an already present artifact. It may never create a
spec not derived from `PlanCommitmentV1`, split a unit, merge units
opportunistically or mark a different unit complete.

Before a producer artifact becomes ready, the supervisor verifier:

1. loads the exact plan and locates `UnitId`;
2. canonical-decodes `UnitArtifactV1` under phase caps;
3. checks bundle, `JobId`, attempt, phase and interval commitment;
4. resolves every source by authenticated root, producer `UnitId` or canonical
   empty ID;
5. recomputes input root, output digest and coverage commitment;
6. reruns the pure phase verifier from producer inputs;
7. requires byte equality with the supplied output;
8. publishes/adopts by transport digest only after all checks.

Two different byte-valid artifacts for one `UnitId` are a deterministic
mismatch. Neither is selected by majority or completion time; the local domain
abstains and retains only digests, counts, phase and failure code in normal
diagnostics.

The reference implementation is the isolated test-only Rust crate selected by
ticket #3. It has no dependency on production Lysis, Outbe domain crates,
Alloy or shared arithmetic helpers. It consumes the canonical semantic corpus,
not production artifacts or encoders, and must match final actions, totals,
first error and digest vectors before OCOMP signing is enabled in a build.

The retry model follows the useful part of
[MapReduce](https://research.google.com/archive/mapreduce-osdi04.pdf): stable
logical work identities and deterministic reduction make re-execution harmless.
The PoC does not import a MapReduce framework.

## 6. Result reconstruction at the trust boundary

The supervisor cannot choose an arbitrary digest or signing purpose. After its
compute domain has verified complete chunk coverage, it sends
`RequestAttestationV1` with constant-size `LysisResultV1`.

The node `OcompAttestationGate` independently:

1. verifies the UDS peer/capability/session;
2. rejects frame/result caps before allocation or cryptography;
3. reloads the canonical `JobIntentV1` and exact attempt;
4. requires the request block and `JobId` to be finalized and non-orphaned;
5. requires the job to be live and latest finalized height to be below the
   exclusive deadline;
6. checks bundle and job-pinned committee snapshot;
7. reloads the durable `EXPORTED` binding and requires
   `input_manifest_hash` equality;
8. canonical-decodes and re-encodes the constant-size result commitment;
9. verifies `PlanHash`, non-empty result-chunk count/root, exact summary
   equations, job bindings, conservation and arithmetic/event commitments;
10. reconstructs `ActivationPayloadV1` and `ResultDigest`;
11. derives the one sign-once key from canonical state;
12. invokes the closed result signer/store operation.

Steps 8–10 are constant-size typed verification, not Lysis execution. The node
does not traverse result chunks: doing so would move bulk compute I/O back into
the consensus failure boundary. Complete chunk/input execution belongs to the
validator domain's separate compute plane; a compromised compute plane can
contribute at most that domain's one signature, and `q=3/4` supplies the
cross-domain correctness threshold. The gate never reads Fidelity/Oracle,
schedules work or recomputes leagues/prices/allocation.

The request has no arbitrary digest, purpose, domain, key selector, path or
message bytes. A claimed `ResultDigest`, if present for diagnostics, must equal
the node-derived value and is never the signer input.

## 7. Static PoC committee and keys

### 7.1 Static network and committee artifacts

`outbe-keygen` gains one narrow offline command for the fresh PoC devnet:

```text
outbe-keygen ocomp-result \
  --network-binding <base-genesis + fork + ProtocolBundleHash> \
  --validator-index <0..3> \
  --validator-address <address> \
  --consensus-public-key <canonical bytes> \
  --output-key <private envelope> \
  --output-registration <OcompKeyRegistrationV1 OCB1>
```

It creates a new secp256k1 key using OS CSPRNG, writes a typed owner-only
no-clobber secret artifact and emits public registration plus proof of
possession. Secret bytes never enter stdout, argv, logs, genesis or the
supervisor.

The PoC secret format is deliberately the existing minimal ECDSA file shape,
not a new keystore:

```text
/var/lib/outbe/ocomp-v1/keys/result-epoch-1.hex
bytes = 64 lowercase ASCII hex characters || LF
```

There is no `0x` prefix, JSON, embedded metadata or passphrase in the file.
Purpose, epoch, validator index, public key and network binding are verified
from the node configuration and `OcompKeyRegistrationV1`, not trusted from
secret-file metadata. `outbe-keygen` decodes the generated scalar before
publication, creates a mode-`0600` same-filesystem staging file with
`create_new`, writes and `sync_all`s it, publishes without replacement by hard
link, syncs the parent directory and removes/syncs staging. Existing secret
parsing/permission checks and `Zeroizing<[u8;32]>` handling are reused; the
existing overwrite-capable rename helper is not.

Startup rejects a symlink/non-regular file, owner other than the configured
node UID, any group/other permission bit, wrong length/case/newline, invalid
scalar or a public key that differs from the pinned registration. The key
directory is inaccessible to supervisor/exporter/worker/relay UIDs.

Generation is deliberately two-stage; putting a PoP that signs
`genesis_hash` inside the hashed genesis would be circular:

1. the base-genesis generator fixes `chain_id` and produces the immutable
   genesis header/hash;
2. the protocol-freeze generator produces `ProtocolBundleV1` and
   `ProtocolBundleHash`;
3. the network-binding artifact fixes that genesis hash, fork id/height and
   bundle hash;
4. four normal keygen invocations derive `ValidatorIdentityHash`, generate
   distinct OCOMP keys and sign registrations bound to that network binding;
5. the final-manifest generator verifies unique indexes `0..3`, identities,
   compressed keys and every PoP, fixes `key_epoch=1`,
   `allowed_purpose=RESULT_SIGNATURE`, `valid_from_height=32` and
   `valid_until_height_exclusive=u64::MAX`, then constructs threshold-3
   `OcompCommitteeSnapshotV1` with `snapshot_epoch=1`;
6. the final chain manifest statically maps fork height `32` to the exact
   `(ProtocolBundleHash, OcompCommitteeSnapshotHash)` and carries the public
   snapshot; every node and relay refuses a different final-manifest hash.

The snapshot is static consensus configuration outside the genesis state; the
base genesis header/hash is not regenerated in step 6. `ProtocolBundleV1`
does not contain the committee hash, while the snapshot does bind the bundle
hash, so neither artifact depends on its own hash.

Key generation always uses OS CSPRNG in release commands. Golden-vector and
reference-E2E reproducibility use an explicit test-only fixed-key fixture
generator that is absent from release binaries.

Intent admission pins that snapshot hash and requires the whole job deadline to
fit its validity interval. Runtime key registration/rotation/revocation is not a
PoC feature, and there is no public registration API.

### 7.2 Node signer

The node-only `OcompResultSigner`:

- accepts only a derived 32-byte `ResultDigest`;
- uses workspace `k256` secp256k1 prehash signing;
- uses deterministic RFC 6979 nonce generation;
- normalizes to low `s`;
- emits exactly `r[32] || s[32]`, with no recovery byte;
- verifies its own output against the loaded 33-byte compressed public key;
- refuses startup/signing if key purpose, epoch, permissions, public key,
  validator identity or committee index differs.

The existing EVM signer type is not reused because it carries a different key
purpose, address identity and recoverable transaction-signature format.
Deterministic ECDSA is pinned to
[RFC 6979](https://datatracker.ietf.org/doc/html/rfc6979).

## 8. Durable sign-once store

### 8.1 Storage model

The PoC “journal” is a directory of immutable OCB1 `SignOnceRecordV1` files,
not JSONL:

```text
/var/lib/outbe/ocomp-v1/sign-once/
  records/<first-two SignOnceSlot hex>/<remaining hex>.ocb1
  staging/<SignOnceSlot hex>.<boot-id>.<counter>.tmp
  quarantine/
```

The final path is derived only from:

```text
SignOnceSlot =
  H("OUTBE_OCOMP_SIGN_ONCE_SLOT_V1",
    chain_id || RESULT_SIGNATURE || JobId || attempt)
```

The record value binds bundle, committee snapshot, epoch, derived digest and
deterministic signature. Filename lookup is local; the complete record is
canonical protocol evidence.

### 8.2 Write-before-release protocol

Under a per-slot lock, the gate:

1. opens an existing final record, if any, and fully verifies canonical bytes,
   slot, key binding and signature;
2. returns its signature only when every value equals the new derived value;
3. returns `EQUIVOCATION_REFUSED` when the slot exists with a different value;
4. otherwise derives the deterministic signature in memory;
5. creates a same-filesystem staging file with `create_new`, mode `0600`;
6. writes exact OCB1 bytes and calls `sync_all`;
7. atomically publishes without replacement using a same-filesystem hard link
   from staging to the final path;
8. `sync_all`s the final parent directory;
9. only now returns the signature;
10. unlinks the staging name and syncs the staging directory as cleanup.

A hard-link publish is selected because ordinary Unix rename may overwrite an
existing final record. Startup verifies that staging and final paths share a
filesystem and that the configured store supports hard links; otherwise the
OCOMP signer remains unavailable. No privileged helper or unsafe syscall is
needed.

Recovery rules:

| State after restart | Rule |
|---|---|
| no final record, orphan staging only | no signature was released; verify then remove staging |
| valid final record, staging absent/present | final binding is authoritative; cleanup staging |
| final record non-canonical, wrong slot/key/signature or non-regular | quarantine and disable all OCOMP signing |
| I/O/fsync/link outcome uncertain in the running process | release nothing; mark signer unavailable until reopen/reconciliation |
| exact final record | exact retry returns stored signature |
| conflicting final record | permanent refusal for that slot |

JSONL operational journals are explicitly forbidden for this authority because
their current implementations swallow errors and do not fsync.

The store is never automatically reset. Loss, permission drift or ambiguity
sets `ocomp_signing_ready=false` while consensus remains available.

## 9. Certificate verification

`ExecutionCertificateVerifierV1` is one pure implementation in
`outbe-ocomp-protocol`, used by relay prefiltering and consensus activation.
Consensus execution remains the authority.

Verification order is:

1. cap and canonical-decode result/certificate before cryptographic work;
2. reload the job-pinned `OcompCommitteeSnapshotV1` and verify its hash;
3. reconstruct result structure, `ActivationPayloadV1` and `ResultDigest`;
4. require certificate digest equality;
5. require threshold exactly 3 and committee length exactly 4;
6. require bitmap high bits zero and population exactly 3;
7. require exactly three signatures, indexes ascending and equal to bitmap bits;
8. reject duplicate/unknown indexes and invalid purpose/epoch/validity/PoP;
9. parse compressed public keys and raw `r||s`;
10. reject zero/out-of-range scalar and any high-`s` signature;
11. verify all three signatures over the exact digest.

Exactly four signatures is non-canonical and rejects. When four valid
announcements exist, relay deterministically chooses the three lowest validator
indexes.

Two 3-of-4 sets intersect in at least two indexes. With at most one Byzantine
domain, at least one intersection signer is honest and sign-once prevents two
different result certificates for the same job attempt.

## 10. Relay contract

The replaceable relay exposes only:

```text
POST /v1/candidates
Content-Type: application/octet-stream
body: exact OCB1 CandidateAnnouncementV1

GET /healthz
```

The generated constant-size candidate cap bounds the HTTP body before
allocation. Success is `202`; exact duplicate is idempotent `200`; malformed,
unauthorized-chain, over-cap or invalid-signature input is rejected. HTTP status
has no protocol authority.

For each announcement the relay:

1. canonical-decodes `LysisResultV1` and recomputes payload/digest;
2. loads the exact public job/committee snapshot;
3. verifies validator index, epoch and signature;
4. groups by `(protocol_bundle_hash, JobId, attempt, ResultDigest,
   exact canonical result-commitment bytes)`;
5. deduplicates validator index;
6. at three valid matches, selects indexes ascending and builds the certificate;
7. independently builds the finalized intent proof;
8. submits `activateLysis(bytes)` with a normal EVM transaction payer key.

The payer key is not an OCOMP/consensus protocol key and grants no authority.
Any other client can submit the same bytes.

The relay needs no new custom public proof RPC for the PoC:

- `outbe_getFinalization(height)` supplies exact Commonware finalization and
  block bytes;
- `eth_getBlockByHash` supplies the canonical Ethereum header view;
- standard `eth_getProof` with exact EIP-1898
  `{blockHash,requireCanonical:true}` supplies the bounded intent and historical
  committee account/storage proofs;
- the fresh-devnet node retains the 64-block request/deadline/evidence window.

The relay verifies every returned proof and canonical byte sequence. A release
gate must demonstrate exact block-hash historical `eth_getProof` on the pinned
Reth v2.2.0 build; an unavailable proof makes that relay wait/fail and does not
authorize a local/private proof substitute.

Relay loss is harmless. It may keep only a bounded one-job cache. Mixed,
reordered or dropped announcements can delay activation but cannot create a
certificate. There is no “result accepted” transaction and no relay-to-node
signing API.

## 11. Required deterministic and adversarial vectors

### 11.1 Plan/work vectors

The protocol-freeze generator must add:

- every valid phase/interval/input-purpose combination;
- invalid phase/interval and input-position combinations;
- `T=1,255,256,257` source partitions, with `257` proving that the first
  record after a full shard belongs to shard 2;
- synthetic `T=10,000` and `T=1,000,000,000` commitments deriving exactly 40
  and 3,906,250 primary units without allocating proportional vectors;
- Fidelity/prefix/root reducer `K=1..8`, including every padded-empty shape and
  both prefix scan directions;
- exact `PlanHash`, primary work-unit root and sampled/boundary `UnitId`s from
  every derived phase;
- every work-output nested schema, empty/non-empty subset and maximum shape;
- duplicate/gap/out-of-range raw ordinal;
- missing/duplicate/replaced producer `UnitId`;
- wrong source root/count/cap;
- changed output byte, semantic digest, input root, interval or coverage;
- owner/bucket order and coverage/permutation mutations;
- prefix zero-load, exact exhaustion, first over-budget ordinal and `U256::MAX`
  arithmetic cases.

Production and independent test encoders must agree. The independent semantic
reference does not call production phase functions.

### 11.2 Schedule vectors

For one exact manifest:

- run with 1, 2 and 4 workers;
- deterministic schedule seeds covering forward, reverse and shuffled ready
  queues;
- kill before read, mid-compute, after staged write and before supervisor adopt;
- duplicate exact completion;
- stale lease/session completion;
- retry on a different slot/process;
- restart supervisor with verified CAS artifacts.

Every run must produce identical plan bytes, ordered unit/result-chunk roots,
`LysisResultV1` and `ResultDigest`. A retry count or PID must not appear in
semantic bytes.

### 11.3 Attestation/store vectors

- nonfinal, orphaned, wrong attempt, completed, expired and wrong-bundle job;
- missing/wrong exported manifest binding;
- malformed/activation-summary-cap+1 result before key use;
- digest-only/generic-purpose request is structurally impossible;
- exact request twice returns byte-identical signature;
- concurrent exact requests create one final inode/record;
- concurrent different results yield one durable winner and one refusal;
- conflicting result before and after node restart;
- fault after create/write/file-fsync/link/directory-fsync/response boundary;
- orphan staging recovery, corrupt final record, unsafe permissions and quota
  exhaustion;
- wrong local key/index/epoch/public fingerprint;
- RFC6979/low-`s` golden signatures.

No failed case may create a released signature.

### 11.4 Certificate/relay vectors

- all four valid 3-index subsets: `012`, `013`, `023`, `123`;
- two signatures, four signatures, duplicate index, unknown index;
- wrong bitmap population/high bits/order;
- wrong key epoch/purpose/committee snapshot;
- invalid compressed key, zero/overflow `r/s`, high `s`;
- valid signature over a different digest;
- mixed result bytes, claimed digest mutation and `JobId` mutation;
- duplicate/reordered/dropped announcements;
- four valid announcements select indexes `0,1,2`;
- finalization/header/state-proof mutation;
- relay restart/loss followed by resubmission from public announcements.

The public activation rejection/state-diff evidence belongs to tickets #8/#9;
this ticket fixes the vectors and pure verifier outcomes.

## 12. Code ownership without extra framework

The implementation task graph must place code as follows:

```text
crates/system/ocomp-protocol
  OCB1 objects/hash domains
  UnitSpec/input/output nested codec registry
  pure result/certificate/key-registration verifiers

crates/core/lysis/src/program_v1/
  input normalization
  exact planner
  phase execute/verify functions
  result reconstruction
  no StorageHandle, node, UDS, CAS or signer

bin/outbe-ocomp
  CAS adapters
  scheduler journal and worker role
  supervisor orchestration
  HTTP relay adapter

crates/blockchain/node/src/ocomp/
  OcompControl server integration
  OcompAttestationGate
  OcompResultSigner
  immutable SignOnceStoreV1

bin/outbe-keygen
  offline OCOMP key/registration artifact command

crates/testing/e2e-harness
  schedule/fault/restart/public-relay evidence controls
```

There is no `Program` trait, registry, task adapter, generic signer, generic
write set, central calculator or one crate per phase.

## 13. Failure semantics

| Failure | Required result |
|---|---|
| worker/supervisor crash | retry exact UnitId or local abstention; consensus unchanged |
| two local artifacts for one UnitId | retain digest mismatch; abstain; never choose by majority |
| semantic failure/first error | no successful result/signature |
| one domain unavailable | other three may form exact q=3 |
| two domains unavailable | no threshold lowering; ticket #8 expiry |
| one domain computes a different digest | separate relay group below q; no signature rewriting |
| signer store unavailable/ambiguous/full | that domain does not sign; consensus key unaffected |
| relay corrupts/mixes/drops | verification rejects or activation delays |
| public historical proof unavailable | relay waits/fails; no private bypass |

No failure invokes on-chain/synchronous Lysis.

## 14. Blocking completion evidence for ticket #7

Later implementation is complete only when retained evidence proves:

1. all four domains independently produce the same plan/result without sharing
   pre-signing artifacts;
2. 1/2/4 workers, schedule permutations and kills produce identical bytes;
3. every phase artifact is producer-bound and coverage-complete;
4. native output matches the independent corpus at all mandatory edges;
5. each validator identity has one distinct OCOMP key/index and epoch-1 PoP;
6. the node, not supervisor/worker, derives and signs the digest;
7. exact sign retry is idempotent and conflicting retry refuses after restart;
8. four certificate subsets pass and all signer/digest mutations fail;
9. a stopped fourth supervisor is absent from the successful three-signature
   certificate;
10. relay builds finality/state proof from public exact-block data and submits
    through the normal transaction surface;
11. no test counts workers as voters or uses a central result;
12. logs/evidence contain public bytes/digests and failure codes, never private
    key bytes.

Planned command surfaces are:

```text
cargo test -p outbe-ocomp-protocol --test plan_and_signature_vectors
cargo test -p outbe-lysis --test program_v1_differential
cargo test -p outbe-ocomp --test deterministic_schedules
cargo test -p outbe-node --test ocomp_sign_once_fault_matrix
cargo test -p outbe-ocomp-protocol --test certificate_mutations
cargo run -p outbe-e2e-harness --bin outbe-e2e -- --tags @ocomp-q3
```

They are planned interfaces, not claims that implementation exists.

## 15. PoC-to-BoundedMVP seam

BoundedMVP may harden key custody, add crash-safe multi-job scheduling, use a
launch broker, make relay redundant and improve mismatch diagnostics. It does
not change:

- authenticated input and `JobId`;
- plan/unit producer binding and deterministic reducer;
- one result vote per validator index;
- node-derived closed-purpose signature and sign-once subject;
- `ResultDigest`, committee snapshot and certificate meaning;
- untrusted public activation submission.

Key rotation/revocation requires a new pinned committee snapshot/epoch for new
jobs; live jobs finish or expire under their original snapshot. Proof-carrying
TargetLarge evidence is a separate protocol profile.

## 16. Why no grilling was needed

The sources already fix the meaningful product choices: one Lysis program,
static four-member epoch-1 committee, deterministic execute-and-attest,
`q=3`, complete result, no on-chain recomputation and a replaceable relay.

The remaining questions were implementation contradictions or derivations:

- downstream specs could not bind unknown future roots, so they now bind
  producer `UnitId`s;
- the result artifact root would be self-referential if it included the final
  carrier, so it excludes that one carrier while `PlanHash` still commits it;
- existing best-effort JSON journals cannot enforce write-before-sign, so the
  selected store uses immutable fsynced records and no-clobber publication.

These are correctness closures, not new product scope or ubiquitous-language
changes.
