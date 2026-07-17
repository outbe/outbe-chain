# ADR-006: Commit to canonical Tribute and Nod bodies and verify MongoDB reads

- **Status:** Superseded; historical input only
- **Canonical mapping:** [`docs/adr/legacy-reconciliation.md`](../docs/adr/legacy-reconciliation.md)
- **Date:** 2026-07-15
- **Depends on:** ADR-005

## Context

ADR-005 makes MongoDB the source of complete Tribute, Nod item, and Nod bucket bodies used by execution. Projection provenance and strict repository decoding detect malformed storage, but a well-formed altered, stale, or wrong body is not yet authenticated against consensus state.

ADR-006 adds deterministic body commitments and verifies actual MongoDB reads before domain logic may use them. It does not add an SMT, global root, proof service, authenticated secondary-index completeness, or off-chain computation.

## Starting system

The starting system has:

- typed MongoDB readers for `TributeData`, `NodItemState`, and `NodBucketState`;
- exact finalized-parent projection readiness;
- no legacy EVM body fallback;
- compact protocol state retained in EVM;
- no canonical body encoding or consensus commitment for a returned MongoDB body.

## Added capability

Consensus state stores the expected current commitment for each complete body. A runtime reader accepts a MongoDB body only after recomputing its canonical identity and body commitment and matching the EVM value.

## Decision

### Canonical 36-byte entity identity and namespace

The logical address of a body is its domain-owned typed collection plus one exact 36-byte ID:

```text
entity_id_36 = worldwide_day_be4 || digest_be32
```

`worldwide_day_be4` is the existing `WorldwideDay`/`u32` encoded unsigned big-endian and is the future `partition_id`. Narrowing it to `u16`/BE2 is forbidden. `digest_be32` is never truncated.

The three typed identities are:

- Tribute: `tribute_id = worldwide_day_be4 || tribute_poseidon_digest_be32`;
- Nod item: `nod_id = worldwide_day_be4 || nod_poseidon_digest_be32`;
- Nod bucket: `bucket_id = worldwide_day_be4 || bucket_key_be32`.

Tribute and Nod item creation derive the full 32-byte canonical Poseidon-BN254 digest from the domain's deterministic owner/WWD recipe, then prefix the same WWD. The existing Tribute digest recipe remains `Poseidon(owner_as_canonical_Fr, worldwide_day_as_Fr)`; Nod moves from its old Keccak ID recipe to the same full-width Poseidon input recipe in its own typed collection. Collection/domain separation is external, so equal digest bytes across collections are not an identity collision. Nod bucket `bucket_key_be32` remains its domain-derived 32-byte bucket digest and is prefixed by the bucket WWD for the canonical bucket ID.

The collection/domain namespace is not duplicated inside the 36 bytes. There is no additional `BodyKind`: equal 36-byte values in different typed collections remain different logical entities.

Runtime code uses a fixed Rust newtype over `[u8; 36]`. The reset changes `TributeData.token_id: U256` into `tribute_id: EntityId36` and changes `NodItemState.nod_id: U256` into `nod_id: EntityId36`; active APIs do not keep parallel old/new ID fields. Solidity ABI boundaries use `bytes` and reject any length other than 36 before state access, allocation-dependent work, or hashing. There is no `uint256` surrogate ID, compatibility overload, or legacy fallback. Tribute/Nod methods and list results therefore use custom bytes IDs rather than claiming ERC-721 `uint256 tokenId` compatibility.

MongoDB primary keys use the exact lowercase hexadecimal encoding of the 36-byte ID. Repository point keys, list records, and pagination cursors use the same fixed ID. The payload's WWD must equal `entity_id_36[0..4]`; Tribute/Nod payload IDs must equal the complete 36 bytes, and a Nod bucket payload must reproduce `WWD || bucket_key`. WWD is immutable because it is part of identity: an update cannot move an existing entity between partitions.

### Append-only canonical Protobuf with per-body schema version

Each typed collection owns one append-only Protobuf body message. Existing field numbers and meanings are immutable, removed numbers are reserved, and new fields receive new numbers. A new field does not require copying the complete message into a parallel `V2` type.

