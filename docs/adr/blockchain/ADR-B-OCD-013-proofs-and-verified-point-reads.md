# ADR-B-OCD-013: Serve independently verified point reads from one finalized CE snapshot

- **Status:** Proposed; migrated design, current implementation evidence requires reconciliation
- **Date:** 2026-07-19
- **Depends on:** ADR-B-OCD-012

## Context

ADR-B-OCD-010 commits every Tribute, Nod item, and Nod bucket body through its collection shard set and the Root Catalog into one `R_sealed`. ADR-B-OCD-011 can retire a complete Tribute WWD by deleting its Root Catalog leaf and finalized shard namespaces. ADR-B-OCD-012 places the execution-computed post-state `{commitment_scheme_version, R_sealed}` in tag `0x08` of every post-genesis block header.

The header root is an external trust anchor only after the selected block is finalized. By itself it does not let an external user verify a MongoDB body or authenticated absence. The user needs the exact Merkle evidence connecting an independently specified compressed-entity identity to that finalized root.

The finalized CE MDBX materialization is intentionally in-place. It retains one latest root-verified finalized tree, not a block-addressed archive of every prior tree version. MDBX MVCC nevertheless gives every already-open read transaction one immutable self-consistent snapshot while later finalized blocks commit concurrently. ADR-B-OCD-013 uses that property to issue a portable proof package for the snapshot selected at request time. Once issued, the package remains valid forever for that block, even after the local in-place tree advances; only its claim of freshness ages.

ADR-B-OCD-013 implements the point-proof package and current verified point-read contract described in `compressed_entities_concept_v6_proposed_10-07-2026.md` sections 10.2–11.1. It does not replace the pinned CKB SMT, add tree versioning, or promise later on-demand proof generation for an arbitrary old block.

## Starting system

After ADR-B-OCD-012:

- every block `B >= 1` carries tag `0x08` with the fork-selected scheme and post-state `R_sealed(B)`;
- execution and validators independently require that header root to equal `SealOutput(B).new_root` and post-state `0xEE0D.slot1`;
- finalized CE MDBX atomically stores the current collection-shard trees, Root Catalog tree, namespace roots, and complete `last_applied` marker;
- one MDBX transaction observes either the complete old finalized materialization or the complete new one;
- each collection shard and the Root Catalog use the pinned vendored CKB/Poseidon proof semantics from ADR-B-OCD-008;
- shard selection, the fixed `K_domain`, collection-top aggregation, collection roots, and `R_sealed` wrapping are fork-deterministic under ADR-B-OCD-009 and ADR-B-OCD-010;
- MongoDB stores canonical `StoredBody` data and finalized secondary-index projections, but is not an authenticated authority;
- canonical mutation events retain exact body payloads and commitment metadata for replay;
- no public service returns a portable entity membership/non-membership proof bound to the finalized header.

## Added capability

A node serves one independently verifiable point-read package for a Tribute, Nod item, or Nod bucket. The package binds the requested identity and an optional canonical body to one frozen finalized CE MDBX snapshot and carries the complete shard, collection-top, and Root Catalog evidence needed to recompute the `R_sealed` found in the selected finalized header.

The client verifies the package locally. It derives identity, collection, key, shard, leaf, path directions, collection root, Catalog root, and sealed root itself. The node supplies body bytes and cryptographic evidence, not trusted derived answers.

## Decision

### Exact security claim

For independently supplied expected identity:

```text
ExpectedEntity = (domain_id, raw_entity_id_36)
```

and a finalized block `H`, successful verification means:

```text
VerifyPointPackage(ExpectedEntity, Header(H), Package(H)) = true
```

only if all of the following hold:

1. `Package(H)` identifies the exact expected chain, block, domain, and EntityId36;
2. the client verifies that `Header(H)` is the canonical finalized header under its chain/light-client trust model;
3. `H >= 1` and the client extracts the fork-active scheme and `R_sealed(H)` from mandatory tag `0x08`;
4. the body, when present, strict-decodes and canonicalizes under the fork-active domain/schema rules and recomputes the authenticated non-zero leaf;
5. the shard CKB proof authenticates that leaf, or authenticated zero absence, at the independently derived `tree_key`;
6. the fixed collection-top path authenticates the resulting shard root under the independently derived shard index and fork-fixed `K_domain`;
7. the Root Catalog CKB proof authenticates the resulting `R_collection` at the independently derived `collection_key`, or authenticates that the collection itself is absent;
8. the client recomputes the Catalog root and `R_sealed` and obtains exactly the root from the finalized header.

