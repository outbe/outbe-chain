# Proposed Concept v6.1 — Authenticated Compressed Entity Storage

## Off-live-state NFT records, consensus-enforced current root, canonical body publication, and independently verifiable reads

Status: `Q1`–`Q10` and `Q12`–`Q23` are closed. `Q11` has a decided limit structure and provisional guards,
but its numerical closure still requires the mandatory benchmark. This is a concept/system description, not an
ADR and not an implementation specification.

Source: a clean-system rewrite of `compressed_entities_concept_v6.md`. Decisions and remaining closure are
tracked in `compressed_entities_v6_decision_map_10-07-2026.md` as `Q1`–`Q23`.

Scope: storage, mutation, commitment, availability, proof, recovery, security, and evolutionary extension of NFT-like records. Off-chain computation is explicitly outside this document.

Confirmed decisions are written normatively so the system can be evaluated end to end. Only Q11 numerical values
remain unapproved for activation until the mandatory benchmark is complete.

---

## 0. System definition

Compressed Entity Storage is a consensus-authenticated current-state store for very large NFT-like record sets.

It moves full record bodies out of live EVM state while preserving four properties:

1. **Current-state integrity.** A current body or key absence can be verified against a finalized block without trusting the RPC or query database that returned it.
2. **Valid mutation.** A committed state is reachable only through ordered mutations accepted by a registered domain runtime and the generic entity lifecycle.
3. **Deterministic validation.** Every validator derives the same post-block root from the same parent state and block execution.
4. **Recoverability.** When a source remains available, tree and body data can be restored from retained canonical data, snapshots, or peers;
   every restored body remains independently verifiable even though local possession of the complete body set
   is not globally provable.

The system is not an arbitrary off-chain database. It is:

```text
canonical mutation data
        +
consensus transition rules
        ↓
authenticated current map (SMT)
        ↓
R_sealed in EVM state and finalized block header
```

MongoDB, object storage, RPC caches, and media stores are adapters that expose or materialize data. They are never state-transition inputs and never authorities.

### 0.1 Exact security claim `[Q1]`

For a present record at finalized block `B` and record key `k`:

```text
VerifyCurrent(B, k, body, proof) = true
```

means:

1. the commitment to `body` is a member of the entity map committed by `R_sealed(B)`;
2. `R_sealed(B)` is bound to the finalized block header;
3. finalized consensus accepted that map under the fork-active transition rules and finality assumptions.

It does not mean:

- an unverified list query is complete;
- media bytes are available;
- historical body bytes are retained forever;
- a membership proof independently re-executes business authorization.

The membership proof establishes record integrity. Consensus finality and validator execution establish transition validity.

A snapshot-bootstrapped validator does not re-execute history before snapshot height `H`. It verifies `R_sealed(H)` against the finalized header and executes transitions after `H`.

`Valid mutation` means accepted by fork-active deterministic protocol code. A proof does not establish that governance or business rules were well designed.

### 0.2 Availability classes `[Q1, Q10 decided]`

The system distinguishes:

```text
Inclusion availability   validators received the canonical mutation data needed
                         to validate the block.

Current-body availability
                         a provider either returns bytes that verify against the
                         current leaf or reports them unavailable.

Historical availability canonical history/archive policy retains prior bodies.

Media availability       application-level best effort behind content hashes.
```

Integrity is consensus-enforced. Every validator deployment must include current-body custody and point
body/proof service capability, but this is an operational validator requirement rather than a consensus-provable
readiness predicate. A node cannot prove complete disk custody without an `O(N)` scan. Retrieval assumes at
least one reachable provider with the requested bytes; the validator signing host itself need not be a public
Internet endpoint.

Historical and media availability remain separate weaker classes.

---

## 1. Goals and non-goals

### 1.1 Goals

- Billions of tribute, nod, and future NFT-like records (Gem joins later as a fork-activated domain,
  §3.2/§16.2) whose full canonical bodies do not live as per-record EVM storage.
- Deterministic `mint`, `update`, and `delete` semantics; a domain burn maps to generic delete.
- Current membership and non-membership proofs.
- Verifiable point body/proof service with explicit `unavailable` semantics and peer/event recovery.
- Untrusted RPC and query projections.
- Safe nested reverts, failed transactions, speculative branches, crashes, and restarts.
- A stable domain-facing interface that can support new record types without changing the commitment machinery.
- Explicit versioning for schema, hash, proof, and commitment evolution.

### 1.2 Non-goals

- Domain economics and business authorization rules.
- Placement and implementation of domain-owned aggregates, counters, consensus indexes, and lifecycle
  worklists outside the generic compressed-body primitive.
- Off-chain computation, lysis, settlement, aggregation, or compute-output delivery.
- Authenticated completeness of arbitrary secondary-index queries.
- Permanent protocol availability of media bytes.
- Historical proof generation from an in-place tree.
- A final Solidity/RPC ABI.

---

## 2. Trust and threat model

### 2.1 Trust and availability assumptions

- Consensus finality is sound under the chain's validator-adversary threshold.
- The frozen Circom-parameter-compatible Poseidon-BN254 hash suite is collision and preimage resistant for this use.
- The registered domain runtime and compressed-entity engine are deterministic consensus code.
- The active protocol versions and domain registry are fork-governed.
- Validators receive the complete block inputs required for execution.
- Validators satisfy the execution prerequisites of every active domain runtime. Those prerequisites are outside this storage concept.
- Current-body network retrieval assumes at least one reachable provider has the requested bytes.

### 2.2 Untrusted actors

- Transaction senders and contracts.
- Block proposers.
- RPC and MongoDB operators.
- Snapshot, archive, and media peers.
- Callers replaying stale proofs.
- Callers grinding IDs to concentrate work in a shard.
- Node-local storage and processes that may crash or become corrupted.
- Domain inputs and producers until the registered domain runtime accepts them.

### 2.3 Required mitigations

| Threat | Required mitigation |
|---|---|
| unauthorized mutation | fixed fork-active call graph; no public mutating EVM interface on the core |
| fake entity event | one canonical event emitter address and strict projector filter |
| body/leaf divergence | engine hashes the exact canonical bytes it publishes/records |
| missing or duplicate canonical event | core owns one event per successful mutation in the same journaled scope |
| schema downgrade | versions selected by block height/domain registry, never freely by caller |
| ID collision or historical reuse | mint ID is generated only by the registered deterministic domain generator; core domain-separates it and rejects current collision |
| unauthorized/reused partition retirement | only the registered domain lifecycle may retire; core validates canonical partition identity; retired keys have permanent non-reuse |
| proposer root forgery | every validator recomputes root; slot/header mismatch rejects block |
| RPC/Mongo body forgery | client verifies body and proof against chosen finalized root |
| stale proof replay | proof response binds height, block hash, root, chain, and versions |
| incomplete/corrupt SMT snapshot | finalized root marker plus streaming or lazy node verification; failure is local and triggers recovery |
| missing body snapshot data | point access returns `unavailable`; any returned body must verify against its current leaf |
| shard concentration | limits and benchmarks assume all block mutations hit one collection shard |
| split persistent commit | persistent tree never advances ahead of either durable Reth EVM state or consensus finality |
| non-finalized Mongo projection | ExEx gates projection on finalized `{height, block_hash}` rather than canonical notifications alone |
| local corruption | startup root/height/hash checks; halt and resync on mismatch |

---

## 3. System modules and seams

```text
fork-designated mutating domain entrypoint
  - entered from EVM or a scheduled system path
  - validates invocation context and all domain-specific rules
  - constructs a full canonical body for mint/update or selects delete
                 │
                 ▼
CompressedEntityStore module
  - owns ID normalization, generic lifecycle, hashing, journal, event, sealing
  - owns the only generic compressed-state mutation interface
                 │
       ┌─────────┴──────────┐
       ▼                    ▼
in-place sharded SMT     canonical mutation stream
in CE-owned MDBX        receipt-visible events
       │                    │
       │                    ▼
       │              current body store
       │              (required validator state;
       │               NOT a sealing input)
       ▼                    │
finalized R_sealed          │
       │                    │
       ├────────────────────┘
       ▼                    
proof/read module       secondary-index projection
SMT + current bodies    current bodies/events → MongoDB
```

### 3.1 Internal domain-to-store interface `[Q2 decided]`

Authority is the fixed fork-active consensus call graph. No capability token or runtime authentication exists between trusted Rust modules.

Only fork-designated mutating domain entrypoints may call the store. An entrypoint may be an EVM precompile handler, a scheduled system/lifecycle handler, or an explicitly wired cross-domain path.

Conceptually, the internal Rust interface is:

```text
mint(fork_bound_domain_id, raw_id, canonical_body_bytes) -> MutationOutcome
update(fork_bound_domain_id, raw_id, canonical_body_bytes) -> MutationOutcome
delete(fork_bound_domain_id, raw_id) -> MutationOutcome
retire_partition(fork_bound_domain_id, partition_key) -> MutationOutcome

read_commitment(domain_id, raw_id) -> leaf_value | absent

CompressedEntitiesLifecycle::end_block(block_runtime_context) -> SealOutput {
  R_sealed,
  staged_tree_batch
}
```

`read_commitment` is the consensus-execution read and is bound to the execution overlay from §8.1: it returns a
pending `Set`, treats pending `Deleted` as absent, and falls through to the parent SMT only for `Untouched`.
External RPC proof reads do not use this interface; they read the persisted finalized tree through §10.3–§10.4.

The domain entrypoint:

1. Validates the authenticated invocation context, authorization, business rules, and all domain-specific production conditions.
2. For mint, internally generates `raw_id`; for update/delete, selects the referenced existing ID.
3. For mint/update, canonical-encodes the complete semantic record using the fork-active domain schema.
4. Calls the typed core operation in the same journaled execution scope as its surrounding domain writes.

Whether domain rules consult domain-owned state before producing the new body or selecting delete is entirely a
domain concern. The generic store neither requests nor interprets an old body.

The store:

1. Rejects unknown or inactive `domain_id`; the ID is fork-bound and never freely supplied by user calldata.
2. Validates generic existence transitions.
3. Derives `partition_key`, `collection_key`, `id_bytes`, `tree_key`, shard, active versions, `leaf_value`, and
   the journaled block-batch update.