Every stored body uses the common envelope:

```protobuf
message StoredBody {
  uint32 schema_version = 1;
  bytes payload = 2;
}
```

`schema_version = 0` and an empty payload are invalid. The envelope has one fixed canonical format. MongoDB and Memory store its canonical bytes as the primary record value; the version is not separate mutable BSON metadata.

The v1 typed payloads are:

```protobuf
message TributeBody {
  bytes tribute_id = 1;                  // exact EntityId36
  bytes owner = 2;                       // Address, 20 bytes
  uint32 worldwide_day = 3;
  bytes issuance_amount_minor = 4;       // U256 BE32
  uint32 issuance_currency = 5;          // <= u16::MAX
  bytes nominal_amount_minor = 6;        // U256 BE32
  uint32 reference_currency = 7;         // <= u16::MAX
  bytes tribute_price_minor = 8;         // U256 BE32
  bool exclude_from_intex_issuance = 9;
}

message NodItemBody {
  bytes nod_id = 1;                      // exact EntityId36
  bytes owner = 2;                       // Address, 20 bytes
  bytes gratis_load_minor = 3;           // U256 BE32
  uint32 worldwide_day = 4;
  uint32 league_id = 5;                  // <= u16::MAX
  bytes floor_price_minor = 6;           // U256 BE32
  bytes bucket_key = 7;                  // B256, 32 bytes
  bytes cost_amount_minor = 8;            // U256 BE32
  uint32 issuance_currency = 9;          // <= u16::MAX
  uint32 reference_currency = 10;        // <= u16::MAX
  uint64 issued_at = 11;
}

message NodBucketBody {
  bytes bucket_key = 1;                  // B256, 32 bytes
  uint32 worldwide_day = 2;
  bytes floor_price_minor = 3;           // U256 BE32
  bool is_qualified = 4;
  uint64 total_nods = 5;
  bytes entry_price_minor = 6;           // U256 BE32
}
```

The selected typed collection determines the payload message; no dynamic type URL or caller-selected type discriminator is stored in the envelope.

Every stored body carries an explicit unsigned `schema_version`. The version is an authenticated body input, not only MongoDB metadata:

- it selects the allowed Protobuf fields, validation rules, and body-to-commitment preimage rules;
- it is included in the commitment preimage;
- an altered version therefore cannot redirect verification without changing the expected commitment;
- several schema versions may coexist as different current bodies when later upgrades require it;
- `outbe-compressed-entities` selects the fork-active schema for new or updated bodies; domains construct semantic records and callers do not choose the version.

The Protobuf wire format is accepted only through a strict canonical profile: decode, validate the version-specific message and fixed-width/range constraints, canonical re-encode, and require byte-for-byte equality with the supplied bytes. Fields are encoded once in ascending field-number order with shortest valid varints; implicit scalar defaults are omitted unless an explicitly optional presence is part of the schema. Unknown fields for the declared version, duplicate singular fields, non-minimal encodings, and alternative representations are rejected. `map`, floating-point, `Any`, `Struct`, and groups are forbidden in committed bodies.

`oneof` and repeated fields are permitted when a future schema defines them. One selected `oneof` arm is part of the canonical body. Repeated order is semantic, and each packable repeated field fixes packed or unpacked as its one canonical wire form; a decoder may understand the alternative form, but byte-for-byte canonical re-encoding rejects it. The current v1 bodies contain no variable-size or repeated fields, so ADR-006 does not invent a generic payload cap. A future schema that adds strings, bytes, or repeated values owns its semantic length/cardinality and gas limits.

A future string field uses Protobuf `string` and therefore valid UTF-8. Its exact UTF-8 bytes are committed; any semantic normalization is an explicit domain rule applied before encoding.

`schema_version` governs body schema/canonicalization. `commitment_scheme_version` governs the complete authenticated-state construction: byte/identity/key/leaf hashing, CKB MergeValue and empty semantics, tree topology, sharding, collection/catalog aggregation, and sealed-root derivation. The two concepts are not conflated.