A successful present package therefore proves that the exact canonical body was committed at the requested identity in finalized block `H`. A successful absent package proves only that the independently derived identity was absent in that selected state.

The proof does not:

- establish that `H` remains the latest finalized block when verification occurs;
- re-execute or independently validate the business authorization that produced the state transition;
- prove completeness, ordering, or continuation of a secondary-index list;
- recover body bytes that no provider possesses;
- distinguish a never-populated collection from a retired collection when both are absent from the Root Catalog;
- make an unverified header finalized merely because its root matches the proof.

Consensus finality and deterministic execution establish transition validity. ADR-B-OCD-013 establishes point-record integrity relative to that finalized transition.

### Point-read request and block selection

The v1 point request is conceptually:

```text
PointReadRequestV1 {
    domain_id: u16,
    raw_id: EntityId36,
}
```

`domain_id` must be one of the fork-active closed domain IDs. `raw_id` is exactly 36 bytes. Tribute derives its WWD partition from `raw_id[0..4]`; NodItem and NodBucket are singleton domains. The caller does not supply a block, collection key, tree key, shard, `K`, commitment, schema, root, or path direction.

The node selects the latest root-verified finalized CE MDBX marker visible when the one proof transaction opens. This may be behind a separately observed network tip while the local node is catching up; the response always states the selected block explicitly, and freshness remains client policy. V1 has no caller-selected block and no historical block selector.

The proof RPC serves only `H >= 1`, where tag `0x08` is mandatory. If the latest visible marker is genesis height `0`, the service is not ready and returns no proof package. V1 has no genesis-specific chain-spec verification path.

The request has no operator-selected commitment topology or proof algorithm. The fork schedule at the selected height determines the active domain policy, `K_domain`, commitment scheme, and accepted body schemas. The response echoes required metadata only so the client can bind and reject it; it does not negotiate those values.

### One request uses one CE MDBX read transaction

Every successful proof package is assembled from exactly one CE MDBX read transaction:

1. open one MDBX read transaction;
2. read and validate the complete `last_applied` marker:

   ```text
   { commitment_scheme_version,
     height = H,
     block_hash,
     parent_block_hash,
     parent_root,
     new_root = R_sealed(H) }
   ```

3. read the Catalog root and recompute the scheme-bound `R_sealed` wrapper;
4. derive the domain, partition, `collection_key`, `id_f`, final `tree_key`, and shard index from the request and fork schedule;
5. read the Root Catalog leaf/proof from that transaction;
6. when the collection is present, read exactly its fork-fixed `K_domain` shard roots, recompute `R_collection`, derive the selected shard, and build the entity shard proof and collection-top siblings;
7. copy the complete immutable marker/proof/body-commitment evidence needed for the response;
8. never reopen CE MDBX or mix a cached node, root, sibling, or proof from another transaction;
9. close the transaction immediately after the tree evidence has been assembled;
10. only then perform the bounded MongoDB body lookup for a non-zero leaf.

The MongoDB body lookup is not part of MDBX atomicity. Closing the read transaction before Mongo I/O avoids retaining old MDBX pages while a storage/network read is slow. The immutable marker, leaf, and proof evidence already bind the response to `H`, so no second CE transaction is needed or permitted.

Mongo may advance or change the requested body after the CE transaction closes. ADR-B-OCD-013 adds no cross-database snapshot, lock, fence, or automatic retry to eliminate that race. The returned bytes are accepted only when they independently recompute the frozen leaf; otherwise the request returns `unavailable` and a caller may issue a new request.

An MDBX writer may commit `H+1`, `H+2`, or a Tribute retirement while the request is active. MVCC keeps all reads from the open transaction on the complete `H` materialization. The writer does not wait for proof logic except for ordinary MDBX reader/page-retention effects, and the proof does not observe a mixture of heights.

ADR-B-OCD-011 immediate finalized namespace reclamation remains valid under this latest-snapshot contract:

- a reader opened before retirement continues to see the old collection/shard pages and can finish the proof for its selected pre-retirement `H`;
- a reader opened after retirement proves Root Catalog absence in the new state and never opens the removed shard namespaces.

This property is not historical version retention. After the pre-retirement transaction closes, a future request cannot regenerate that old proof from the in-place tree.

### Snapshot and canonical-header binding