4. Emits the canonical generic mutation event from `0xEE0B`.
5. For mint/update, hashes exactly the canonical bytes that it places in the canonical event. Delete receives and publishes no body.

The caller cannot supply `id_bytes`, `collection_key`, `tree_key`, versions, leaf, shard, root, pending state, or
canonical event fields. A partitioned domain may pass its semantic partition key only through its fork-designated
typed path; core verifies it against the registered derivation.

The core has no mutating EVM ABI. Direct user calls to `0xEE0B` cannot invoke `mint`, `update`, or `delete`.

Domain-specific product events may coexist at domain addresses. They are not the canonical recovery or projection source.

Mutation batch changes and the canonical event share one journaled execution scope. A failed EVM transaction, subcall, or atomic system handler reverts them together.

Mutating EVM domain entrypoints accept only ordinary `CALL` execution at their registered address. The
dispatcher rejects static frames and foreign-context schemes including `STATICCALL`, `DELEGATECALL`, and
`CALLCODE` before domain logic. This rule belongs to the dispatcher/entrypoint because the internal core does not
receive an EVM call scheme.

System mutations use the same domain runtime and store interface inside a receipt-visible system transaction.

Raw hooks cannot call the core: their logs are not part of transaction receipts and therefore cannot satisfy the canonical-event contract.

Consensus code is trusted under the protocol threat model. A new Rust bypass in a modified binary is a protocol-code bug or different fork, not an EVM caller capability.

The SMT implementation is private to the module. A public tree-backend abstraction is not required while only one implementation exists.

### 3.2 Current-body and projection seams

The current-body store is required validator materialization for point service. Its bytes are checked against
leaf commitments but are not a separate root authority. Every validator deployment must retain current bodies
and provide the service capability, but the node does not claim global local completeness without enumerating
the entire SMT and body store. The service may be separated from the signing process and need not be publicly
reachable on the signing host.

The generic query projection consumes the same finalized mutations and may maintain owner/day/domain indexes.
This does not constrain consensus state or indexes privately owned by a domain module; those remain part of that
domain's design and are not storage-core authorities.

MongoDB is the initial adapter and may physically host both current-body rows and secondary indexes. Neither
Mongo high-water nor local row counts authenticate global completeness; a concrete body is accepted only after
checking it against the current leaf.

Projection lag, omission, duplication, or corruption cannot change consensus state.

Missing or corrupt body rows cause per-key `unavailable` and local recovery. They cannot alter consensus state
or make an invalid body pass proof verification.

**Stage 1 testnet execution profile (Variant A — recorded owner decision).** Until off-chain computation is
introduced, every validator runs its OWN local MongoDB projection (a separate container in the same
deployment counts as local; a shared/external Mongo serving multiple validators is forbidden), and EVERY
body-dependent Tribute/Nod runtime operation — Lysis partition lists, and point reads in NodFactory
mine_gratis, Tribute burn/processing — reads
canonical bodies through the standard Mongo-backed API. This is an explicit, testnet-only exception to the
rule that MongoDB is never a state-transition input: Mongo materialization completeness/availability is a
conscious testnet operational trust assumption, not a production security guarantee. Binding rules (fixed
in the implementation task packet, Gate D0): strict checkpoint equality before any body-dependent
operation; canonical query handling; per-body CES verification before use; after a successful mutation any
further body-dependent operation on that entity is forbidden until the next finalized block (no
canonical-body overlay exists); detected unavailability is LOCAL and graded — a global readiness failure
(outage, checkpoint lag, missing mandatory partition baseline) gates the validator's roles entirely, while
a single unavailable row at an aligned checkpoint affects only the reading operation/candidate — in both
cases the proposer does not build the affected operation/block, the validator abstains from voting, the
candidate is never consensus-invalid, and the network continues on quorum; hard production disable.
Stage 1 additionally fixes: the Gem domain is DEFERRED — Stage 1 migrates Tribute and Nod only, while
Gem/GemFactory/GemLifecycle remain on their existing per-record EVM storage and onboard later as a
fork-activated new domain (§16.2); the Lysis system phase executes before user transactions and before any other
CE-mutating system work on the same partitions (validator-verified, hard-fork-governed order); a validator
reaches operational readiness only by bootstrapping from a `tree-with-bodies` snapshot fully covering all
active Tribute partitions (a `tree`-only bootstrap serves as full node and never claims validator
readiness); and post-finalization recovery of a mutated body relies on a bounded recent-version retention
window — outside it there is NO automatic rejoin guarantee and the operator restores a complete paired
Reth+CE+body checkpoint. Production/mainnet activation must not make validator-local Mongo an execution
prerequisite; it requires a separately designed off-chain computation path with its own release gate.

---

## 4. Domain registry and identity

### 4.1 Domain registry `[Q6, Q20, Q23 decided]`

Each active domain version fixes:

```text
domain_id
registered runtime identity
id_encoding_kind_u8
ID generation version and registered generator
partition policy and canonical partition-key derivation
collection_shard_count = 2^k
partition retirement policy
active schema version(s)
active hash version
generic lifecycle policy extensions
gas and quota profile
activation height
```

`domain_id` is an unsigned 16-bit integer. Its `domain_id_be2` encoding is exactly two bytes, big-endian; every
bare `domain_id` Poseidon input is the same `u16` value embedded canonically into `Fr`. Genesis domains use
`activation_height = 0`; later versions use their fork activation height.

`id_encoding_kind_u8` is the concrete unsigned byte assigned by the fork-active domain-registry entry, not a
caller- or RPC-selected hint. The activation specification for every domain version must assign its exact value;
that assignment is immutable for the version. All consensus execution and proof verification at height `H` use
the registry entry active at `H`. Unknown, inactive, or mismatched encoding kinds fail closed.

Changing the registry is fork-governed. A domain runtime may tighten generic mutation rules but may not bypass them.

Each registered generator must be deterministic and lifetime-unique within its domain. The caller cannot freely choose its mint ID, generation mode, or version.

The partition policy is either `Singleton` or `Partitioned`. `Singleton` has no partition key and therefore one
collection per domain. `Partitioned` defines a canonical non-empty `partition_key` derivable from `raw_id` (or,
for a future explicitly keyed interface, included in the expected public identity). The core and verifier derive
and validate it; calldata/RPC cannot redirect an entity to another partition. `collection_shard_count` is a
power of two fixed for the domain version and shared by all its collections. Changing it for existing state
requires an explicit migration/new commitment scheme.

### 4.2 Poseidon-BN254 hash suite `[Q7, Q18 decided]`

Commitment scheme v1 uses the existing Circom-parameter-compatible `outbe-poseidon::Poseidon` over the BN254
scalar field `Fr`. Compatibility means the same BN254/x5 parameter set; because this scheme uses non-zero domain
tags as the initial state, a circuit must use `PoseidonEx`/an equivalent gadget with `initialState = tag`, not
circomlib's stock zero-initial-state `Poseidon(nInputs)` template.

Poseidon2 is a different protocol primitive because it produces different commitments. It may be introduced only under a new `commitment_scheme_version`.

For canonical field elements:

```text
P(tag; x_0, ..., x_(m-1)) =
  Poseidon::<Fr>::with_domain_tag_circom(m, tag).hash([x_0, ..., x_(m-1)])
```

Every `tag` is a distinct fixed non-zero `Fr` constant selected by the protocol. Commitment scheme v1 uses the
structured namespace:

```text
CES1_TAG_BASE = 0x4345533100000000 = 4847372043852709888
TAG(tag_id)   = Fr(CES1_TAG_BASE + tag_id)
```

`0x43455331` is ASCII `CES1` (Compressed Entity Storage v1). The following table is the normative tag registry;
Rust constants, circuits, and golden vectors mirror it and never redefine it:

| Symbol | ID | Unsigned integer | Canonical `Fr` big-endian bytes |
|---|---:|---:|---|
| `TAG_BYTES_INIT` | 1 | 4847372043852709889 | `0x0000000000000000000000000000000000000000000000004345533100000001` |
| `TAG_BYTES_ABSORB` | 2 | 4847372043852709890 | `0x0000000000000000000000000000000000000000000000004345533100000002` |
| `TAG_BYTES_FINAL` | 3 | 4847372043852709891 | `0x0000000000000000000000000000000000000000000000004345533100000003` |
| `TAG_ID` | 4 | 4847372043852709892 | `0x0000000000000000000000000000000000000000000000004345533100000004` |
| `TAG_KEY` | 5 | 4847372043852709893 | `0x0000000000000000000000000000000000000000000000004345533100000005` |
| `TAG_BODY` | 6 | 4847372043852709894 | `0x0000000000000000000000000000000000000000000000004345533100000006` |
| `TAG_LEAF` | 7 | 4847372043852709895 | `0x0000000000000000000000000000000000000000000000004345533100000007` |
| `TAG_SMT_BASE` | 8 | 4847372043852709896 | `0x0000000000000000000000000000000000000000000000004345533100000008` |
| `TAG_SMT_NORMAL` | 9 | 4847372043852709897 | `0x0000000000000000000000000000000000000000000000004345533100000009` |
| `TAG_SMT_ZERO` | 10 | 4847372043852709898 | `0x000000000000000000000000000000000000000000000000434553310000000a` |
| `TAG_TOP_NODE` | 11 | 4847372043852709899 | `0x000000000000000000000000000000000000000000000000434553310000000b` |
| `TAG_SEALED_ROOT` | 12 | 4847372043852709900 | `0x000000000000000000000000000000000000000000000000434553310000000c` |
| `TAG_COLLECTION_KEY` | 13 | 4847372043852709901 | `0x000000000000000000000000000000000000000000000000434553310000000d` |
| `TAG_COLLECTION_ROOT` | 14 | 4847372043852709902 | `0x000000000000000000000000000000000000000000000000434553310000000e` |

Tag ID `0` is never assigned; the untagged/stock zero-`Fr` initial state is outside the CES1 namespace and is
forbidden for CES1 hashes. IDs `15..=65535` are reserved for explicitly
fork-activated CES1 extensions; reservation does not authorize their use without a normative registry update.
Assigned IDs and values are immutable and never reused. Changing an assigned tag requires a new
`commitment_scheme_version`; a future scheme defines its own namespace. All CES1 values are below `2^64 < p`,
so their canonical embedding uses no field reduction. Callers cannot provide tags, Poseidon parameters, fields,
or hash versions.