### Commitment scheme v1

Commitment scheme v1 uses the existing Circom-parameter-compatible Poseidon-BN254 implementation over the scalar field `Fr` and the CES1 domain-tag namespace. The normative implementation anchor is `outbe-poseidon` v0.11.0 at git commit `a6066cede0bb95672b194dc69c89946131794396`; changing its Circom parameter set or pinned implementation requires a new commitment scheme rather than a dependency-only update. The byte formulas and independent golden vectors below remain authoritative over library convenience APIs.

For canonical field elements:

```text
P(tag; x_0, ..., x_(m-1)) =
  Poseidon::<Fr>::with_domain_tag_circom(m, tag)
    .hash([x_0, ..., x_(m-1)])
```

ADR-006 uses these immutable CES1 tags from the compressed-entity design:

```text
TAG_BYTES_INIT   = CES1_TAG_BASE + 1
TAG_BYTES_ABSORB = CES1_TAG_BASE + 2
TAG_BYTES_FINAL  = CES1_TAG_BASE + 3
TAG_ID           = CES1_TAG_BASE + 4
TAG_KEY          = CES1_TAG_BASE + 5  // ADR-008 CKB SMT key
TAG_BODY         = CES1_TAG_BASE + 6
TAG_LEAF         = CES1_TAG_BASE + 7
TAG_SMT_BASE     = CES1_TAG_BASE + 8  // ADR-008 CKB MergeValue codec
TAG_SMT_NORMAL   = CES1_TAG_BASE + 9
TAG_SMT_ZERO     = CES1_TAG_BASE + 10
CES1_TAG_BASE     = 0x4345533100000000
```

ADR-008 uses `TAG_KEY` and the three CKB MergeValue tags without changing ADR-006 body/leaf commitments. `PBytes(object_tag, bytes)` is the only arbitrary-byte conversion:

```text
chunks = bytes split left-to-right into 31-byte chunks
chunk_i = unsigned big-endian value of chunk i, right-zero-padded to 31 bytes
n = number of chunks

s_0     = P(TAG_BYTES_INIT;   object_tag, byte_len, n)
s_(i+1) = P(TAG_BYTES_ABSORB; object_tag, s_i, i, chunk_i)
result  = P(TAG_BYTES_FINAL;  object_tag, byte_len, n, s_n)
```

`byte_len` is unsigned `u64`; chunk count and index are non-negative integers embedded without reduction. Every chunk is below `2^248` and therefore below the BN254 modulus. Reducing arbitrary 32-byte input modulo the field is forbidden.

For a typed-collection body:

```text
identity_bytes = entity_id_36
identity_f     = PBytes(TAG_ID, identity_bytes)
body_f         = PBytes(TAG_BODY, canonical_payload_bytes)

leaf_f = P(
  TAG_LEAF;
  commitment_scheme_version,
  schema_version,
  identity_f,
  payload_byte_len,
  body_f
)

commitment = BE32(leaf_f)
```

`commitment_scheme_version = 1` is fork-global: exactly one commitment scheme is active for every current body at a given height. It is therefore carried in public mutation events for independent replay but is not duplicated in each `StoredBody` or direct-map entry. An event declaring anything other than the fork-active scheme is invalid.

Commitment schemes do not coexist. ADR-006 through ADR-010 are implemented without intermediate testnet activation, so their direct-map/unsharded/sharded milestones do not consume versions: the first deployed complete collection/Root Catalog construction is commitment scheme 1. Only a semantic change after that first activation increases `commitment_scheme_version` and requires fork-governed migration/reset/recommit (or a later ADR adding coexistence metadata); an operator cannot switch it locally. `schema_version` is taken from the canonical `StoredBody` envelope. The typed collection/domain is the external commitment namespace and is not duplicated in the 36-byte identity or leaf preimage.

The output must be a canonical BN254 field element. Zero is the unique absent/delete sentinel. A computed zero for a present body is never interpreted as absence: creation/update fails closed before state mutation or event emission. A schema version may select a different registered body-to-commitment recipe within the active scheme. After the first scheme-1 activation, any authenticated-tree semantic change listed above requires a new `commitment_scheme_version`; pre-activation implementation milestones do not. Local MDBX schema/vendor metadata does not version network roots.