Before returning a successful package, the proof service requires `H >= 1`, requires the selected marker to identify a canonical finalized block available through the local provider, and requires its mandatory tag `0x08` scheme/root to equal the marker.

The per-request service does not repeat the historical EVM-slot check already required by ADR-B-OCD-012 finalized persistence. ADR-B-OCD-014 later defines exhaustive restart/readiness reconciliation among canonical headers, historical EVM state, CE MDBX, and Mongo checkpoints. Until that work is implemented, any marker/header mismatch disables proof readiness and fails closed; it never returns a package and never affects network block validity.

The package identifies only the selected chain and block:

```text
chain_id
block_number
block_hash
```

It does not echo `commitment_scheme_version` or `R_sealed`. The verifier obtains both only from tag `0x08` in the independently selected header and the fork schedule, then compares the root recomputed from the package evidence directly with that header root.

The package also contains no Simplex certificate, committee transition, finalized-header chain, or other finality evidence. ADR-B-OCD-013's pure verifier accepts a header whose finality the caller has already established through the chain's separate light-client/consensus mechanism. Point-state proof validity and consensus finality validity remain separate checks.

The client:

1. requires the package chain ID to equal its expected chain;
2. obtains the exact header by `block_hash` from any provider or local archive;
3. requires the header number to equal `block_number`;
4. verifies finality independently;
5. decodes tag `0x08` through the standard OART codec;
6. requires the header scheme to equal the fork schedule;
7. uses that scheme for every body/tree/top/Catalog hash and requires the final recomputed root to equal the header `R_sealed`.

A malicious node may return a valid package for an old finalized block. That is a stale but cryptographically valid statement, not a forged one. The client decides freshness separately, for example by requiring:

```text
trusted_finalized_tip - package.block_number <= client_delta
```

No protocol-wide `client_delta` is defined. A package remains valid forever as evidence of block `H`; it does not remain a claim about current state.

### Canonical package and proof variants

The v1 result is conceptually one closed typed union:

```text
PointReadResultV1 =
    Present {
        common: PointProofCommonV1,
        body_bytes: bytes,
        evidence: PresentEvidenceV1,
    }
  | Absent {
        common: PointProofCommonV1,
        evidence: AbsentEvidenceV1,
    }
  | Unavailable

PointProofCommonV1 {
    proof_encoding_version: u32,

    chain_id: u64,
    block_number: u64,
    block_hash: B256,

    domain_id: u16,
    raw_id: EntityId36,
}

PresentEvidenceV1 {
    shard_smt_proof: CkbCompiledProofV1,
    shard_top_siblings: [B256; log2(K_domain)],
    root_catalog_proof: CkbCompiledProofV1,
}

AbsentEvidenceV1 =
    CollectionAbsent {
        root_catalog_proof: CkbCompiledProofV1,
    }
  | EntityAbsentInCollection {
        shard_smt_proof: CkbCompiledProofV1,
        shard_top_siblings: [B256; log2(K_domain)],
        root_catalog_proof: CkbCompiledProofV1,
    }
```

There is no independent status field and no optional body. The type makes `Present` without body bytes, `Absent` with body bytes, and `Unavailable` with partial proof fields unrepresentable. The exact JSON-RPC field spelling and hex/binary adapter representation are pinned during the implementation seam review, but every adapter maps to this one typed model. No second proof model exists for Tribute, NodItem, or NodBucket.

The package carries no partition echo. Tribute derives its exact WWD partition from the first four `raw_id` bytes; singleton domains derive no partition. The verifier performs that derivation before computing `collection_key`, so the node cannot select or redirect the collection independently.

`body_bytes` is the exact byte-for-byte ADR-B-OCD-006 strict-canonical Protobuf `StoredBody` envelope stored in MongoDB:

```protobuf
message StoredBody {
  uint32 schema_version = 1;
  bytes payload = 2;
}
```

The package does not duplicate `schema_version` or the payload as sibling fields. The verifier strict-decodes the envelope, requires byte-exact canonical re-encoding, validates the typed payload's embedded identity/WWD, obtains `schema_version` and `canonical_payload`, takes `commitment_scheme_version` only from the finalized header/fork schedule, and recomputes:

```text
id_f   = PBytes(TAG_ID, EntityId36)
body_f = PBytes(TAG_BODY, canonical_payload)

leaf_f = P(
    TAG_LEAF;
    commitment_scheme_version,
    schema_version,
    id_f,
    payload_byte_len,
    body_f
)
```