`PBytes(object_tag, bytes)` is the only byte-to-field primitive:

```text
chunks = bytes split left-to-right into 31-byte chunks
chunk_i = unsigned big-endian Fr value of chunk i, right-zero-padded to 31 bytes
n = number of chunks

s_0     = P(TAG_BYTES_INIT;   object_tag, byte_len, n)
s_(i+1) = P(TAG_BYTES_ABSORB; object_tag, s_i, i, chunk_i)
result  = P(TAG_BYTES_FINAL;  object_tag, byte_len, n, s_n)
```

For empty bytes, `n = 0` and the absorb step is skipped. Including length, chunk count, index, phase tags, and object tag makes the encoding unambiguous.

`byte_len` is the exact pre-chunking byte count as an unsigned `u64`, canonically embedded into `Fr`; `n` and
`i` are likewise non-negative integer values embedded without reduction. Inputs exceeding their declared
integer range are rejected.

Every 31-byte chunk is strictly below `2^248` and therefore below the BN254 modulus. Reducing arbitrary 32-byte values modulo the field is forbidden because it is not injective.

Every field element has one wire/storage representation: its canonical 32-byte big-endian encoding. External non-canonical values `>= p` are rejected rather than reduced.

### 4.3 Canonical collection and entity identity `[Q6, Q20, Q23 decided; Q7 hash suite]`

All raw IDs use one tagged derivation; fixed-width IDs are not passed through directly:

```text
id_f = PBytes(
  TAG_ID,
  domain_id_be2 || id_encoding_kind_u8 || raw_id_len_be4 || raw_id
)

partition_presence_u8 = 0 for Singleton, 1 for Partitioned
partition_key_len_be4 = 0 for Singleton

collection_key_f = PBytes(
  TAG_COLLECTION_KEY,
  domain_id_be2 || partition_presence_u8 || partition_key_len_be4 || partition_key
)

tree_key_f = P(
  TAG_KEY;
  commitment_scheme_version,
  collection_key_f,
  id_f
)

id_bytes = BE32(id_f)
collection_key = BE32(collection_key_f)
tree_key = BE32(tree_key_f)
```

This prevents ambiguity between a raw 32-byte ID and the commitment to a variable-length ID.

The domain generator may use a counter, nonce, UUID-like derivation, unique source identity, or another scheme. The exact algorithm is not a generic storage primitive.

Its inputs must be consensus-visible or already accepted deterministic domain inputs. Local randomness and dependence on a future block hash are forbidden.

If generation mutates a counter or nonce, that write shares the mint journal and rolls back with a failed mint.

The generator must never reproduce a prior `raw_id`, including one whose entity was deleted. Core checks current absence only and stores no historical used-ID set, IMT, or tombstone.

Domain separation prevents cross-domain/cross-partition key collisions under the frozen Poseidon-BN254
assumption. One encoding/generation/partition mode is active per domain version.

For Tribute v1, `partition_key` is exactly the first four bytes of `tribute_id`, interpreted as canonical
big-endian `wwd_id: u32`; the core rejects a mismatch. Nod v1 uses `Singleton`; Gem (deferred, §3.2) is
expected to onboard as `Singleton`. A retired partition key
has permanent non-reuse: the domain runtime must never create it again. Core stores no permanent retirement
tombstone; Tribute enforces the monotonic/lifetime-unique WWD lifecycle in domain-owned consensus state.

---

## 5. Canonical body and authenticated value

### 5.1 Body encoding

- Bodies are typed DAG-CBOR arrays with schema-fixed field order.
- Free-form maps are forbidden in consensus bodies.
- Integers have schema-fixed signedness and width/range.
- Floats, indefinite-length forms, unknown tags, duplicate map keys, and non-canonical encodings are rejected.
- Optional values and strings have schema-defined representations; any normalization occurs before consensus encoding and is part of the domain rule.
- The codec crate is pinned, but golden byte vectors and the protocol byte grammar are authoritative.

### 5.2 Leaf and tree value

```text
body_hash_f = PBytes(TAG_BODY, body_bytes)

leaf_f = P(
    TAG_LEAF;
    commitment_scheme_version,
    domain_id,
    schema_version,
    hash_version,
    id_f,
    body_len,
    body_hash_f
)

leaf_value = BE32(leaf_f)
```

`body_len` is the exact canonical `body_bytes` length as an unsigned `u64`, canonically embedded into `Fr`.

Generic lifecycle status is not stored. A domain state such as `Issued`, `Qualified`, or `Settled` remains an ordinary field of that domain's canonical body.

CKB SMT stores `leaf_value` verbatim as its canonical 32-byte value. `ZERO` is reserved as the unique empty/delete sentinel; a derived present value equal to `ZERO` is rejected fail-closed.

The exact node, value, delete, and empty-tree encodings are consensus vectors.

### 5.3 Present and absent `[Q5 decided]`

```text
present = inclusion proof for tree_key → non-zero leaf_value
absent  = non-membership proof for tree_key
```

The cryptographic empty sentinel and the value-hash rule are fixed by the commitment scheme. A produced value must never be interpreted as the empty sentinel.

Current absence does not distinguish a never-minted key from a deleted key. Historical deletion is established only by its canonical ledger event.

---

## 6. Mutation semantics

### 6.1 Generic lifecycle `[Q5, Q23 decided]`

```text
mint(id, body)
  require absent
  require leaf(body) != ZERO
  record Set(leaf(body))

update(id, new_body)
  require present
  require leaf(new_body) != ZERO
  record Set(leaf(new_body))

delete(id)
  require present
  record Deleted

retire_partition(partition_key)
  require domain partition policy allows retirement
  require partition exists and is not already retired
  record RetiredCollection
```

- Mint/update accept only a complete canonical body. The core has no patch operation and does not load an old body.
- Delete accepts only the domain-bound ID. No old body, old leaf, tombstone, or persistent generic status is retained.
- `mint → update → delete` finishes absent; `delete → update` and a repeated delete reject.
- The `Q6` domain invariant forbids the generator from producing the same `raw_id` again after delete.
- An update that reproduces the same leaf remains a successful ordered operation unless the domain rejects it before calling core.
- A reverted call or transaction leaves no mutation and no canonical event.

At end-block sealing, `Deleted` becomes `SMT.update(tree_key, ZERO)`. CKB SMT removes the leaf and any newly empty branches.

`retire_partition` deletes the collection leaf from the Root Catalog in one consensus mutation. Every entity in
that collection becomes absent in the new current root without per-entity deletes. It emits one canonical
partition-retirement event. The retired partition key is permanently non-reusable; physical shard namespaces
are reclaimable only after finality.

### 6.2 Domain-produced canonical body `[Q3 decided]`

For mint/update, the domain entrypoint completes all domain-specific processing before it calls the storage module.

It converts the accepted semantic record into canonical bytes using the fork-active domain schema. Delete has no semantic body.

```text
domain transaction or system input
      → domain validation and computation
      → semantic record
      → canonical domain codec
      → CompressedEntityStore.mint/update(..., canonical_body_bytes)
```

For Tribute, `TributeFactory` processes the encrypted offer and `Tribute` supplies the resulting canonical `TributeData`. That internal path does not create a special storage mode.

The storage module treats the body as opaque canonical bytes. It does not decrypt inputs, verify domain evidence, call external producers, or branch on how the record was created.

The core hashes the exact bytes it receives. It applies generic lifecycle, journal, and SMT rules, then publishes the same body through the canonical full-body event fixed in `Q4`.

No ZK proof links calldata to the body. Normal consensus execution validates the domain call graph and the resulting receipt, state root, and `R_sealed`.

For a product-level burn, the domain entrypoint performs its rules and calls `CompressedEntityStore.delete(domain_id, raw_id)` without a body.

Full execution repeats the domain path. MongoDB rebuild consumes finalized canonical events. Snapshot bootstrap loads the current body set at `H` and applies finalized events after `H`.

---

## 7. Canonical mutation data and events

### 7.1 Uniform full-body publication `[Q4 decided]`

Calldata is the domain execution and full-replay input. It may contain body fields, derivation inputs, or ciphertext.

Every successful `mint/update` publishes the full canonical body in a receipt-visible event. This is the only generic input consumed by ExEx and MongoDB rebuild.

For public domains the body may therefore occupy both calldata and receipt bytes. The duplication is accepted in exchange for one domain-independent projection path.

`Exactly one` means one canonical event per successful core operation, not one event per entity, transaction, or block.

Multiple mutations of one entity remain separate ordered logs. Their order is `block_number → transaction_index → log_index_in_receipt`.

### 7.2 Canonical event

The reserved engine address emits one of three discriminated event forms:

```text
CompressedEntityWriteV1(
  domain_id,
  partition_key_or_none,
  id_bytes,
  operation,        // Mint | Update
  schema_version,
  hash_version,
  leaf_value,
  body
)

CompressedEntityDeleteV1(
  domain_id,
  partition_key_or_none,
  id_bytes
)

CompressedEntityPartitionRetiredV1(
  domain_id,
  partition_key
)
```

For mint/update, `body` is byte-identical to the canonical bytes hashed into `leaf_value`.

`partition_key_or_none` is empty for `Singleton` domains and the canonical partition key for `Partitioned`
domains. It is carried because `partition_key` is not derivable from the hashed `id_bytes`, and `DeleteV1`
has no body to consult: without it a generic consumer could not re-derive `collection_key`/`tree_key` for a
partitioned domain. A consumer validates its canonical shape against the fork-active partition policy
(presence, length, canonical encoding) and derives `collection_key`/`tree_key` from the event plus the
fork-active registry parameters (`partition_presence_u8`, `commitment_scheme_version`) — without the body,
a second event, or `raw_id`; the
binding between `raw_id` and the partition key is guaranteed by the consensus-validated core emitter and is
not independently re-checkable from the event (the event carries no `raw_id`).

Delete is encoded by its own versioned topic and carries no body, old leaf, schema/hash versions, root, proof, status, or placeholder zero leaf.

Partition retirement is domain-authorized and carries the canonical partition identity only. ExEx removes the
current projection range by `{domain_id, partition_key}`; it does not synthesize per-entity canonical events.

The event format is versioned by its signature/topic. Historical decoders remain available after an upgrade.