### Direct EVM commitment mappings

ADR-006 does not build an SMT. The current commitment authority is three distinct typed EVM mappings:

```text
identity_f = PBytes(TAG_ID, entity_id_36)

Tribute commitment[identity_f]    -> leaf commitment
Nod item commitment[identity_f]   -> leaf commitment
Nod bucket commitment[identity_f] -> leaf commitment
```

The mappings are distinct collection/domain namespaces. Every point API already receives the complete 36-byte ID, so it can derive `identity_f`, extract WWD, and authenticate absence before reading MongoDB.

A zero mapping value is canonical absence. A non-zero value requires a matching MongoDB body:

1. validate the exact 36-byte requested ID and derive `identity_f`;
2. read the typed EVM commitment by `identity_f`;
3. zero returns the normal canonical `NotFound` result without trusting a stale Mongo row;
4. non-zero requires the repository to return a body rather than `None`;
5. validate the body identity and its WWD prefix against the requested ID;
6. canonicalize its `StoredBody` and typed payload under the declared schema version;
7. recompute the leaf and require exact equality with the EVM value before domain use.

`None` for a non-zero EVM commitment is local projection unavailability/corruption, not entity absence. A stale, altered, wrong-identity, wrong-version, or non-canonical returned body fails verification.

Create writes a non-zero leaf, update replaces it, and delete clears it to zero in the same EVM journal/revert scope as the domain mutation and body event. ADR-007 centralizes those transition and same-block overlay rules. ADR-008 later replaces the direct mappings behind that lifecycle with an SMT while preserving the leaf formula.

### Public verifiable mutation events

Body events are public protocol evidence for an external observer, not merely ExEx transport. Each typed collection retains its owning emitter and distinct event signature, which together identify the collection namespace.

Stored events carry the complete transition inputs, conceptually:

```solidity
event TributeBodyStored(
    bytes tributeId, // exact EntityId36
    uint32 commitmentSchemeVersion,
    uint32 schemaVersion,
    bytes32 previousCommitment,
    bytes32 newCommitment,
    bytes canonicalPayload
);

event NodBodyStored(
    bytes nodId, // exact EntityId36
    uint32 commitmentSchemeVersion,
    uint32 schemaVersion,
    bytes32 previousCommitment,
    bytes32 newCommitment,
    bytes canonicalPayload
);

event NodBucketBodyStored(
    bytes bucketId, // exact EntityId36
    uint32 commitmentSchemeVersion,
    uint32 schemaVersion,
    bytes32 previousCommitment,
    bytes32 newCommitment,
    bytes canonicalPayload
);
```

Delete events carry the complete identity and prior leaf:

```solidity
event TributeBodyDeleted(
    bytes tributeId, // exact EntityId36
    bytes32 previousCommitment
);

event NodBodyDeleted(
    bytes nodId, // exact EntityId36
    bytes32 previousCommitment
);

event NodBucketBodyDeleted(
    bytes bucketId, // exact EntityId36
    bytes32 previousCommitment
);
```

For Stored, zero `previousCommitment` means mint and non-zero means update. `newCommitment` must be non-zero. Delete requires a non-zero `previousCommitment` and represents its transition to zero. Domain lifecycle rules may reject additional transitions.

`canonicalPayload` is the exact typed Protobuf payload, excluding the `StoredBody` envelope because the event already carries `schemaVersion`. ExEx constructs the MongoDB envelope from those two exact values; it does not reconstruct payload fields.

Execution and an independent observer can verify the complete transition:

1. derive the typed collection from emitter and signature;
2. validate the exact 36-byte event ID and derive `identity_f`;
3. select the declared commitment/body schema rules;
4. strict-decode, validate, and canonical re-encode the payload;
5. require embedded ID/WWD equality with the event ID and its prefix;
6. recompute and compare `newCommitment`;
7. require `previousCommitment` equality with the execution/replay pre-state leaf;
8. apply the transition to the replay state.