The `Present` variant always carries non-empty canonical `body_bytes` and requires a non-zero canonical leaf. The `Absent` variant has no body field at all.

V1 carries no independent `hash_version`. ADR-B-OCD-006 defines all body, key, CKB MergeValue, collection, Catalog, and sealed-root hash semantics under `commitment_scheme_version`. Adding an independently evolving hash version without a real coexistence requirement would create ambiguous combinations. A future concrete format-evolution ADR may separate hash/proof/schema evolution when an actual post-production compatibility requirement exists.

`proof_encoding_version = 1` identifies the stable external proof package/CKB-proof transport. It is not the CKB vendor revision and does not select commitment semantics. Changing only transport framing may consume another proof encoding version; changing proof verification in a way that changes authenticated roots requires the fork-governed commitment scheme path.

### CKB proof encoding and independent verifier

The conceptual explanation of a Merkle proof is an ordered sibling path, but the pinned CKB engine uses compact-zero `MergeValue` semantics and compiled proof opcodes. ADR-B-OCD-013 does not replace that representation with an ad hoc array of hashes.

`CkbCompiledProofV1` wraps the canonical compiled single-key proof bytes produced by the vendored ADR-B-OCD-008 engine. Its opcode, `MergeValue`, zero-count, key-order, BE32 bridge, canonical-field, poison-rejection, and stack semantics are exactly the pinned CKB/Poseidon semantics. The external package adds explicit length-delimited framing and versioning; it does not reinterpret the proof program.

The verifier supplies the independently derived key and expected value to the existing stateless CKB verification function:

```text
verify_shard_proof(tree_key, expected_leaf_or_ZERO, shard_proof)
    -> shard_root

verify_catalog_proof(collection_key, expected_R_collection_or_ZERO, catalog_proof)
    -> catalog_root
```

It never accepts a node-supplied claimed shard or Catalog root without recomputation.

For `K_domain = 2^k`, `shard_top_siblings` contains exactly `k` canonical non-poison field values ordered from the shard-root level upward. At top level `level`:

```text
if bit(level, shard_index) == 0:
    current = P(TAG_TOP_NODE; level, current, sibling[level])
else:
    current = P(TAG_TOP_NODE; level, sibling[level], current)
```

The verifier derives `shard_index` from `tree_key` through ADR-B-OCD-009's pinned CKB bit mapping and derives every left/right choice from that index. A direction bitmap or caller/node-selected path is not transported. For provisional `K = 16`, the path has exactly four siblings.

After obtaining `shard_top_root`, the verifier derives:

```text
R_collection = P(
    TAG_COLLECTION_ROOT;
    commitment_scheme_version,
    collection_key_f,
    K_domain,
    shard_top_root
)
```

It supplies that value to the Root Catalog proof, obtains `catalog_root`, and derives:

```text
R_sealed = P(
    TAG_SEALED_ROOT;
    commitment_scheme_version,
    catalog_root
)
```

The final result must equal tag `0x08` in the independently finalized header.

The verifier is a small stateless library surface built from:

- the fork schedule/domain definitions required at block `H`;
- ADR-B-OCD-006 strict body decoding and commitment derivation;
- ADR-B-OCD-008 vendored CKB proof verification and `PoseidonCkbHasher`;
- ADR-B-OCD-009 shard selection and `TAG_TOP_NODE` folding;
- ADR-B-OCD-010 collection/Catalog/sealed-root derivation;
- ADR-B-OCD-012 OART tag `0x08` decoding.

It has no MDBX, MongoDB, Reth provider, cache, or mutable global state. Network/header acquisition and finality verification remain caller/light-client responsibilities; the pure verifier accepts the already selected header and expected entity identity.

### Present and absent proofs

The service has two authenticated success forms.

#### Present

A present Catalog leaf and non-zero shard leaf are required:

```text
RootCatalog[collection_key] = non-zero R_collection
CollectionShard[tree_key]   = non-zero leaf_f
```

The node loads the canonical body, validates domain/identity/schema, recomputes `leaf_f`, and requires exact equality with the shard leaf before returning the package. The client independently repeats the same computation.

#### Absent collection

If the Root Catalog proof authenticates:

```text
RootCatalog[collection_key] = ZERO
```

then the entity is absent for the selected root. The result uses `AbsentEvidenceV1::CollectionAbsent`, contains no body, shard proof, or shard-top siblings, and cannot distinguish `NeverPopulated` from `Retired` without separate domain state/history.