Projection adapters accept only events from the reserved engine address. Events with the same signature from any other address are ignored.

Mutation batch changes and event emission share one journaled execution scope. Revert, OOG, or failed nested call leaves none of them committed.

Raw hooks cannot produce compressed mutations. Lifecycle and other system mutations execute through receipt-visible system transactions.

---

## 8. Journaled block transition `[Q8 decided]`

### 8.1 Execution overlay `[Q8, Q21 decided]`

During block `B`, the engine writes through the existing journaled EVM storage provider at reserved address `0xEE0B`:

```text
fixed storage layout v1:
  slot 0  storage_schema_version = 1
  slot 1  last_sealed_root
  slot 2  pending entity map base
  slot 3  touched_entities StorageVec base
  slot 4  pending retired-collection map base
  slot 5  touched_collections StorageVec base

pending entity map:
  (collection_key, tree_key) → Untouched | Set(non_zero_leaf_value) | Deleted

single-slot wire/storage encoding:
  0                         = Untouched
  1 <= word < p             = Set(BE32(word))
  U256::MAX                 = Deleted
  p <= word < U256::MAX     = invalid

touched_entities:
  unique (collection_key, tree_key) locators in deterministic first-touch order

pending retired collections:
  collection_key → Untouched | Retired

touched_collections:
  unique collection keys retired in deterministic first-touch order
```

Slot 0 is an immutable v1 schema marker. Slot 1 is the only persistent semantic value changed by ordinary block
sealing. Slots 2–5 are transient journal anchors whose keyed/element storage is empty after every successful seal.

Here `p` is the BN254 scalar-field modulus. Every valid `leaf_value` is a canonical non-zero `Fr` encoding and
therefore strictly below `p`; `U256::MAX` cannot collide with a leaf. `Deleted` exists only in the journaled
execution overlay. It is mapped to SMT `ZERO` during seal and its pending slot is then reset to `0`; no tombstone
or permanently reserved 32-byte value remains in the SMT or finalized EVM state. Any pending word in the invalid
range fails block execution deterministically.

At begin-block, both touched vectors must be empty. Opening the batch is an executor view over these journaled slots; it does not require a persistent open flag.

On the first entity transition from `Untouched`, the core appends the composite locator once to
`touched_entities`. Repeated mutations replace only its pending value. Retirement appends a collection once to
`touched_collections`, makes the entire collection absent for subsequent same-block reads, and rejects later
entity mutation in that collection.

An overlay read returns a pending `Set` value, treats `Deleted` as absent, and falls through to the parent SMT only when the key is untouched.

Transactions execute sequentially. Ordered mutation history already exists in canonical receipt events, so a second ordered op-log would duplicate data and cleanup work. The SMT seal consumes only one final value per touched key.

Existing EVM journaling supplies:

- nested-call revert;
- failed-transaction revert;
- read-your-write within and across transactions in the block;
- log and mutation rollback together.

Mutating domain EVM entrypoints accept only ordinary `CALL` at their registered address; `STATICCALL`,
`DELEGATECALL`, `CALLCODE`, and any future static or foreign-context scheme are rejected before domain logic.
Direct user access to the reserved storage writer does not exist.

### 8.2 End-block seal `[Q8, Q21, Q22 decided]`

After all transactions and all other fork-active end-block lifecycle modules, but before final state-root
calculation, `run_end_block_seal()` executes as the last consensus end-block module. Its placement is explicit
and hard-fork governed in the executor through `BlockLifecycle`/`BlockRuntimeContext`; no later lifecycle step
may create a compressed mutation.

The shared lifecycle contract has a typed end-block result:

```text
trait BlockLifecycle {
  type EndBlockResult

  begin_block(ctx: &BlockRuntimeContext) -> Result<()>
  end_block(ctx: &BlockRuntimeContext) -> Result<EndBlockResult>
}

ordinary lifecycle module:
  EndBlockResult = ()

CompressedEntitiesLifecycle:
  EndBlockResult = SealOutput { R_sealed, staged_tree_batch }
```

The associated result type keeps the generic lifecycle primitive independent of CE types while allowing the
executor, which calls the concrete module, to receive its typed output directly. Every lifecycle implementation
declares its result type; no global output registry, type erasure, or mutable result slot in `BlockRuntimeContext`
is used.

1. Read every unique touched entity/collection and its final pending state.
2. Decode each pending word using §8.1, reject `Untouched` or an invalid encoding, map `Deleted` to `ZERO`,
   sort by `{collection_key, shard_index, tree_key}`, and group by collection/shard.
3. For every non-retired collection, prepare one staged `update_all` per touched shard against the parent
   collection tree/ancestor overlays; preparation has no persistent side effects.
4. Recompute each touched non-retired collection's shard top and `R_collection`. Delete retired collection
   leaves, update the remaining changed `collection_key → R_collection` leaves in the Root Catalog, and
   derive `R_sealed(B)`.
5. On the proposer path, export the computed root to the header-artifact builder. On the validator path, require
   exact equality with the supplied tag-`0x08` header artifact defined in §9.3.
6. Through one buffered post-block storage hook, write `last_sealed_root = R_sealed(B)`.
7. In the same buffered hook, zero every pending entity/collection entry, every touched-vector element, and
   finally both vector lengths.
8. Assert the transient-state post-condition and flush the buffered EVM changes once.
9. Notify the state-root task through `OnStateHook` with
   `StateChangeSource::PostBlock(StateChangePostBlockSource::Other("compressed_entities_seal"))` using the
   complete root/cleanup change set.
10. Return `SealOutput` directly to the executor. The lifecycle module does not wait for executor finish and
    does not publish the batch to the speculative cache itself.

The executor retains `SealOutput` as an ordinary local typed value. After successful executor finish and block
sealing, it assigns the block hash and publishes the batch to the speculative cache keyed by that hash. An
executor-finish or sealing error drops the local value and its provisional batch.

Post-condition:

```text
touched_entities.len == 0
touched_collections.len == 0
all pending entity/collection entries touched by B are Untouched
last_sealed_root == computed R_sealed(B)
the only persistent compressed-entity EVM change is last_sealed_root
```

Any tree, root-comparison, cleanup, state-flush, or executor-finish error aborts block construction/validation and discards the staged tree batch. A partially sealed block is never produced or accepted.

### 8.3 Performance boundary `[Q8 decided; Q11 numeric limits]`

Seal work is part of full block execution. Each domain-version collection has a fixed power-of-two shard count;
the touched collection tops, Root Catalog update, and final `R_sealed` are bounded by block resource limits.

The variable cost is Poseidon byte hashing, worst-case single-shard SMT apply, journal cleanup, state-root notification, and tree-storage access.

Every mutation must prepay a deterministic gas charge for its deferred seal work, and the block must enforce explicit mutation/key/byte limits. Exact values are not guessed in Q8.

`Q11` derives the numerical limits from a mandatory worst-case benchmark on minimum supported validator hardware.

Under the default timing contract, a gas-saturated full block must execute in less than 2 seconds, leaving the remaining certification window for propagation and votes.

---

## 9. Collection-sharded commitment `[Q7 superseded in part by Q23]`

### 9.1 Collection shards `[Q23 decided]`

```text
K_domain = collection_shard_count = 2^k
shard_index = low_k_bits(tree_key_f)
```