ExEx must preserve ADR-004 crash replay after receipt batches were partially committed but the block checkpoint was not. It therefore does not compare the first event for an identity with the currently materialized Mongo body, which may already be ahead within that replayed block. During full-block prepare ExEx validates identity, versions, canonical payload, recomputed `newCommitment`, zero/non-zero transition shape, and continuity between successive events for the same identity inside the block. Execution guarantees the first `previousCommitment` against EVM pre-state; an external observer verifies it against its own ordered replay or an ordinary EVM state proof.

A failure of the ExEx-checkable invariants writes no MongoDB domain mutation or checkpoint for that block. The EVM mapping write and event emission share one journal/revert scope, so reverted execution leaves neither canonical state nor a receipt-visible mutation.

The event deliberately does not duplicate every body field as separate Solidity arguments. The published `.proto` schema and canonical payload are the normative, independently decodable body; duplicating both representations would create two body copies that could diverge.

Until a later SMT/header root, an observer verifies the finalized event replay and may additionally use the ordinary EVM state root/proof for the direct commitment mapping. ADR-012/013 later provide a dedicated finalized-header root and entity proofs.

### Read-verification boundary

ADR-006 authenticates point bodies. A direct point lookup first reads the EVM commitment; zero is canonical absence, while non-zero requires an available matching MongoDB body.

For a MongoDB owner/day/global page, every returned 36-byte ID/body is subjected to the same point commitment verification before use. Dangling indexes, zero commitments for returned IDs, identity/version/canonicalization mismatches, duplicates, malformed ordering, and missing bodies fail explicitly.

ADR-006 does not authenticate secondary-index completeness, omitted IDs, or page continuation. That limitation already belongs to the ADR-005 testnet trust model and is not solved or redesigned here. An authenticated entity SMT alone will not prove a secondary list complete; any production completeness claim requires a separate authenticated-index design.

### Storage ownership

ADR-006 defines three logically distinct typed commitment mappings as a focused implementation/test backend. ADR-007 finalizes their temporary physical owner, and ADR-008 replaces them before the first combined deployment: `outbe-compressed-entities` at system state address `0x000000000000000000000000000000000000EE0D` owns the Tribute, Nod item, and Nod bucket namespaces together with the shared journaled overlay.

Tribute and Nod still own semantic body construction, authorization, and business state. `outbe-compressed-entities` owns fork-active schema selection, canonical encoding, and commitment/event construction. Central physical ownership does not merge the collections, expose arbitrary collection selection, or create a public mutating precompile. Small canonical-Protobuf and Poseidon helpers provide one byte-exact implementation at the compressed-entity seam.

ADR-008 replaces the direct mapping backend inside that same module, ADR-009 shards it, and ADR-010 completes the first deployed Root Catalog topology. The single coordinated testnet reset then removes legacy body/index fields and initializes only the final system without a live-state migration.

### Runtime failure policy

Verification results are classified before domain logic observes a body:

- zero EVM commitment is canonical absence and produces the normal domain `NotFound`/revert behavior;
- non-zero commitment plus backend `Unavailable` enters the ADR-005 shared `MongoUnavailable` recovery path and locally aborts/abstains without a `false` vote;
- non-zero commitment plus MongoDB `None` is `CommittedBodyMissing` and `Fatal`, because exact finalized-parent readiness says the committed body must exist;
- malformed/non-canonical Protobuf, unsupported declared schema, wrong fixed width/range, WWD/ID mismatch, non-canonical field element, zero present leaf, and commitment mismatch are deterministic corruption and `Fatal`;
- an EVM storage/provider failure is an execution failure, not a MongoDB absence;
- a recognized finalized event whose versions, payload, identity, recomputed new commitment, zero/non-zero transition shape, or same-identity within-block previous/new continuity disagree is `Fatal` and does not advance projection; the first event's previous leaf is intentionally not compared with possibly partial-block-ahead Mongo state.

Only technical backend unavailability receives the ADR-005 eight-second recovery window. Deterministic corruption, unsupported protocol data, and projector/event invariant failures initiate immediate structured graceful shutdown. These local failures do not make the node cast a negative consensus vote.