#### Absent entity in a present collection

If the Catalog leaf is present but the selected shard proof authenticates:

```text
CollectionShard[tree_key] = ZERO
```

then the result uses `AbsentEvidenceV1::EntityAbsentInCollection`, contains no body field, and carries the shard proof, collection-top siblings, and Catalog membership proof. This includes an entity absent from an `EmptiedByDelete` collection.

A node never interprets missing local branch/leaf records, a failed proof program, an incomplete shard-root vector, or a root mismatch as authenticated absence. Those are local materialization failures.

### MongoDB body verification and cross-store races

MongoDB is not inside the MDBX read transaction. The service therefore makes no cross-database snapshot claim.

For a non-zero authenticated leaf, the node performs one bounded Mongo point read and requires a canonical body whose recomputed leaf equals the frozen CE leaf. The proof RPC does not read, compare, wait on, or gate on the Mongo durable checkpoint. The source row's projection height is irrelevant to validity when its exact canonical value equals the leaf committed at `H`.

Mongo may advance or change the row after the CE transaction closes but before the body read completes. This creates an accepted v1 availability race but not a validity race:

```text
body at another state hashes to the same leaf:
  it is value-identical and valid for H under ADR-B-OCD-007 value-based equality

body missing, malformed, wrong-identity, non-canonical, unknown-schema,
or hashing to another leaf:
  return unavailable; never return present or absent
```

Mongo timeout/unavailability, a missing body, or body/leaf mismatch is exposed to the RPC caller as `unavailable` and recorded internally with its structured ADR-B-OCD-005, ADR-B-OCD-006 and ADR-B-OCD-015 classification. Deterministic local corruption may additionally disable proof readiness or trigger recovery/shutdown; it still never changes the finalized root or becomes a negative consensus vote. Mongo checkpoint comparison remains part of execution/projection readiness and ADR-B-OCD-014 reconciliation, not this proof RPC.

Authenticated tree absence does not require a Mongo lookup. A stale Mongo row cannot override a valid non-membership proof.

Canonical full-body events remain the recovery source for Mongo projection. ADR-B-OCD-013 does not itself fetch peers, replay receipts, or promise recovery before responding; the caller may query another node or retry after readiness returns.

### Public outcomes and invalid packages

The point-read service distinguishes:

```text
present
  a complete package with canonical body and membership evidence

absent
  a complete body-free package with authenticated collection or entity non-membership

unavailable
  this node cannot currently supply a complete matching body/proof package
```

Malformed request identity, unknown domain ID, and invalid fixed widths are invalid request errors rather than `absent`.

`unavailable` is a non-success result, not a partial proof variant. V1 returns no body, claimed leaf/commitment, shard proof, top siblings, Catalog proof, or independently verifiable state assertion with it. The caller retries or asks another node. This keeps exactly one package contract: a package is returned only for authenticated `present` or `absent`.

`unavailable` is not cryptographic absence. A malicious node may always refuse service or claim unavailability; the trustless property is that it cannot make an invalid `present` or `absent` package pass local verification.

A syntactically returned package that fails identity, body, proof, root, block, scheme, finality, or freshness checks is `invalid` at the client verifier. `invalid` is not a successful server outcome and must not be collapsed into `absent` or `unavailable` by client libraries.

### Freshness and long-lived package validity

The selected finalized state is frozen by the one MDBX transaction. Pauses between package creation, header retrieval, finality checking, and local verification do not affect cryptographic validity.

If the network advances from `H` to `H+n` while the request or verification runs:

```text
Package(H) still proves state at H.
Package(H) says nothing about state at H+n.
```

The user may retain the complete present package, including body bytes, and verify it again years later against the same finalized header. Tree pruning or later namespace retirement cannot invalidate an already issued package.

The in-place tree does not let a user first arrive years later, select an arbitrary old `H`, and require the node to regenerate the old proof. That stronger service contract would require versioned authenticated-tree persistence, retained logical snapshots, or another separately designed archive mechanism. It is deliberately outside ADR-B-OCD-013.

### Secondary-index and list boundary

ADR-B-OCD-013 authenticates one independently specified entity identity. It does not authenticate Mongo owner/day/global indexes, result ordering, omitted IDs, pagination continuation, or list completeness.