Every collection of a domain version uses the same fixed `K_domain`. Exact values for Tribute/Nod are
selected by Q11 benchmark (Gem's is selected at its onboarding fork); domains need not use the same count. Sharding exists for parallel batch execution,
cache locality, snapshot/recovery granularity, and bounded working sets, not security. Worst-case limits assume
all block mutations hit one collection shard.

Each collection shard is an independent in-place Poseidon-BN254 sparse Merkle tree implemented by the vendored
and panic-sanitized CKB `sparse-merkle-tree` engine in the CE-owned MDBX environment. Nodes are namespaced by
`{collection_key, shard_index}`. They do not extend or share Reth's primary database environment.

The vendored engine retains CKB key/path, update/delete, compact-zero, proof, and storage mechanics. Its Outbe-owned typed merge codec is part of commitment scheme v1:

```text
merge(ZERO, ZERO) = ZERO

base_node = P(
  TAG_SMT_BASE;
  base_height, base_key_f, base_value_f
)

normal_node = P(
  TAG_SMT_NORMAL;
  height, node_key_f, left_hash_f, right_hash_f
)

merge_with_zero = P(
  TAG_SMT_ZERO;
  base_node_f, zero_bits_f, zero_count
)
```

`zero_count` preserves the upstream CKB wire semantics exactly: it is `u8` and each increment uses wrapping
addition modulo 256. Therefore a full 256-level compact-zero path encodes `zero_count = 0`, not `256`.

`TAG_SMT_BASE`, `TAG_SMT_NORMAL`, and `TAG_SMT_ZERO` are distinct. Height, path, side/zero information, and child hashes cannot be omitted or reordered.

Every non-zero H256 consumed by this codec must be a canonical BN254 field encoding. Since `tree_key_f < p`, its parent paths and `zero_bits` remain canonical field values.

Structural emptiness is represented by `ZERO`: an empty subtree and an empty shard root are legal `ZERO` values
and legal inputs to a collection top. If any Poseidon output computed over non-empty content — a present leaf,
base/internal node, non-empty shard/collection/catalog root, top node, or sealed root — evaluates to `ZERO`, sealing fails
deterministically. `ZERO` never represents present content.

### 9.2 Collection root, Root Catalog, and sealed root `[Q23 decided]`

The `K_domain` shard roots are leaves of a fixed-depth binary Poseidon tree in ascending shard-index order:

```text
collection_top[0][i] = shard_root[i]              for 0 <= i < K_domain

collection_top[level + 1][j] = P(
  TAG_TOP_NODE;
  level, collection_top[level][2*j], collection_top[level][2*j + 1]
)                                                  for 0 <= level < log2(K_domain)

top_shard_root = collection_top[log2(K_domain)][0]

R_collection = P(
  TAG_COLLECTION_ROOT;
  commitment_scheme_version,
  collection_key_f,
  K_domain,
  top_shard_root
)

RootCatalogSMT.update(collection_key, R_collection)
RootCatalogSMT.update(retired_collection_key, ZERO)
catalog_root = RootCatalogSMT.root()

R_sealed(B) = P(
  TAG_SEALED_ROOT;
  commitment_scheme_version,
  catalog_root
)
```

A never-populated collection has no Root Catalog leaf. A collection emptied by ordinary deletes remains a
*changed* collection per §8.2: it keeps its catalog leaf with `R_collection` computed over an all-ZERO shard
top (a valid non-ZERO hash). Only `retire_partition` deletes a catalog leaf. Consequently, non-membership of
a key in an emptied-by-delete collection is proven through the shard-absence branch with a present catalog
leaf; catalog-absence proves never-populated-or-retired. Retirement deletes the catalog leaf in one consensus
mutation and permanently forbids reuse of that partition key. Shard subtree bytes become uncommitted
immediately and may be reclaimed after finality.

An entity proof contains the shard SMT proof, exactly `log2(K_domain)` collection-top siblings, and a Root
Catalog SMT proof. Directions are derived, never supplied independently. Changing an existing domain's
`K_domain`, partition derivation, leaf order, tags, hash parameters, or node formula requires explicit migration
and a new commitment scheme.

### 9.3 Root carriers `[Q19 decided]`

Genesis height `0` is the sole carrier exception. Its `extra_data` is empty. The genesis EVM state seeds
`0xEE0B.slot1 = R_sealed(0)`, so the genesis state root/hash commits the value; the height-0 CE marker and the
normative derivation from the genesis specification provide the local cross-check described in §17.2.

For every executed block `B >= 1`:

```text
EVM slot at 0xEE0B:
  last_sealed_root = R_sealed(B)

header artifact tag 0x08:
  { commitment_scheme_version, R_sealed(B) }
```

`0x08` is the next unassigned tag in the current `OutbeBlockArtifacts` namespace; `0x07` is already assigned to
committee pre-announcement. Adding the compressed-entity record also requires the corresponding artifact-envelope
version bump. The tag is a wire integration identifier, not part of the Poseidon commitment scheme.

The EVM slot serves contracts. The header artifact serves header/light-client verification without an EVM state
proof. Every validator requires slot, artifact, and locally recomputed root to match for `B >= 1`. A verifier of
height `0` uses the trusted chainspec/genesis hash and the normative derived `R_sealed(0)` rather than a nonexistent
tag-`0x08` record.

A diagnostic mutation count belongs in receipts/metrics unless a concrete consensus consumer requires it.

---

## 10. Finality and reads

### 10.1 Contract-visible state

During every executed block `B >= 1`:

- mutation-aware domain reads use the leaf overlay and observe same-block writes;
- `last_sealed_root` remains `R_sealed(B-1)` until end-block;
- no intermediate tree root exists or is externally observable.

The `read_commitment` interface in §3.1 is this overlay read. This preserves simple lag-1 root semantics while
allowing lag-0 commitment/existence semantics through the registered precompile/domain view. A lag-0 body-level
view exists only if a domain maintains the required body fields in its own consensus state; it is a domain
design choice, not a generic storage-core capability.

### 10.2 External root trust

A verifier chooses a finalized block `H` and verifies it according to the chain's light-client trust model. For
`H >= 1` it extracts artifact `R_sealed(H)`. For `H = 0`, the trusted genesis chainspec/hash and the normative
genesis derivation supply `R_sealed(0)`; genesis intentionally has no tag-`0x08` artifact.

The proof establishes membership relative to that chosen root. An RPC-provided root is not trusted merely because it accompanies a proof.

### 10.3 Point-proof package `[Q7, Q20 decided]`

```text
chain_id
block_height
block_hash
commitment_scheme_version
R_sealed
domain_id
partition_key_or_none
raw_id
schema_version
hash_version
body_bytes
proof_encoding_version
smt_proof
collection_shard_proof
root_catalog_proof
```

Verification:

1. Bind the package identity to the verifier's expected `{domain_id, partition_key_or_none, raw_id}`. For an RPC response these values
   must exactly equal the request; for offline verification they are explicit verifier inputs. A package without
   an independently supplied expected identity proves only the identity stated by that package.
2. Resolve the fork-active ID/partition policy and `K_domain`; derive/validate `partition_key`, `collection_key`,
   `id_bytes`, `tree_key`, and shard index. The RPC/package cannot select any derived locator. Any redundant
   transported value must match exactly.
3. Recompute `leaf_value` from the exact body bytes and versions.
4. Verify `(tree_key → leaf_value)` against the shard root.
5. Verify the shard root through the `log2(K_domain)` collection-top path and recompute `R_collection`.
6. Verify `collection_key → R_collection` through the Root Catalog proof, recompute `R_sealed`, and compare it
   with the selected finalized header.

For entity non-membership, either verify that the collection itself is absent from the Root Catalog or, if it
exists, verify absence in the derived collection shard and both upper paths.

For a present record, `body_bytes` is mandatory. An absent record has no current body.

### 10.4 Freshness

Per `Q7`, v1 guarantees on-demand proof generation only for the latest persisted finalized state.

One proof package is assembled from one MDBX read transaction/snapshot. Its `last_applied` marker, collection,
shard/top/catalog metadata, and all SMT nodes must come from that same snapshot; crossing a tree commit with multiple read
transactions is forbidden.

A proof generated for an older root remains cryptographically valid for that root. A client requesting current state rejects it unless the returned height/root equals the client's chosen current finalized checkpoint.

An in-place tree does not promise later generation of historical proofs.

---

## 11. Query and body projections

### 11.1 Point reads `[Q20 decided]`

`outbe_getBody(domain_id, partition_key?, raw_id, height?)` returns the verification package above or one of the
following results. For `Singleton`, `partition_key` is absent; for Tribute it is derived from `raw_id[0..4]` and
any explicit echo must match:

```text
absent       a valid non-membership proof is returned
unavailable  the node has the commitment/proof capability but not the body bytes
unsupported  the requested historical height/proof version cannot be served
```

`unavailable` is never treated as `absent`.

`absent` is valid only when the non-membership proof verifies for the `tree_key` that the client independently
derives from the requested `{domain_id, partition_key?, raw_id}` and the fork-active registry at the selected height. A node that
lacks body bytes, has stale projection data, or supplies a proof for another identity must return or be treated as
`unavailable`/invalid; it cannot turn that condition into `absent`.

`absent` describes only the selected current root. Historical receipts, when retained, distinguish never-minted keys from deleted keys.

Any node may return `unavailable` when the selected root proves presence but matching local body bytes are not
currently available. This is a local availability failure, not absence and not a consensus-state change. If the
body-store cursor is ahead of the served tree checkpoint, a mismatch may be a newer body rather than missing or
corrupt data; the node first waits for tree catch-up or retrieves the body version for the selected root. It
fetches/rebuilds current bytes from peers, retained events, or snapshot chunks only after cursor alignment still
shows them missing or invalid.

### 11.2 Secondary indexes

`by_owner`, `by_wwd`, media lookup, analytics, and future domain-specific indexes are projection features, not core storage primitives.

This statement concerns the generic RPC/query surface. A domain module may independently maintain whatever
consensus aggregates or indexes its own rules require; their design is outside this storage concept.

Each returned record is individually verifiable when accompanied by its point-proof package.

Without an authenticated-index design, the list has no guarantee of completeness, ordering, or freedom from omissions.

MongoDB is the initial projection adapter, not a protocol dependency.

### 11.3 Finalized ExEx projection `[Q4, Q9 decided]`

The Reth ExEx is a read-only post-consensus projector. It never applies tree mutations and never participates in block validity.

Standard Reth canonical notifications are not finality signals. The projector gates processing on a finalized `{height, block_hash}` stream.

For each finalized target, it reads every missing canonical block and receipt in `high_water+1..finalized_tip`. Gaps are not skipped.

Within a block it applies events in transaction and receipt-log order.

Mint/update events upsert the current row and its current index memberships. A delete event removes the current row and those memberships by canonical entity key.

The idempotency identity is `{block_hash, transaction_index, log_index_in_receipt}`. Equal redelivery is a no-op. A conflicting payload is corruption and stops the projector.

Delete removes the current-body row and current index memberships if present. A missing local row is allowed
because global body-store completeness is not assumed; the canonical event cursor still advances exactly once.

Mongo high-water stores `{height, block_hash}`. It advances only after the whole finalized block is durably applied.

The ExEx sends Reth `FinishedHeight` only after that durable commit. A crash before the marker causes safe block replay.

This durability gate deliberately couples projector progress to Reth retention on that node. While MongoDB is
unavailable, `FinishedHeight` does not advance, Reth pruning is held back, and the ExEx notification WAL grows.
Operators must monitor and provision this backlog; it is durability backpressure and local availability risk,
not a consensus-state dependency.

This cursor is independent of the persistent-SMT marker. It neither authorizes tree advancement nor delays the
Marshal execution acknowledgement. It proves that the projector processed a contiguous event range, not that
every current body row is locally present forever.

Unknown event versions, malformed core events, body/leaf mismatch, and finalized hash conflicts fail closed.

A local ExEx or Mongo failure cannot alter consensus state. It causes projection lag and recovery by replay.

If MongoDB lags or loses a row, affected point reads return `unavailable` until replay or peer recovery succeeds.

---

## 12. Body and media availability

### 12.1 Bodies `[Q1 decided; Q10 snapshot mechanics]`

At block inclusion, mint/update execution produces the canonical body and the successful receipt publishes it in the full-body event.

Launch profile at finalized checkpoint `H`:

- every validator deployment retains current bodies and provides point body/proof service capability through
  enabled interfaces; the signing host itself need not expose a public Internet endpoint;
- every returned body is checked against the current leaf before it is served;
- missing bytes produce `unavailable` and a local recovery attempt;
- snapshots may carry current bodies but do not prove that no body is missing;
- a validator may discard history according to the active retention policy and is not required to be an archive node;
- an archive-profile node, if operated, retains the full blocks/receipts history from genesis;
- MongoDB may implement the current-body/index store, but no database product or cursor becomes a body-completeness authority.

There is no objective proof that a node still possesses every current body. Missing data may remain latent until
the corresponding key is read or used by a future body-dependent subsystem.

Retrieval requires at least one reachable provider with the requested current bytes.

The protocol does not guarantee that an archive provider exists, is reachable, or serves historical bodies forever.

### 12.2 Media

Media bytes never enter the consensus ledger. A body contains a content hash or chunked-manifest root.

```text
R_sealed → leaf → body → media commitment → fetched bytes
```

Integrity is independently verifiable. Availability is best effort. Withholding at mint and total later loss are possible unless a domain adds an external availability policy.

---

## 13. Persistent tree commit, reorg, and crash recovery

### 13.1 Speculative state `[Q9 decided]`

Execution of non-finalized candidates produces immutable in-memory staged tree batches keyed by block hash.
Child execution reads through the staged ancestor chain. Losing branches are discarded without touching
persistent SMT tables.

Staged batches are reconstructible cache, not recovery authority. A missing batch can be rebuilt from the latest
persisted finalized tree plus the ordered candidate blocks. Retention is bounded by branch/count/byte limits
derived from the consensus candidate window; `Q11` sets their numerical values after benchmark.

### 13.2 Persistent ordering `[Q9, Q14, Q16 decided]`

Before block 1 execution, the CE-owned MDBX is initialized or deterministically rebuilt with the verified genesis
checkpoint from §17.2. Its height-0 marker is the only marker allowed to use `ZERO` parent fields.

The in-place persistent tree advances only after all of the following are true for block `B`:

1. Commonware Marshal has durably synced the block and its finalization certificate;
2. Reth has accepted execution and the finalized forkchoice update;
3. Reth has durably persisted the canonical block, receipts, and EVM state through `B`;
4. the durable canonical hash and on-chain `last_sealed_root` equal the staged batch metadata.

A normal Reth canonical notification, successful `new_payload`, or successful FCU is not a durable persistence
barrier. With compressed storage active, Reth must start with `persistence_threshold = 0` and
`memory_block_buffer_target = 0`; incompatible configuration fails startup. After finalized FCU for `H`, the
coordinator waits for `PersistedBlockSubscriptions`, then uses a DB-only provider to verify
`persisted_tip >= H`, the exact canonical block hash, durable block/receipts/EVM state, and
`last_sealed_root(H)`. Only then may SMT commit begin.

Required invariant:

```text
persistent_tree_height <= min(durable_evm_height, consensus_finalized_height)
```

For finalized block `B`, one transaction in the CE-owned MDBX environment writes:

```text
all changed tree nodes
all current collection shard roots / collection-top metadata / Root Catalog metadata
last_applied = {
  commitment_scheme_version,
  height: B,
  block_hash,
  parent_block_hash,
  parent_root,
  new_root: R_sealed(B)
}
```

The marker and nodes are atomic. The transaction requires contiguous height, matching parent block/root, and
matching commitment scheme. Applying the same complete marker is an idempotent no-op; a conflicting marker is
corruption.

The CE environment stores identity metadata binding it to at least `{chain_id, genesis_hash,
commitment_scheme_version}` and fails startup on mismatch. It owns its schema, map-size/capacity checks, writer
lifecycle, and proof-read snapshots independently from Reth. Copying only this directory is not a consistent
full-node backup unless the paired Reth checkpoint is known.

Only after this transaction commits does the executor acknowledge `B` to Marshal. While compressed storage is
active, finalized delivery is ACK-gated and `MAX_PENDING_ACKS = 1` is a protocol-required startup invariant, not
operator tuning. Marshal does not deliver the next finalized block before the previous ACK; the persistent SMT
marker can therefore lag the durable finalized Reth checkpoint by at most one in-flight block.

The one-in-flight-block bound applies to live ACK-gated operation. During §14.5 snapshot bootstrap (import at
`H`, replay `H+1..head`) an arbitrary contiguous lag is legitimate: the `last_applied` marker is the durable
progress cursor, and a crash mid-replay resumes through the §13.3 behind-row without any separate bootstrap
staging state. While behind, the node does not propose, validate, or serve proofs beyond its marker height.

The complete order is:

```text
Marshal durable block + finalization
  -> Reth execution + finalized FCU
  -> forced durable Reth block/receipt/EVM checkpoint
  -> atomic SMT nodes + marker
  -> Marshal ACK
```

This ordering deliberately allows only derived-state lag. It never permits the in-place tree to move ahead of
the durable EVM state needed to reproduce the same execution view.

### 13.3 Restart matrix

```text
tree marker == durable finalized EVM checkpoint
  verify height/hash/root/scheme equality; equal redelivery after a crash between SMT commit and Marshal ACK
  is an idempotent no-op, then ACK.

tree marker behind durable finalized EVM checkpoint
  this is the allowed crash after Reth commit but before SMT commit;
  rebuild batches from durable finalized canonical core events/receipts;
  apply every missing height contiguously and verify each committed root.

tree marker ahead of durable EVM state or consensus finality
  impossible invariant violation; halt and resync.

same height, different block hash, scheme, or root
  corruption/invariant violation; halt and resync.

next marker does not match parent block/root
  gap or corruption; halt incremental apply and recover from a verified checkpoint.

root mismatch at recorded marker
  corruption; do not repair in place; restore from a snapshot anchored to an independently verified finalized root.
```

Thus the two post-write crash windows are intentionally asymmetric:

```text
Reth = H, SMT = H-1  → recover SMT H from durable receipts, verify root, commit, ACK.
Reth = H, SMT = H    → verify equal marker, idempotent no-op, ACK.
```

`Reth = H-1, SMT = H` is forbidden by the durable barrier. If observed, the node fails closed and resyncs; it
does not propose/validate or serve proofs. This is bounded local recovery. A single node failure cannot change
the already-finalized network state while the remaining validator quorum continues.

The node must verify the local parent tree root against the parent block's committed root before proposing, validating, or serving a proof.

Current-body persistence is not part of the Marshal/SMT acknowledgement critical path. Mongo high-water records
contiguous event processing, but it cannot prove that every current row remains present on disk.

```text
proof_ready_height = persistent_tree_height
```

The §13.2 invariant already requires the persistent tree not to exceed consensus finality or durable EVM state;
an ahead/conflicting marker is a fail-closed restart condition rather than a proof-serving checkpoint.

At `proof_ready_height`, a point body response is served only if the concrete local bytes recompute to the
current leaf; otherwise it returns `unavailable`. If the body-store high-water is ahead of `proof_ready_height`,
the node treats a mismatch as possible temporal skew: it waits for tree catch-up or retrieves the historical body
for the served root rather than repeatedly fetching the same newer current body. After cursor alignment, a
remaining missing/mismatched body is recovered by replay from the applicable event range or another source. No
global materialization scan is required.

Secondary MongoDB indexes recover independently from the same finalized mutations with idempotent upserts.

A slow or failed secondary-index projection never blocks consensus persistence. It may degrade list queries, but not point body/proof correctness.

---

## 14. Snapshot and bootstrap

### 14.1 Semantic snapshot contract `[Q10, Q17 decided]`

A snapshot at finalized height `H` is a resumable node-recovery carrier:

```text
snapshot header:
  snapshot_format_version
  profile, body_coverage
  chain_id, genesis_hash
  H, block_hash
  commitment_scheme_version, R_sealed(H)

successful import:
  same canonical set (collection_key, shard_index, tree_key, leaf_value)
  same collection shard roots and collection roots
  same Root Catalog
  same R_sealed(H)
  same canonical current bodies in the declared body_coverage
```

The snapshot format is semantically deterministic, not a canonical physical database image. Given the same
finalized checkpoint, format version, profile, and `body_coverage`, conforming producers represent the same
logical records and a successful import reconstructs the same authenticated state and covered bodies. MDBX
pages, map size, allocator history, insertion order, compression, container bytes, file names, and derived
MongoDB indexes may differ.

The receiver independently selects the finalized header and checks `{chain_id, genesis_hash, H, block_hash,
commitment_scheme_version, R_sealed(H)}` against it. A snapshot, producer signature, manifest, checksum, or object
store is never a new trust root and never replaces finality verification.

`snapshot_format_version` normatively fixes record kinds, field widths, byte order, canonical encoding,
uniqueness and ordering rules, logical range addressing, and import semantics. Unknown versions, profiles, or
commitment schemes fail closed; silent downgrade is forbidden.

### 14.2 Logical records and local materialization `[Q17 decided]`

The portable network format describes logical state rather than MDBX internals:

```text
leaf record:
  collection_key, shard_index, tree_key, leaf_value

body record:
  domain_id, partition_key_or_none, schema_version, hash_version,
  canonical id bytes, canonical body bytes,
  expected tree_key, expected leaf_value

logical range:
  profile, payload_kind, collection_key, shard_index, start_key, end_key
```

`shard_index` must equal the index derived from `tree_key` and the registered `K_domain`; mismatch rejects the
record. The body record carries `schema_version`/`hash_version` (mirroring the §10.3 package) so the importer
can recompute `leaf_value` even when §16.1 keeps more than one schema version readable at `H`. Records are ordered by payload kind, collection, shard, and key. Duplicate keys, conflicting records, out-of-order input,
non-canonical encodings, and invalid range continuation are rejected. Independent producers may package records
differently, but must return the same ordered records for the same logical range and checkpoint.

Persistent internal SMT nodes are derived materialization. A snapshot may contain a versioned acceleration
section with internal nodes, but an importer may discard it and rebuild from normative leaf records. Acceleration
data never substitutes for checking all reconstructed collection shard roots, collection roots, Root Catalog,
and final `R_sealed(H)`.

The importer writes into staging. The snapshot becomes active only after its checkpoint identity and reconstructed
root match the independently selected finalized header. Missing or corrupt tree data therefore causes a local
bootstrap/proof failure; under collision resistance it cannot make an incorrect state equal the finalized root.

### 14.3 Snapshot profiles and body semantics `[Q10, Q17 decided]`

The format distinguishes the following versioned profiles:

1. `tree` contains the complete current collection/leaf set needed to reconstruct all collection shard roots,
   Root Catalog, and `R_sealed(H)`.
2. `tree-with-bodies` carries body records in addition to the tree and declares `body_coverage` as canonical
   logical ranges. Inside each declared range, the importer requires exactly one canonical body for every present
   leaf. Different coverage produces a different snapshot identity. The manifest is still not a consensus proof
   that the producer possesses every current body outside those ranges.
3. An implementation may expose `full-current-body` as an operational profile. Its exporter and importer perform
   a one-time streaming leaf-to-body merge and require exactly one canonical body for every present leaf. This
   `O(N)` snapshot job is not a permanent startup/readiness scan and not a consensus predicate.
4. Partial or lazy body bundles use distinct profile/coverage and artifact identity. They do not claim that the
   imported node is immediately capable of serving every current body.

Stage 1 testnet note (Variant A): the `tree` profile never qualifies a validator as operationally ready —
validator readiness requires `tree-with-bodies` whose declared coverage spans all present leaves of all
active Tribute partitions (the active set is enumerated by domain-owned consensus state, not by the Root
Catalog, whose keys are one-way hashes).

Outside the explicit `full-current-body` profile, missing body bytes may remain latent until access. When a body
is imported, returned, or used, the node derives its canonical identity, `tree_key`, and `leaf_value` and compares
them with the selected current tree. Missing or mismatched bytes produce `unavailable` and trigger
peer/event/snapshot recovery. This is the local availability limitation accepted in `Q10`, not a
consensus-integrity gap.

### 14.4 Manifests, chunks, and multi-source recovery `[Q17 decided]`

A manifest describes one physical transport artifact:

```text
per chunk:
  logical payload kind and range
  encoded and decoded sizes
  transport checksum / content ID

manifest:
  checkpoint, profile, and body_coverage
  ordered chunk descriptors
  total range-coverage metadata
```

Chunk boundaries, compression, container, and file names may be manifest-local. Checksums and content IDs are
calculated over the canonical decoded chunk payload so transport encoding cannot change its meaning.

Byte-level resume and arbitrary chunk mixing are guaranteed between mirrors that serve the same manifest.
Independent producers may publish different manifests for the same semantic snapshot. Cross-producer failover
therefore occurs at canonical logical range and continuation-key boundaries, not by assuming that physical chunk
number `N` is interchangeable. A receiver may repeat a missing or suspect logical range against another producer;
the final reconstructed root remains the authoritative completeness check.

Snapshot producers, validators, archive peers, object stores, and mirrors are untrusted byte sources. Bulk
snapshot service need not be publicly exposed by every signing host. Recovery assumes at least one reachable
source has the required bytes.

Import rejects malformed, duplicate, overlapping, conflicting, or out-of-range records; gaps; checksum mismatch;
checkpoint mismatch; root mismatch; and resource-limit violations. Parsers impose explicit bounds on manifest
bytes and entry count, encoded and decoded chunk size, record length, decompression ratio, temporary disk,
concurrency, and time. Artifact identifiers are data and are never interpreted as filesystem paths.

### 14.5 Bootstrap, pruning, and physical relocation `[Q10, Q16, Q17 decided]`

Bootstrap paths are:

1. Logical tree/body snapshot at finalized `H`, followed by replay of retained canonical events from `H+1` to
   head.
2. Full replay of canonical history when every active domain runtime can replay its transaction inputs.
3. Lazy recovery of an individual missing body from peers, retained events, or snapshot chunks.

A snapshot at `H` may be advertised as bootstrap-capable only while the available canonical event/receipt tail
covers every height in `H+1..head`. Before pruning breaks that tail, an operator must retain it, publish/obtain a
newer usable snapshot, or stop advertising the older snapshot as independently bootstrap-capable. This is an
operational availability invariant, not a requirement that every validator retain genesis history.

A raw MDBX/datadir copy is a separate node-local backup and relocation mechanism, not the network snapshot
format. It is valid only from a stopped node or a database-native consistent checkpoint. A complete relocation
bundle must align the durable Reth checkpoint and CE tree marker at the same `H`; body materialization is copied
with its own durable high-water or rebuilt from canonical events/snapshot after that cursor. Copying only
`<datadir>/compressed_entities/smt/` is not a consistent full-node backup.

Validator private keys, signer state, node identity, live locks, and ephemeral caches are not part of a portable
snapshot. Operational controls must prevent the original and a clone from concurrently signing with the same
validator key.

After history pruning, the system still verifies any available current body against the current root. It does not
guarantee recovery of deleted/superseded historical bodies or of current bytes whose last copy was lost.

Ring/MMR does not prove body availability or snapshot completeness. Historical-root/proof retention after pruning
belongs to a future pruning design and is not part of storage v1.

Domain-specific replay prerequisites are owned by those domains and do not change the compressed-storage recovery
contract. Future off-chain computation separately defines behavior when body-dependent inputs are unavailable.

---

## 15. Gas, quotas, and liveness

### 15.1 Provisional implementation guard `[Q11 numerical closure open; Q15 decided]`

Until the mandatory benchmark exists, the first implementation uses the following temporary guard:

```text
CE_MUTATION_GAS_PROVISIONAL = 50_000 per attempted core mint/update/delete/retire_partition
MAX_CE_MUTATION_ATTEMPTS_PER_TX_PROVISIONAL = 600
MAX_CE_MUTATION_ATTEMPTS_PER_BLOCK_PROVISIONAL = 600
```

plus a conservative provisional bound set (all values `PROVISIONAL_Q11`, replaced by the benchmark
closure): per-operation body bytes per domain, aggregate body/calldata/event bytes per block, unique keys
per block, staged-tree bytes, speculative-cache count/bytes, and the system-lane resource policy. Runtime
integration never starts with unbounded inputs; the attempt cap alone does not bound a single oversized
body or staged batch.

The per-transaction and per-block attempt counters are executor-local, non-persistent, and non-journaled resource
guards; they are not EVM state and do not enter any root or header artifact. Proposer and validator execution
recompute them deterministically over the same sequential call path.

Domain authorization and business validation occur before a CE attempt. Entry into generic core
`mint/update/delete/retire_partition` atomically checks/reserves one per-tx slot, one per-block slot, and the fixed gas charge. If
gas or quota reserve fails, the attempt did not start; a per-operation body-size rejection happens at the
same pre-reserve stage and equally means the attempt did not start — no attempt slot is consumed and no
charge is taken (postfix PF-H03: this pins the boundary the per-attempt wording alone left ambiguous).
After successful reserve, generic lifecycle rejection,
nested revert, or full transaction revert does not remove the attempt from the block counter when that
transaction is included.

Reserve classification has a normative order. Gas sufficiency is checked first, then the per-transaction cap,
then remaining per-block capacity. Any entry that exceeds the per-transaction cap is always
`TransactionLimitExceeded`, even when the same entry would also exceed the remaining block capacity.
`BlockCapacityExhausted` applies only after the transaction remains within its own cap.

The charge is deducted inside the core before hashing, journal writes, or event emission. It is per mutation,
not per transaction or precompile dispatch, and is additional to ordinary dispatch/storage gas. Revert removes
state and logs but does not refund performed computation.

The block attempt cap spans user and system paths. This is required because receipt-visible system calls execute
with an internal `10_000_000_000` gas lane rather than the normal `30_000_000` user block lane. Only the current
bounded/fixed Tribute and Nod canonical schemas may use the provisional flat price.

Payload-building behavior is part of the contract:

```text
tx fits remaining CE block budget
  -> execute normally

tx fits an empty block but not the remaining CE budget
  -> rollback speculative tx, return BlockCapacityExhausted,
     keep tx in pool for the next block

tx requires more than 600 CE attempts in an empty block
  -> TransactionLimitExceeded; reject/revert, never defer forever
```

With the provisional values, ordinary dispatch/storage gas makes the 600-attempt branch unreachable on the
normal 30M user lane: user execution reaches OOG first. The explicit attempt cap remains effective for the
10B receipt-visible system lane. Final lane behavior is recalibrated with the Q11 benchmark.

`BlockCapacityExhausted` is not `InvalidTransaction`: an honest builder must not evict the deferred transaction.
A validator rejects a proposed block that crosses the cap. System bulk work must split across blocks with a
deterministic progress cursor.

Attempts performed by an included transaction count even if that transaction later reverts, because the work was
already consumed. When a speculative transaction is excluded from the payload, the builder restores the local
attempt counter to its pre-transaction checkpoint together with execution state.

The first entry beyond the per-tx cap fails reserve with `TransactionLimitExceeded`; its preceding attempts
remain counted if the reverted transaction is included. Block-capacity overflow is different: the proposer
excludes the whole speculative transaction with `BlockCapacityExhausted` and restores its counter checkpoint,
while a validator encountering the same overflow in a proposed payload rejects the whole block without creating
a receipt for that overflow.

The provisional constants are not activation evidence and do not settle the final limit structure or gas formula.

### 15.2 Mandatory performance benchmark `[fixed by Q8; Q11 numerical closure remains open]`

Normative benchmark requirements are tracked in `compressed_entities_v6_performance_benchmark_requirements_10-07-2026.md`.

Numerical limits are selected iteratively: choose an explicit candidate limit set, construct the saturated
worst-case workload at exactly those limits, measure it, and accept or reduce the candidates until the target
and safety margin hold. The reproducible workload includes:

- every entity mutation is a new key;
- every entity mutation falls in one collection shard;
- collection retirement and Root Catalog updates are included;
- bodies are at maximum size;
- Poseidon byte hashing, leaf construction, SMT node hashing, top-root computation, and journal cleanup are included;
- `OnStateHook(StateChangeSource::PostBlock(StateChangePostBlockSource::Other(...)))` state-root notification is included;
- proof reads and MDBX persistence run concurrently;
- the node uses minimum supported validator hardware.

The benchmark must output:

```text
max_unique_keys_per_block
max_ce_mutation_attempts_per_tx
max_ce_mutation_attempts_per_block
aggregate body/calldata/event byte limits
deferred-seal gas charge per operation/byte/key
max_staged_tree_bytes
```

Acceptance target under the default consensus timing contract:

```text
gas-saturated full_block_execution_time(minimum_validator_hardware) < 2 seconds
```

The benchmark report, hardware profile, dataset shape, cache state, commands, raw results, and safety margin are activation evidence.

Changing the validator hardware floor, gas limit, hash implementation, SMT codec, or persistence path requires re-running it.

ZeroFee admission never replaces execution limits. Free writes still consume permanent DA and tree capacity.

---

## 16. Versions and evolutionary extension

### 16.1 Independent version axes `[Q12 decided]`

```text
schema_version
  body field layout and meaning

hash_version
  body-to-leaf preimage rule within a commitment scheme

proof_encoding_version
  RPC/wire proof bytes

commitment_scheme_version
  collection key, key/value, per-domain shard/top, Root Catalog, empty-tree, and tree-hash semantics

domain_runtime_version(height)
  registered methods, authorization, body production, gas, and DA profile
```

Callers do not freely select obsolete versions. Active/accepted versions are resolved from domain registry and block height. Upgrade transitions specify whether an old-schema record remains readable, migrates on update, or is bulk-migrated.

### 16.2 Cheap extensions

The following do not require a new commitment scheme when existing key/value rules suffice:

- a new domain;
- a new body schema;
- a new derived index;
- a new media commitment type inside a schema;
- a new proof transport envelope that still proves the same tree.

### 16.3 Commitment-scheme changes

Changing `K`, Poseidon parameters, byte-to-field encoding, key derivation, leaf/value formula, empty-tree
semantics, tree topology, or top-root function increases `commitment_scheme_version`.

Such a transition is a separate hard-fork design with its own migration mechanics, activation plan, and
acceptance evidence. Storage v1 defines the version seam but does not pre-design a migration for a hypothetical
future scheme.

Off-chain computation and historical-root accumulators attach at future seams. They do not change v1 storage semantics until separately specified and activated.

---

## 17. Genesis activation

### 17.1 Greenfield launch `[Q12 decided]`

Mainnet starts from genesis with Compressed Entity Storage v1 and `commitment_scheme_version = 1` active.
Stage 1 note (postfix PF-L02): this subsection describes the eventual mainnet launch shape; Stage 1
activates CES on TESTNET only — T14/T25 produce testnet evidence, and the production/mainnet gate remains
a separate OPEN gate (§3.2 Variant A discipline).
The existing testnet is wiped before the new implementation starts; its Tribute, Nod, and Gem state is not
migrated (Tribute/Nod restart greenfield on CES; Gem restarts on its unchanged legacy storage, §3.2).

Therefore v1 has no legacy snapshot, freeze height, dual-write window, catch-up replay, storage-migration
activation height, migration manifest, or retirement of legacy EVM body slots. Domain-registry activation
heights remain: genesis domains activate at height `0`, and later versions activate at their fork height.

### 17.2 Genesis root and carriers `[Q12, Q19 decided]`

Genesis commits `R_sealed(0)` through EVM state and does not contain a tag-`0x08` header artifact. Its
`extra_data` remains empty, matching the existing Outbe genesis bootstrap convention.

The genesis alloc contains:

```text
address 0xEE0B:
  code   = 0xef
  slot 0 = 1             // storage_schema_version
  slot 1 = R_sealed(0)   // derived, never independently configured
```

`0xEE0B` is also included in the runtime EIP-161 marker set. Slots 2–5 entity/collection pending maps and touched
vectors are structurally empty.

During initialization every node:

1. Canonicalizes any genesis entities and derives all leaves using the fork-active genesis domain/schema versions.
2. Builds any configured genesis collections, their per-domain shard layouts, the genesis Root Catalog, and
   derives `R_sealed(0)` using commitment scheme v1 and the Q18/Q23 tag registry.
3. Requires exact equality between the derived root, seeded slot 1, and the genesis chainspec/state. The root is
   not an independent operator-supplied parameter; any mismatch rejects genesis/startup fail-closed.
4. Initializes or rebuilds the CE-owned MDBX with:

   ```text
   last_applied = {
     commitment_scheme_version: 1,
     height: 0,
     block_hash: genesis_hash,
     parent_block_hash: ZERO,
     parent_root: ZERO,
     new_root: R_sealed(0)
   }
   ```

   `ZERO` parent fields are valid only at height 0.

Block 1 begins with the EVM slot and CE marker equal to `R_sealed(0)`. Its end-block seal derives
`R_sealed(1)`, writes slot 1, and emits the first tag-`0x08` artifact. Validator execution requires the genesis
parent hash/root and then the computed block-1 root, EVM slot, and artifact to match exactly.

With no genesis entities this procedure produces the deterministic empty root. A future preloaded genesis uses
the same path for its canonical bodies/tree; it does not use synthetic events or migration machinery.

---

## 18. Implementation placement

The proposed system retains the useful v6 integration shape:

| Concern | Placement |
|---|---|
| reserved root/journal state | system address `0xEE0B`; slot 0 schema version, slot 1 root, slots 2–5 entity/collection pending maps and touched vectors; genesis `0xef` plus runtime EIP-161 marker |
| internal domain-to-store interface | `crates/core/compressed_entities` |
| vendored SMT implementation | private implementation inside the store module |
| end-block seal | `BlockLifecycle::end_block()` has an associated typed result; one shared `CompressedEntitiesLifecycle` returns `SealOutput` as the last consensus end-block lifecycle step before state-root calculation |
| header commitment | `OutbeBlockArtifacts` tag `0x08` plus artifact-envelope version bump |
| finalized persistence coordinator | Marshal delivery → Reth durable checkpoint barrier → SMT commit → Marshal ACK |
| persistent tree commit | separate CE-owned MDBX at `<datadir>/compressed_entities/smt/`; namespaced collection shards plus Root Catalog; one atomic transaction over nodes, roots, and complete `last_applied` marker |
| current-body materialization | MongoDB or another local store; per-key verification and explicit `unavailable` semantics |
| finalized projection | new Reth ExEx; finalized-gated canonical receipts → idempotent MongoDB block apply |
| ExEx cursor | durable `{height, block_hash}` high-water before Reth `FinishedHeight` |
| proof/body RPC | `outbe_*` namespace |
| domain wiring | registered tribute/nod runtime adapters (Gem deferred, §3.2) |

The proposer and validator call sites must exercise the same store interface and share golden state-transition vectors.

---

## 19. Acceptance evidence `[Q13 decided]`

Every group below is a mandatory release gate before mainnet activation:

1. Independent reference model: ordered valid mutations → final map → commitment.
2. Golden vectors for Poseidon tags/parameters, byte-to-field chains, ID, key, body encoding, non-zero leaf,
   pending-slot `Untouched`/`Set`/`Deleted` encoding and invalid reserved words,
   every CKB merge form including `zero_count` 255→0 wrapping at depth 256, delete sentinel, singleton and
   partitioned collection keys, per-domain shard counts, empty collection/catalog, the emptied-by-delete
   ZERO-top `R_collection` leaf, collection/root-catalog proofs, partition retirement, event
   `partition_key_or_none` presence/absence, sealed root, event, and proof.
3. Differential tests between the vendored SMT and the reference model.
4. Proposer/validator cross-architecture root equality.
5. Nested call/revert/OOG/static/delegate/callcode/reentrancy adversarial suite.
6. Body/event/leaf coherence and zero-sentinel rejection tests at the store interface.
7. Wrong emitter, unknown/inactive domain, public core call, schema downgrade, stale-proof, wrong-ID,
   RPC-selected-ID-encoding, and mismatched redundant-identity tests.
8. Per-domain ID generator vectors for determinism, collision/repeat rejection, and failed-mint rollback.
9. Raw-hook mutation rejection and receipt-visible system-mutation tests.
10. Finality-gate, duplicate delivery, gap replay, delete-row consistency, and conflicting-cursor ExEx tests.
11. Crash fault injection before/after Marshal archive sync, Reth FCU, Reth durable persistence, SMT transaction,
    Marshal ACK, Mongo block commit, high-water update, and `FinishedHeight`; complete restart-matrix tests.
12. Snapshot semantic-conformance tests across independent exporters and different MDBX layouts; logical-range
    multi-source failover; manifest-local byte resume; omission, duplicate, overlap, reorder, corruption,
    malicious-manifest, downgrade, decompression-bomb, lazy missing-node, and per-key body-recovery tests.
13. Reproducible performance report required by §15.2, proving the gas-saturated full-block target on minimum validator hardware.
14. Worst-case single-collection/single-shard throughput, multi-collection parallel preparation, MDBX growth,
    partition-retirement namespace reclamation, journal cleanup, typed `SealOutput` handoff/drop paths,
    `OnStateHook(StateChangeSource::PostBlock(StateChangePostBlockSource::Other(...)))` notification, and
    concurrent proof-serving/tree-commit benchmarks.
15. Genesis initialization rehearsal for the production genesis specification, including deterministic empty
    root verification, `0xEE0B` slot/layout/marker checks, height-0 CE marker rebuild, absence of tag `0x08` in
    block 0, block-1 parent/root-carrier checks, and any explicitly configured genesis entities.
16. Differential delete tests: branch cleanup, non-membership, same-key sequences, nested revert, and OOG.
17. Property-based and coverage-guided fuzzing for mutation sequences, canonical event/proof/snapshot codecs,
    malformed lengths, unknown versions, and resource-boundary inputs.
18. Sustained testnet soak covering full-block compressed-entity load, validator restarts, finalized catch-up,
    snapshot bootstrap, cross-source resume, consistent datadir relocation, ExEx replay, local body loss, and
    continued point-proof serving.
19. Body-store-ahead-of-tree and tree-ahead-of-body cursor-skew tests, including proof-height body selection,
    catch-up without futile peer fetch, and recovery after aligned mismatch.

Tests target the module interface and observable roots/events/errors, not private tree internals except for differential implementation conformance.

This package does not require complete formal verification. Final numerical performance thresholds are supplied
by `Q11`. The soak duration and exact workload belong to the release plan: they are mandatory operational
evidence, not consensus constants.

---

## 20. Remaining closure condition

`Q1`–`Q10` and `Q12`–`Q23` are closed. `Q11` remains numerically provisional until the required benchmark
confirms or replaces the temporary gas/capacity values.

No off-chain computation decision is required to complete this storage system.

---

## 21. One-line summary

Compressed Entity Storage is an internal consensus module reached only through fork-designated domain entrypoints.

It turns validated NFT-body writes, deletes, and partition retirements into domain-versioned collections. Each
collection uses a fixed power-of-two set of in-place Poseidon-BN254 shard SMTs; their collection roots are
committed through a dynamic Root Catalog SMT into one versioned `R_sealed` stored in EVM state and finalized headers.

Every successful mutation publishes a canonical receipt event; mint/update events contain the full body. A finalized-gated ExEx rebuilds MongoDB without re-executing domain logic.

The system serves verifiable point reads through untrusted adapters. MongoDB, media storage, domain internals, and future off-chain computation never become storage authorities.