## Working result

After implementation:

- every current Tribute, Nod item, and Nod bucket body has one non-zero Poseidon-BN254 leaf in its typed EVM mapping;
- canonical storage uses a strict Protobuf `StoredBody` envelope with authenticated per-body schema version;
- direct point absence is decided by EVM state rather than a missing MongoDB row;
- every returned MongoDB body is canonicalized and matched against its EVM leaf before domain use;
- finalized events expose independently replayable previous/new commitment transitions and exact canonical payload bytes;
- ExEx stores those exact payload bytes in the canonical envelope after full-block verification;
- proposer and validator execution use the same verification path;
- legacy complete EVM bodies remain removed.

## Accepted limitations

- Direct mappings consume one EVM entry per existing body and are not scalable production storage.
- There is no SMT, dedicated compressed-entity root, Merkle proof, sharding, Root Catalog, or partition retirement.
- The first four ID bytes bind WWD as the future partition ID, but WWD does not yet select an SMT collection.
- Secondary-index list completeness and page continuation remain unauthenticated.
- MongoDB/body availability remains local operator responsibility; commitments authenticate returned point bodies but do not recover missing bytes.
- An observer can replay finalized events and use ordinary EVM state proofs, but there is no dedicated finalized-header root or entity-proof RPC.
- Only fork-supported Protobuf schemas and the one fork-global commitment scheme are accepted; there is no dynamic caller-selected schema or simultaneous commitment-scheme coexistence.
- This remains a coordinated-reset testnet stage, not a production/mainnet availability claim.

## Consequences

### Positive

- Well-formed altered, stale, wrong-identity, and wrong-version MongoDB bodies fail before business logic uses them.
- EVM zero/non-zero state authenticates point absence versus local body unavailability.
- Canonical Protobuf supports append-only typed evolution, including future strings and nested structures, without binding consensus to BSON.
- Public events carry enough information for independent commitment recomputation and deterministic replay.
- The leaf format is reusable by the later generic lifecycle and SMT stages.

### Negative

- Every point body read now adds an EVM mapping read plus Protobuf and Poseidon work.
- Three domain mappings temporarily duplicate the eventual authenticated-tree responsibility.
- Protobuf becomes a consensus codec and requires pinned generation, strict-profile enforcement, and golden vectors.
- Full payload bytes plus previous/new commitments increase receipt size.
- Tribute/Nod use custom 36-byte `bytes` IDs and no longer claim ERC-721 `uint256 tokenId` compatibility.
- Adding a committed field requires a new per-body schema version and coordinated fork-active validation rules.
- Verified list members do not imply a complete list.

## Alternatives considered

### Raw BSON as the commitment encoding

Rejected because BSON has no single consensus-canonical representation across field order, numeric variants, drivers, and its broader type system. MongoDB remains a storage envelope, not the body-hash specification.

### Continue hashing Postcard bytes

Rejected because Postcard was explicitly temporary in ADR-002 and ties consensus bytes to Rust/Serde layout rather than a published append-only schema contract.

### Typed DAG-CBOR arrays or maps

Rejected in favor of Protobuf because append-only numbered fields, generated typed codecs, strings, optional fields, and ecosystem tooling provide the desired evolution model. Protobuf is accepted only through the stricter canonical profile defined here; ordinary permissive Protobuf decoding is insufficient.

### Omit per-body schema version

Rejected because bodies under different append-only schemas may require different validation and body-to-commitment rules. The authenticated version selects those rules without requiring a copied `V2` message.

### Keccak body commitments

Rejected because the accepted future tree uses Poseidon-BN254. Using the CES1 leaf now avoids replacing every current commitment when ADR-008 introduces the SMT.

### Duplicate every body field in the Solidity event

Rejected because explicit ABI fields plus canonical Protobuf would create two complete body representations that could diverge. The event publishes the canonical payload and all verification metadata instead.

### Truncate Poseidon to keep a 32-byte WWD-prefixed ID