A list adapter may attach one valid point package to every returned entity. That proves each returned body individually but does not prove that the adapter returned every matching entity. No API or documentation may describe such a list as a complete authenticated query without a later authenticated-index design.

### Resource and denial-of-service bounds

One request performs bounded work determined by the active topology:

- one CE MDBX read transaction;
- one Root Catalog single-key proof;
- zero or one collection-shard single-key proof;
- exactly `K_domain` shard-root reads for a present collection;
- exactly `log2(K_domain)` top siblings;
- one bounded canonical Mongo point read for present state;
- one canonical body decode/commitment computation;
- bounded stateless proof verification by the client.

The proof decoder rejects oversized length prefixes, excessive CKB proof programs/stacks, the wrong top-sibling count, malformed canonical fields, poison values, unknown variants, and trailing bytes before expensive work. Structural maxima derive from two 256-level CKB single-key proofs, the fork-fixed `K_domain`, and the active body schema rather than operator-provided sizes.

ADR-B-CAP-001 measures actual proof bytes, Poseidon counts, MDBX reader lifetime, page retention, current proof latency under finalized writers, concurrent reader count, RPC body limits, and cache/memory envelopes. ADR-B-OCD-013 does not claim the conceptual `~2 KiB` or sub-millisecond estimates as protocol bounds before those measurements.

Proof generation is an RPC/read service, not consensus execution and not the Marshal ACK path. Resource exhaustion may reject or throttle local proof requests but cannot delay finalization through an implicit shared unbounded queue.

## Working result

After implementation:

- a caller requests a Tribute, NodItem, or NodBucket by typed domain ID and exact EntityId36;
- the node opens one root-verified finalized CE MDBX snapshot and returns one present or absent proof package, or an explicit non-success outcome;
- all shard, top, Catalog, marker, and root evidence in a successful package comes from that one snapshot;
- the node never returns a body whose strict canonical commitment differs from the authenticated leaf;
- the client independently derives every locator and commitment and verifies the complete hierarchy against tag `0x08` in a finalized header;
- concurrent finalized commits cannot produce a mixed-height package;
- a saved package remains verifiable indefinitely for its selected block;
- a stale package is rejected only by the client's freshness policy, not because its historical proof expired;
- unverified Mongo indexes remain ordinary projection queries with no completeness claim.

## Accepted limitations

- V1 guarantees on-demand proof generation only for the latest root-verified finalized CE MDBX snapshot selected by the node. The caller cannot select a block, and the RPC provides no arbitrary historical proof generation.
- MongoDB body availability is not created by the Merkle proof. A present commitment with no matching local body returns only `unavailable`, without partial commitment/proof evidence.
- MongoDB is read only after the CE MDBX transaction closes. No cross-store snapshot/fence is attempted; hash equality prevents false acceptance, but a projection transition can cause `unavailable` and require a new caller request.
- A proof package establishes state at block `H`, not current state after `H`.
- The package deliberately contains no finality evidence and does not contain or replace the chain's finality proof/light-client protocol.
- Genesis height `0` is not served; proof readiness begins with the first finalized post-genesis block carrying tag `0x08`.
- Catalog non-membership does not distinguish never-populated from retired.
- Secondary-index completeness and authenticated pagination remain unsolved.
- Proof/body peer recovery, exhaustive cross-store restart classification, and proof-readiness repair remain ADR-B-OCD-014 work.
- Portable snapshot import and archive bootstrap remain ADR-B-OCD-015 work.
- Production proof size, latency, concurrency, reader/cache, and availability envelopes remain ADR-B-CAP-001 work.

## Consequences and trade-offs

Benefits:

- the finalized header root becomes directly useful to an external verifier;
- MongoDB and RPC cease to require trust for the correctness of any returned point body;
- a node cannot forge another identity, body, shard, collection, root, or block binding without failing local verification;
- one immutable package can be stored by a user as permanent evidence of one finalized state;
- the existing CKB proof verifier and Poseidon formulas are reused rather than introducing a second tree or verification algorithm;
- the proof mirrors the write-side shard/collection/Catalog topology and preserves its retirement and parallel-seal benefits;
- no consensus state, EVM slot, header field, CE MDBX tree format, or body commitment changes.

Costs:

- one point proof contains two chained CKB proof layers for a present collection plus the fixed collection-top path;
- present reads require both authenticated CE data and matching Mongo bytes;
- proof RPCs add local MDBX/CPU/I/O and denial-of-service surface even though the MDBX snapshot is released before Mongo I/O;
- external clients must implement the fork schedule, strict body codec, CKB proof program, Poseidon formulas, OART decoding, and finality checking correctly;
- the latest-only service cannot regenerate an unretained old proof for a user who did not save it.

Rejected alternatives:

- trusting a root returned beside a proof, because only the independently finalized header is the trust anchor;
- trusting a node-supplied leaf/commitment instead of recomputing it from body bytes, because that would not authenticate the body;
- accepting node-supplied collection keys, shard indices, `K`, or direction bitmaps, because all are derivable from expected identity and fork rules;
- flattening shard, top, and Catalog evidence into one unrelated proof claim, because the verifier must reproduce the actual commitment topology;
- inventing a raw sibling-only CKB wire format, because compact-zero `MergeValue` and compiled proof semantics are already pinned and tested;
- opening separate MDBX transactions for shard and Catalog evidence, because a writer could commit between them and produce a mixed snapshot;
- treating missing Mongo bytes as absence, because only authenticated zero in the selected tree proves absence;
- returning a partial leaf/proof alongside `unavailable`, because it would create a second incomplete package contract without authenticating the requested body;
- requiring the proof service to block finalization or consensus participation, because it is a local read adapter over already finalized state;
- adding versioned JMT/history persistence in ADR-B-OCD-013, because the accepted v1 contract is proof generation from the snapshot frozen at request time;
- adding a root-only RPC, because ADR-B-OCD-012 already exposes the trust anchor in standard block `extraData`;
- embedding Simplex certificates or a finalized-header chain in every point package, because consensus finality verification is a separate reusable light-client concern;
- claiming list completeness from individually verified members, because point membership says nothing about omitted index results.

## Verification

### Golden package and hierarchy vectors

Pin independent golden vectors for all three domains covering:

- request identity, WWD extraction, collection key, ID field, final tree key, and shard index;
- strict canonical body payload, schema version, body hash, and leaf commitment;
- shard CKB membership and non-membership compiled proof bytes;
- all four `K = 16` collection-top siblings and left/right derivation;
- `R_collection`, Root Catalog membership/non-membership proof, `catalog_root`, and final `R_sealed`;
- exact package proof encoding version, variant discriminants, lengths, and complete transport bytes;
- present, entity-absent-in-present-collection, never-populated collection absence, retired collection absence, and `EmptiedByDelete` entity absence;
- proof unavailability at genesis height `0` and first post-genesis tag-`0x08` binding.

Vectors must agree across the node implementation, stateless verifier, pinned CKB reference semantics, and an independent Poseidon/reference implementation.

### One-snapshot concurrency

Hold a proof read transaction at finalized `H` while injecting:

- a zero-change finalized `H+1` marker;
- a mutation in another collection;
- an update/delete of the requested entity;
- a mutation in another shard of the same collection;
- a Root Catalog change;
- Tribute partition retirement with finalized prefix reclamation;
- `H+1` and `H+2` commits before the request closes.

The package must either complete entirely against `H` or fail without a package. It must never mix marker, shard root, top sibling, Catalog node, or root from another height. After closing the old reader, a new request must observe only the complete new state.

Assert that no proof path opens a second CE read transaction or consumes a cache entry not keyed and verified by the exact frozen marker; v1 should use the one transaction directly.

### Present body and Mongo races

For each domain verify:

- any canonical body whose recomputed leaf equals the frozen leaf returns `present` without consulting Mongo checkpoint height/hash;
- the same exact body is accepted whether Mongo is internally behind, equal to, or ahead of the selected CE marker;
- same-body ABA remains valid;
- missing, unavailable, malformed, non-canonical, wrong-identity, wrong-WWD, wrong-schema, zero-present, and one-bit-modified bodies never return `present`;
- projection advancement after the CE transaction closes yields either exact leaf equality or bare `unavailable`, with no partial package and no automatic in-request retry;
- authenticated absence does not accept or depend on a stale Mongo row;
- a Mongo failure never changes CE MDBX, finalized state, voting, or Marshal ACK.

### Non-membership matrix

Verify independently:

- collection absent from an empty/non-empty Catalog;
- entity absent from a present non-empty shard;
- entity absent from another shard;
- entity absent from an `EmptiedByDelete` collection;
- collection absent after retirement;
- wrong domain, partition, raw identity, collection key, tree key, or shard cannot reuse another valid non-membership proof;
- Catalog absence is not labelled never-populated or retired without separate evidence.