Rejected because `WWD_BE4 || Poseidon_BE28` reduces generic collision security from approximately 127 bits for full BN254 output to approximately 112 bits. The custom bytes ABI accepts `WWD_BE4 || Poseidon_BE32`, so ADR-006 preserves the full digest instead of spending security margin to retain `uint256` compatibility.

## Verification

### Canonical Protobuf vectors

For every v1 body type:

- publish normative `.proto`, payload, and `StoredBody` golden bytes;
- cover zero/minimum and maximum integer values, zero/non-zero addresses and IDs, and both boolean values;
- assert exact EntityId36/20-byte/32-byte length, ID-prefix/WWD equality, and `u16` range validation;
- reject unknown fields for the declared version, duplicate singular fields, out-of-order fields, non-minimal varints, explicit non-optional defaults, malformed lengths, empty payloads, and schema version zero;
- prove decode -> validate -> canonical re-encode byte equality;
- reserve removed field numbers and test append-only schema fixtures when the first extension is introduced.

### Commitment vectors

Publish cross-implementation golden vectors for:

- every CES1 tag used by ADR-006;
- `PBytes` for empty, 1, 30, 31, 32, 61, and 62-byte inputs;
- all three 36-byte identity forms;
- all three v1 body payloads and final leaves;
- different schema versions plus rejection of a non-fork-active commitment-scheme version;
- one-bit changes in identity and payload;
- canonical field encoding and rejection of values at or above the BN254 modulus;
- present-leaf zero rejection.

Vectors must agree with the pinned `outbe-poseidon` implementation and an independent reference implementation using the same Circom-compatible parameters and non-zero domain tags.

### Mapping and runtime reads

For Tribute, Nod item, and Nod bucket independently:

- the exact 36-byte ID derives `identity_f` and selects the typed mapping entry;
- mint requires zero and stores the emitted non-zero leaf;
- update requires/records the prior leaf, preserves the ID/WWD partition, and replaces it;
- delete records the prior leaf and clears to zero;
- reverted nested call, failed transaction, and out-of-gas execution leave no mapping change or event;
- zero mapping returns normal `NotFound` without accepting a stale Mongo row;
- non-zero plus missing Mongo body is `CommittedBodyMissing`;
- mutate every payload field, WWD, entity ID, schema version, stored payload, and EVM commitment independently and require failure;
- stale old body after an update fails;
- proposer and validator paths produce equal results, logs, mapping writes, balances, and state roots from independent correct MongoDB projections.

### Event and projection replay

Verify for every Stored/Delete event:

- exact emitter/signature and raw EntityId36 event data;
- declared versions are fork-supported;
- canonical payload re-encoding and embedded ID/WWD-prefix equality;
- recomputed new leaf equals `newCommitment`;
- execution and independent ordered replay see `previousCommitment` equal to pre-state;
- ExEx does not compare the first event to possibly partial-block-ahead Mongo state after restart;
- zero/non-zero transition rules;
- multiple mutations of the same identity in one block form a continuous previous/new chain;
- any ExEx-checkable mismatch causes zero MongoDB domain writes for the block and no checkpoint advance;
- crash/restart after each receipt boundary converges without falsely rejecting the first replayed `previousCommitment` against partial-block-ahead Mongo state;
- replay from genesis produces the same current typed commitment map and MongoDB bodies as execution.

List tests verify every returned member but make no completeness assertion beyond repository structural checks.

## Reset policy

ADR-006 changes canonical body bytes, event signatures, EVM layouts, and execution-visible verification. Implement it on the same branch as ADR-005 through ADR-010, then use the single complete coordinated testnet reset only after ADR-010 and combined verification.

The new genesis contains no Tribute, Nod item, or Nod bucket bodies or commitments. There is no legacy U256 Tribute/Nod ID migration, Postcard-body migration, old-event decoder in the active reset history, dual commitment calculation, dual-write period, or fallback. Projection starts from the first executable block using only the canonical Protobuf events.

## Next unlocked step

ADR-007 can place the three typed commitment mappings behind one generic mint/update/delete lifecycle and add the permanent journaled body overlay for deterministic same-block reads and rollback.