### Adversarial package verification

Mutate every package component independently:

- package chain ID, block number, and block hash;
- domain, raw EntityId36 (including its derived Tribute WWD), `StoredBody` envelope/schema/payload byte, and body length;
- proof encoding version, CKB opcode, MergeValue, zero count, sibling, sibling order, proof truncation, and trailing byte;
- shard-top sibling, count, order, level, derived direction, and `K`;
- `Present`/`Absent` result discriminant and both absent-evidence discriminants;
- header `extraData`, OART tag, header hash, selected chain, and finality evidence.

Every mutation must produce `invalid` or a local verifier-version error, never another accepted identity or silent absence. Fuzz all proof/package decoders under structural allocation/stack bounds and verify no panic, unsafe path, or unbounded allocation. An unknown future proof encoding is a verifier/decoder error, not an RPC result variant.

### Freshness and persistence of issued evidence

Create a package at `H`, then advance, update, delete, retire, reclaim local shard prefixes, restart, and rebuild current CE MDBX. The saved package must continue to verify against the saved/retrieved finalized header `H`.

The same package must fail a client policy that requires a newer tip when outside its chosen delta, without being described as cryptographically invalid. After the in-place tree advances, a new request must select the new visible marker and expose no way to request old `H`, proving ADR-B-OCD-013 does not accidentally promise archive behavior.

### Multi-node and external verification

From independently materialized validator/full nodes at the same finalized marker:

- produce packages whose decoded body and recomputed roots are equivalent;
- permit proof byte differences only if the pinned CKB proof format explicitly allows multiple valid encodings; otherwise require byte identity;
- verify Node A body plus Node B proof package against a header obtained from Node C when the body/proof identity and selected block agree;
- reject a valid proof against a different finalized block, chain, or identity;
- run the verifier without MDBX, MongoDB, or node process access.

### Resource verification

Measure and retain raw samples for:

- proof byte size for empty, sparse, dense, and adversarially shaped shard/Catalog trees;
- proof generation and verification Poseidon counts/latency;
- one-request MDBX transaction lifetime and pages retained while writers commit;
- concurrent readers during saturated finalized mutation/retirement;
- Mongo hit/miss/mismatch timing;
- RPC allocation and decoder maximums.

These measurements inform ADR-B-CAP-001; they do not change the correctness contract fixed here.

## Reset policy

ADR-B-OCD-013 adds an external proof/read contract and stateless verifier over the already activated ADR-B-OCD-008 through ADR-B-OCD-012 roots. It changes no body, identity, leaf, key, shard, collection, Catalog, sealed-root, EVM, header, candidate, or CE MDBX consensus format. A hard fork or state reset is therefore unnecessary when implementation matches the pinned existing semantics.

The proof package has its own `proof_encoding_version = 1` and may be enabled through the coordinated binary/RPC rollout. There is no dual proof verifier or compatibility fallback before a real preserved-client requirement exists.

If implementation vectors expose a defect in the pre-production CKB/Poseidon/root semantics rather than the new proof transport, the defect must be fixed at its owning ADR and the testnet reset/rebuild performed explicitly. ADR-B-OCD-013 must not hide a root-format correction behind a proof-encoding change.

Before implementation, ADR-B-OCD-013 must be reviewed against the actual implemented ADR-B-OCD-010 collection/view/root structures, ADR-B-OCD-011 retirement/reclamation transaction, ADR-B-OCD-012 header/OART decoder and finalized-provider APIs, vendored CKB compiled-proof interface, Mongo point-read seam, and RPC extension surface. Concrete Rust types and transport field spelling are pinned only after that seam review without changing the decisions above.

## Next unlocked step

ADR-B-OCD-014 defines exhaustive cross-store crash/restart reconciliation and proof-readiness recovery among canonical finalized headers, historical EVM state, CE MDBX markers/materialization, Mongo checkpoints/bodies, and candidate/replay evidence.

## Open questions and technical debt

- Expose a bounded production RPC and hold one immutable finalized CE snapshot from
  block selection through proof encoding.
- Verify package bytes independently and bind root to canonical header, chain id,
  topology and commitment scheme.
- Define pruning and the validity of already issued historical proofs.
- Bound key count, proof/body bytes, concurrency and verifier work before allocation.
- Add multi-node membership/non-membership, Mongo-race, historical-block and external
  verification tests through actual RPC.
