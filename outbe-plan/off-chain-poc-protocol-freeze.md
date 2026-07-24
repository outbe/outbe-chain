# Off-chain PoC: protocol-byte and capacity freeze

Status: **resolved decision asset for ticket #4**

Scope: the disposable-devnet `LysisProgramV1` PoC only

Date: 2026-07-23

This asset freezes the protocol decisions that must precede implementation. It
does not implement the protocol and it does not introduce a generic off-chain
program registry.

The normative inputs are:

- [`off-chain-poc.md`](../off-chain-poc.md);
- [`off-chain-computation.md`](../off-chain-computation.md);
- [`ADR-S-OCM-001`](../docs/adr/system/ADR-S-OCM-001-ocomp-kernel-and-typed-program-boundary.md)
  through
  [`ADR-S-OCM-004`](../docs/adr/system/ADR-S-OCM-004-certified-activation-job-fsm-and-protocol-versioning.md);
- [`ADR-B-WIR-001`](../docs/adr/blockchain/ADR-B-WIR-001-protocol-identifiers-and-consensus-wire-contract.md);
- [`ADR-B-CRY-001`](../docs/adr/blockchain/ADR-B-CRY-001-cryptographic-profiles-namespaces-and-canonical-codecs.md);
- [`ADR-B-CAP-001`](../docs/adr/blockchain/ADR-B-CAP-001-resource-metering-and-capacity-closure.md);
- the frozen
  [`Lysis V1 semantic baseline`](off-chain-poc-lysis-v1-semantics.md).

## 1. Decision

PoC uses one closed, fork-pinned protocol:

```text
OCOMP lifecycle kernel
  + one LysisProgramV1
  + one strict OCOMP canonical binary codec
  + one normal Ethereum activateLysis transaction
  + one static 3-of-4 secp256k1 result committee
  + constant-size transaction-carried result commitment
```

There are two different classes of values:

1. **Semantic constants fixed by this decision**: identities, encoding grammar,
   object tags, field order, domains, signature rules, list-root construction,
   `n/f/q`, deadline and candidate bounds.
2. **Generated literals**: genesis hash, protocol bundle hash, semantic artifact
   hashes and final measured capacity values. These cannot honestly be chosen in
   a planning document. A mandatory freeze generator produces and checks them in
   before any dependent runtime implementation is accepted.

A generated value is not an open product decision. Its derivation, input,
output schema and acceptance gate are fixed below. No runtime `TODO`, zero hash,
environment override or “use current default” may stand in for it.

No grilling is required for this ticket. The ADRs, current production wire
patterns and the disposable-devnet constraint determine the choices.

## 2. Fork and profile identity

Identifiers are `B256 = keccak256(exact UTF-8 token bytes)` with no prefix,
terminator, normalization or surrounding whitespace:

| Purpose | Exact token | Exact ID |
|---|---|---|
| PoC fork | `OUTBE_OCOMP_POC_DEVNET_FORK_V1` | `0x1d1378c5bf42070e0326f13d3094f4b7ed3ef42c63edb3179745045f65027eb2` |
| correctness profile | `OUTBE_OCOMP_LYSIS_V1` | `0x0469002bbc5065d8e9d200daede2a65ec479cad78210d7dadabdd5320d241c8d` |
| capacity profile | `OUTBE_OCOMP_POC_CAPACITY_V1` | `0x7c9614691067b57829fba66f37e02860529ab1a24497dbb8f5b53dc5bd0f52df` |

The protocol version is `1`. The reference four-validator E2E chain activates
the fork at block height `32`, allowing assertions at `31`, `32` and `33`.
Other disposable test manifests may move the height only by generating a new
network binding and final chain manifest; a node-local setting cannot move it.

Every protocol object also binds the generated `chain_id`, `genesis_hash`,
`fork_id` and `ProtocolBundleHash` where its schema below says so. The reference
chain ID and genesis hash are outputs of the reproducible base-genesis task,
not copied from an existing network. The static committee is assembled only
after that hash and `ProtocolBundleHash` exist; the final chain manifest binds
both plus the committee snapshot without regenerating the genesis header.

The semantic profile constants are:

| Constant | Value |
|---|---:|
| result validators `n` | `4` |
| tolerated faulty/offline domains `f` | `1` |
| matching signatures `q` | `3` |
| complete-job Tribute ceiling | **none** |
| maximum Tribute per worker shard | `256` |
| workers per validator domain | `1..4` |
| pending parent jobs | `1` |
| intents per block | `1` |
| activations per block | `1` |
| READY inspections per block | `1` |
| expirations per block | `1` |
| retry backoff | `1` block |
| terminal job records | `365` |
| distinct reference currencies | `8` |
| Fidelity cohorts per owner | `64` |
| Oracle WWD pair entries | candidate `256` |
| active Oracle S-curve entries | candidate `256` |
| result deadline delta | `64` blocks |
| source evidence retention after terminal | `64` finalized blocks |
| result-chunk candidate ceiling | `524288` bytes |
| result-summary candidate ceiling | `1048576` bytes |
| action data availability | authenticated result chunks retained by the three signing domains; no independent PoC custody claim |
| proof system | canonical `none` |
| external DA system | canonical `none` |

Worker-shard, chunk, concurrency, Oracle-range and byte ceilings may only stay
the same or decrease after capacity generation. Capacity generation must
preserve an accepted multi-shard boundary case and synthetic 10,000/1,000,000,000
unit-count derivation. No generated or configured field may impose a total
Tribute ceiling.

## 3. OCOMP Canonical Binary V1

### 3.1 Top-level envelope

All signed, hashed, persisted or cross-process semantic OCOMP objects use
`OCB1`:

```text
4 bytes  magic          = ASCII "OCB1"
2 bytes  object_kind    = unsigned big-endian registry tag
2 bytes  schema_version = unsigned big-endian; exactly 1 for this PoC
4 bytes  body_len       = unsigned big-endian
N bytes  body           = fields in the exact schema order
```

The decoder:

- checks the applicable byte/count cap before allocation;
- requires exact magic, registered kind and version;
- requires `body_len` to equal the remaining byte count;
- consumes every byte;
- rejects unknown enum/option tags, invalid booleans, duplicate or out-of-order
  collection entries and trailing bytes;
- re-encodes the decoded value and requires byte-for-byte equality.

`serde`, JSON, `postcard`, protobuf defaults and Rust memory layout are not
consensus codecs. JSON may wrap golden vectors, but its `canonical_hex` member
is the authority.

### 3.2 Scalar and collection grammar

| Type | Canonical bytes |
|---|---|
| `u8/u16/u32/u64/u128` | fixed-width unsigned big-endian |
| `U256` | exactly 32-byte unsigned big-endian |
| `B256` | exactly 32 bytes |
| `Address20` | exactly 20 raw address bytes |
| `EntityId36` | exactly 36 raw bytes: WWD BE4 plus digest |
| `bool` | one byte: `0x00` or `0x01` |
| enum | its declared `u8` or `u16`; unknown values reject |
| `Option<T>` | `0x00` for none; `0x01 || T` for some |
| bounded bytes/string | `u32_be(length) || bytes`; strings are strict UTF-8 or ASCII as declared |
| `Vec<T>` | `u32_be(count) || canonical(T_0)..canonical(T_n)` |
| nested struct | its fields in the listed order, without another OCB1 header |

There are no maps in canonical bytes. Logical maps are encoded as vectors with
a schema-defined strict sort key. Zero values are encoded normally; absence is
represented only by an `Option` tag. No alternate empty/default form is valid.

PoC bulk artifacts use `compression = NONE`. This avoids a second
decompression grammar and expansion-risk surface. A later bundle may add a
bounded compression codec without reinterpreting PoC bytes.

### 3.3 Object-kind registry

The following `u16` tags are permanent for the PoC history:

| Tag | Object |
|---:|---|
| `0x0001` | `ProtocolBundleV1` |
| `0x0002` | `CorrectnessProfileV1` |
| `0x0003` | `CapacityProfileV1` |
| `0x0004` | `PreAdmissionEnvelopeV1` |
| `0x0005` | `ActivationPreconditionsV1` |
| `0x0006` | `JobIntentV1` |
| `0x0007` | `FinalizedIntentProofV1` |
| `0x0008` | `InputManifestV1` |
| `0x0009` | `AuthenticatedInputChunkV1` |
| `0x000a` | `UnitSpecV1` |
| `0x000b` | `UnitArtifactV1` |
| `0x000c` | `ResultChunkV1` |
| `0x000d` | `LysisResultV1` |
| `0x000e` | `ActivationPayloadV1` |
| `0x000f` | `OcompCommitteeSnapshotV1` |
| `0x0010` | `OcompKeyRegistrationV1` |
| `0x0011` | `ExecutionCertificateV1` |
| `0x0012` | `PoCActivationV1` |
| `0x0013` | `ActiveGenerationV1` |
| `0x0014` | `AggregateActivationReceiptV1` |
| `0x0015` | `NodBatchReceiptV1` |
| `0x0016` | `ContributorReceiptV1` |
| `0x0017` | `TributeReceiptV1` |
| `0x0018` | `RequestBudgetSplitReceiptV1` |
| `0x0019` | `CarryOverReceiptV1` |
| `0x001a` | `CandidateAnnouncementV1` |
| `0x001b` | `SignOnceRecordV1` |
| `0x001c` | `ActivationCallCoreV1` |
| `0x001d` | `LysisArithmeticSummaryV1` |
| `0x001e` | `OcompJobRecordV1` |
| `0x001f` | `ShuffleRunArtifactV1` |

The implementation task creates one machine-readable registry and generates
Rust constants, docs and collision tests from it. Handwritten duplicate tag,
domain, selector or codec literals fail CI.

### 3.4 Public activation envelope

The public transaction is a normal Ethereum transaction to the existing
Metadosis precompile address
`0x000000000000000000000000000000000000100e`.

The only new public write method is:

```solidity
function activateLysis(bytes calldata pocActivationV1)
    external
    returns (bytes32 activationCallId, bytes32 resultDigest, uint8 outcome);
```

Its selector is `0x5b2818ca`, the first four bytes of
`keccak256("activateLysis(bytes)")`. The dynamic bytes contain exactly one
OCB1 `PoCActivationV1`. The selector is Lysis-specific; the bytes cannot select
a program, call arbitrary targets or carry an opaque write set.

The transaction path is normal RPC -> txpool -> gossip -> proposal -> import ->
replay. A private/direct executor injection is not a valid PoC path.

The same existing Metadosis address exposes three bounded reads:

```solidity
function getOffchainJob(bytes32 intentId)
    external view returns (bytes memory ocompJobRecordV1);

function getActiveLysisGeneration(uint32 wwd)
    external view returns (bytes memory activeGenerationV1);

function getLysisTerminalReceipt(bytes32 intentId)
    external view returns (bytes memory aggregateActivationReceiptV1);
```

Their selectors are respectively `0x4c132d3d`, `0x50f8b3e4` and `0x20f46be7`.
The returned bytes are one complete, size-capped OCB1 object. The PoC adds no
custom RPC.

The activation's closed user-error ABI is:

```solidity
error OcompActivationRejected(uint16 code);
```

Its selector is `0x8e346803`. The stable code registry is:

| Code | Name |
|---:|---|
| `1` | `MALFORMED_ENCODING` |
| `2` | `LIMIT_EXCEEDED` |
| `3` | `FORK_OR_BUNDLE_MISMATCH` |
| `4` | `JOB_NOT_FOUND` |
| `5` | `JOB_TERMINAL` |
| `6` | `COMPLETED_BINDING_MISMATCH` |
| `7` | `DEADLINE_NOT_LIVE` |
| `8` | `FINALITY_PROOF_INVALID` |
| `9` | `JOB_BINDING_INVALID` |
| `10` | `COMMITTEE_SNAPSHOT_INVALID` |
| `11` | `CERTIFICATE_INVALID` |
| `12` | `RESULT_DIGEST_MISMATCH` |
| `13` | `RESULT_STRUCTURE_INVALID` |
| `14` | `ACTIVATION_PRECONDITION_MISMATCH` |
| `15` | `BLOCK_ACTIVATION_LIMIT` |
| `16` | `OWNER_APPLY_REJECTED` |
| `17` | `RECEIPT_MISMATCH` |

Unknown codes never decode as aliases. Storage/provider/invariant corruption is
a fatal block-execution error and has no public rejection code.

The new consensus logs have these exact ABI declarations and topic zeroes:

```solidity
event OffchainJobRequested(
    bytes32 indexed intentId,
    uint32 indexed wwd,
    uint64 pendingNonce,
    uint32 attempt,
    uint64 deadlineHeight,
    bytes32 activationPreconditionsHash
);
// topic0 =
// 0x997139de4a9928090f392ef70b47db793b46eb650dd837141c0650776ea8e8ee

event OffchainJobExpired(
    bytes32 indexed intentId,
    uint32 indexed wwd,
    uint64 oldPendingNonce,
    uint64 nextPendingNonce,
    uint64 expiredAtHeight
);
// topic0 =
// 0x9ca54d8b0ae876fcd3b6e519643b9409e3694c8110d3e17f72f8baa51cea320d

event OffchainJobConflicted(
    bytes32 indexed intentId,
    bytes32 indexed jobId,
    uint32 attempt,
    uint64 oldPendingNonce,
    uint64 nextPendingNonce,
    bytes32 resultDigest
);
// topic0 =
// 0x4ccae6e58910db3b75ee57048a6f1ef06f25b45ef2564a13ca9ef93aacc371bf

event LysisActivated(
    bytes32 indexed intentId,
    bytes32 indexed jobId,
    bytes32 activationCallId,
    bytes32 resultDigest,
    bytes32 terminalReceiptHash,
    uint32 wwd
);
// topic0 =
// 0xf9f846ebfcc5895f442c47e436aeb0a86c170ad48a7863eb9f9dddb0a4492f68
```

The indexed qualifiers do not change the listed signature hashes. Existing
owner events remain owner-defined; their certified projections are bound by
the state-event digest below.

## 4. Hash, root and signature contracts

### 4.1 Hash framing

Unless an existing nested protocol explicitly owns its bytes, OCOMP uses:

```text
H(domain_ascii, payload) =
  keccak256(
    u16_be(len(domain_ascii)) ||
    exact ASCII domain bytes ||
    u32_be(len(payload)) ||
    payload
  )
```

Lengths are byte lengths. Domain strings are ASCII and include their version.
For `canonical(T)`, `payload` includes the complete OCB1 envelope.

Registered domains are:

| Domain | Payload |
|---|---|
| `OUTBE_OCOMP_PROTOCOL_BUNDLE_V1` | canonical `ProtocolBundleV1` |
| `OUTBE_OCOMP_PRE_ADMISSION_V1` | canonical `PreAdmissionEnvelopeV1` |
| `OUTBE_OCOMP_ACTIVATION_PRECONDITIONS_V1` | canonical `ActivationPreconditionsV1` |
| `OUTBE_OCOMP_INTENT_V1` | canonical `JobIntentV1` |
| `OUTBE_OCOMP_INTENT_SLOT_V1` | `IntentId` |
| `OUTBE_OCOMP_JOB_V1` | `IntentId || finalized_block_hash || finalized_state_root` |
| `OUTBE_OCOMP_INPUT_CHUNK_V1` | canonical `AuthenticatedInputChunkV1` |
| `OUTBE_OCOMP_INPUT_MANIFEST_V1` | canonical `InputManifestV1` |
| `OUTBE_OCOMP_CODEC_DESCRIPTOR_V1` | role, version and complete canonical schema descriptor |
| `OUTBE_OCOMP_OPENING_CODEC_REGISTRY_V1` | ordered Fidelity/Oracle source-kind and codec-ID pair |
| `OUTBE_OCOMP_PLAN_V1` | canonical nested `PlanCommitmentV1` |
| `OUTBE_OCOMP_UNIT_V1` | canonical `UnitSpecV1` |
| `OUTBE_OCOMP_UNIT_INTERVAL_V1` | phase and canonical nested interval |
| `OUTBE_OCOMP_UNIT_INPUTS_V1` | ordered canonical nested `CanonicalInputRefV1` vector |
| `OUTBE_OCOMP_UNIT_EMPTY_V1` | unit-input purpose |
| `OUTBE_OCOMP_UNIT_OUTPUT_V1` | unit identity and exact phase output bytes |
| `OUTBE_OCOMP_UNIT_COVERAGE_V1` | phase, interval and work-output coverage header |
| `OUTBE_OCOMP_UNIT_ARTIFACT_V1` | canonical `UnitArtifactV1` |
| `OUTBE_OCOMP_RESULT_CHUNK_V1` | canonical `ResultChunkV1` |
| `OUTBE_OCOMP_RESULT_V1` | canonical `ActivationPayloadV1` |
| `OUTBE_OCOMP_VALIDATOR_IDENTITY_V1` | index, validator address and canonical consensus public key |
| `OUTBE_OCOMP_COMMITTEE_V1` | canonical `OcompCommitteeSnapshotV1` |
| `OUTBE_OCOMP_KEY_POP_V1` | canonical nested `OcompKeyRegistrationCoreV1` |
| `OUTBE_OCOMP_SIGN_ONCE_SLOT_V1` | canonical sign-once key |
| `OUTBE_OCOMP_ACTIVATION_CALL_V1` | canonical `ActivationCallCoreV1` |
| `OUTBE_OCOMP_LYSIS_ARITHMETIC_V1` | canonical `LysisArithmeticSummaryV1` |
| `OUTBE_OCOMP_RESULT_EVIDENCE_V1` | canonical `PoCActivationV1` |
| `OUTBE_OCOMP_TERMINAL_RECEIPT_V1` | canonical `AggregateActivationReceiptV1` |
| `OUTBE_OCOMP_NOD_RECEIPT_V1` | canonical `NodBatchReceiptV1` |
| `OUTBE_OCOMP_CONTRIBUTOR_RECEIPT_V1` | canonical `ContributorReceiptV1` |
| `OUTBE_OCOMP_TRIBUTE_RECEIPT_V1` | canonical `TributeReceiptV1` |
| `OUTBE_OCOMP_DESIS_REQUEST_BRIEF_V1` | protocol bundle, WWD, auction base, entry price and request logical anchor |
| `OUTBE_OCOMP_BUDGET_SPLIT_RECEIPT_V1` | canonical `RequestBudgetSplitReceiptV1` |
| `OUTBE_OCOMP_CARRY_OVER_RECEIPT_V1` | canonical `CarryOverReceiptV1` |
| `OUTBE_OCOMP_ACTIVE_GENERATION_V1` | canonical `ActiveGenerationV1` |
| `OUTBE_OCOMP_JOB_RECORD_V1` | canonical `OcompJobRecordV1` |
| `OUTBE_OCOMP_STATE_EVENTS_V1` | owner kind, effect binding and canonical owner-specific event projection |
| `OUTBE_OCOMP_APPLY_EVENT_SUMMARY_V1` | four activation-owner state-event digests in fixed order |
| `OUTBE_OCOMP_EFFECTS_V1` | four activation-owner receipt hashes in fixed order |
| `OUTBE_OCOMP_LIST_EMPTY_V1` | list kind |
| `OUTBE_OCOMP_LIST_LEAF_V1` | list kind, index and canonical item |
| `OUTBE_OCOMP_LIST_PAD_V1` | list kind and missing index |
| `OUTBE_OCOMP_LIST_NODE_V1` | list kind, level, index and children |
| `OUTBE_OCOMP_LIST_ROOT_V1` | list kind, count, height and tree root |

The freeze generator asserts that every production hash operation maps to
exactly one registered domain and exactly one preimage grammar.

Derived IDs are:

```text
ProtocolBundleHash = H(
  "OUTBE_OCOMP_PROTOCOL_BUNDLE_V1",
  canonical(ProtocolBundleV1)
)

PreAdmissionEnvelopeHash = H(
  "OUTBE_OCOMP_PRE_ADMISSION_V1",
  canonical(PreAdmissionEnvelopeV1)
)

ActivationPreconditionsHash = H(
  "OUTBE_OCOMP_ACTIVATION_PRECONDITIONS_V1",
  canonical(ActivationPreconditionsV1)
)

IntentId = H("OUTBE_OCOMP_INTENT_V1", canonical(JobIntentV1))

IntentStorageKeyV1 =
  H("OUTBE_OCOMP_INTENT_SLOT_V1", IntentId)

JobId = H(
  "OUTBE_OCOMP_JOB_V1",
  IntentId || finalized_request_block_hash || finalized_request_state_root
)

PlanHash = H(
  "OUTBE_OCOMP_PLAN_V1",
  canonical_nested(PlanCommitmentV1)
)

UnitId = H("OUTBE_OCOMP_UNIT_V1", canonical(UnitSpecV1))

UnitIntervalCommitment = H(
  "OUTBE_OCOMP_UNIT_INTERVAL_V1",
  phase || canonical_nested(interval)
)

UnitInputRoot = H(
  "OUTBE_OCOMP_UNIT_INPUTS_V1",
  u32_be(input_count) ||
  canonical_nested(CanonicalInputRefV1_0) || ... ||
  canonical_nested(CanonicalInputRefV1_n)
)

EmptyUnitInputId(purpose) =
  H("OUTBE_OCOMP_UNIT_EMPTY_V1", u8(purpose))

UnitOutputSemanticDigest = H(
  "OUTBE_OCOMP_UNIT_OUTPUT_V1",
  protocol_bundle_hash || JobId || u32_be(attempt) || UnitId ||
  u8(phase) || UnitIntervalCommitment ||
  u32_be(len(canonical_output_bytes)) || canonical_output_bytes
)

UnitCoverageCommitment = H(
  "OUTBE_OCOMP_UNIT_COVERAGE_V1",
  u8(phase) || UnitIntervalCommitment ||
  source_coverage_root || output_coverage_root ||
  u32_be(source_coverage_count) || u32_be(output_coverage_count)
)

ResultChunkHash = H(
  "OUTBE_OCOMP_RESULT_CHUNK_V1",
  canonical(ResultChunkV1)
)

ResultDigest =
  H("OUTBE_OCOMP_RESULT_V1", canonical(ActivationPayloadV1))

InputChunkSemanticDigest =
  H("OUTBE_OCOMP_INPUT_CHUNK_V1", canonical(AuthenticatedInputChunkV1))

InputManifestHash =
  H("OUTBE_OCOMP_INPUT_MANIFEST_V1", canonical(InputManifestV1))

UnitArtifactDigest =
  H("OUTBE_OCOMP_UNIT_ARTIFACT_V1", canonical(UnitArtifactV1))

ValidatorIdentityHash = H(
  "OUTBE_OCOMP_VALIDATOR_IDENTITY_V1",
  u8(validator_index) || validator_address20 ||
  u32_be(len(canonical_consensus_public_key)) ||
  canonical_consensus_public_key
)

OcompCommitteeSnapshotHash =
  H("OUTBE_OCOMP_COMMITTEE_V1", canonical(OcompCommitteeSnapshotV1))

SignOnceSlot = H(
  "OUTBE_OCOMP_SIGN_ONCE_SLOT_V1",
  u64_be(chain_id) || purpose=RESULT_SIGNATURE(1) ||
  JobId || u32_be(attempt)
)

ActivationCallId = H(
  "OUTBE_OCOMP_ACTIVATION_CALL_V1",
  canonical(ActivationCallCoreV1)
)

ResultEvidenceHash = H(
  "OUTBE_OCOMP_RESULT_EVIDENCE_V1",
  canonical(PoCActivationV1)
)

NodReceiptHash = H(
  "OUTBE_OCOMP_NOD_RECEIPT_V1",
  canonical(NodBatchReceiptV1)
)

ContributorReceiptHash = H(
  "OUTBE_OCOMP_CONTRIBUTOR_RECEIPT_V1",
  canonical(ContributorReceiptV1)
)

TributeReceiptHash = H(
  "OUTBE_OCOMP_TRIBUTE_RECEIPT_V1",
  canonical(TributeReceiptV1)
)

DesisRequestBriefHash = H(
  "OUTBE_OCOMP_DESIS_REQUEST_BRIEF_V1",
  protocol_bundle_hash
  || u32_be(wwd)
  || u256_be(auction_base)
  || u256_be(auction_entry_price)
  || u64_be(logical_anchor)
)

RequestBudgetSplitReceiptHash = H(
  "OUTBE_OCOMP_BUDGET_SPLIT_RECEIPT_V1",
  canonical(RequestBudgetSplitReceiptV1)
)

CarryOverReceiptHash = H(
  "OUTBE_OCOMP_CARRY_OVER_RECEIPT_V1",
  canonical(CarryOverReceiptV1)
)

ActiveGenerationHash = H(
  "OUTBE_OCOMP_ACTIVE_GENERATION_V1",
  canonical(ActiveGenerationV1)
)

TerminalReceiptHash = H(
  "OUTBE_OCOMP_TERMINAL_RECEIPT_V1",
  canonical(AggregateActivationReceiptV1)
)

OcompJobRecordHash = H(
  "OUTBE_OCOMP_JOB_RECORD_V1",
  canonical(OcompJobRecordV1)
)

OwnerStateEventDigest(
  owner_kind,
  effect_binding,
  canonical_owner_projection
) = H(
  "OUTBE_OCOMP_STATE_EVENTS_V1",
  u8(owner_kind) ||
  canonical_nested(effect_binding) ||
  u32_be(len(canonical_owner_projection)) ||
  canonical_owner_projection
)

ApplyEventSummaryHash = H(
  "OUTBE_OCOMP_APPLY_EVENT_SUMMARY_V1",
  nod_state_event_digest ||
  contributor_state_event_digest ||
  tribute_state_event_digest ||
  carry_over_state_event_digest
)

TransportDigestV1 = keccak256(exact stored bytes)
```

Activation owner kinds are fixed as `NOD(1)`, `CONTRIBUTOR(2)`, `TRIBUTE(3)`
and `CARRY_OVER(4)`. Each owner has one concrete nested projection schema
generated beside its receipt type; no generic event map, arbitrary target or
opaque event byte string is accepted. Receipt verification
reconstructs the projection from pre-state, action and post-state, then
recomputes this digest.

`TransportDigestV1` has no domain prefix: it is the content address and
corruption check for the exact stored byte stream, not a semantic protocol
statement. For an OCOMP semantic CAS object, the stored stream includes its
complete OCB1 envelope. `encoded_bytes` is exactly the length of that same
stream. A CAS path, filesystem metadata, compression wrapper or local reference
record is never part of the digest. PoC stores semantic objects uncompressed.

Every CAS reader checks `encoded_bytes` and `TransportDigestV1` from the same
open file descriptor before accepting the independently verified semantic
digest/root/count. Local process/CAS layout is fixed by decision ticket #6;
changing it cannot change these bytes.

### 4.2 Canonical ordered-list roots

List kinds are:

| ID | List |
|---:|---|
| `1` | Nod actions |
| `2` | bucket records |
| `3` | contributor actions |
| `4` | complete output manifest |
| `5` | semantic event records |
| `6` | input chunk references |
| `7` | unit specifications/artifacts |
| `8` | raw Tribute coverage `(raw_ordinal, tribute_id)` |
| `9` | Fidelity authenticated openings |
| `10` | Oracle authenticated openings |

For list kind `k` and canonical item bytes `item_i`:

```text
leaf_i = H(
  "OUTBE_OCOMP_LIST_LEAF_V1",
  u16_be(k) || u32_be(i) || u32_be(len(item_i)) || item_i
)

pad_i = H(
  "OUTBE_OCOMP_LIST_PAD_V1",
  u16_be(k) || u32_be(i)
)
```

For a non-empty list, pad to the next power of two. At tree level `0`, use the
leaf/pad values. Then:

```text
node(level, index) = H(
  "OUTBE_OCOMP_LIST_NODE_V1",
  u16_be(k) || u16_be(level) || u32_be(index) || left || right
)
```

The first parent level is `level=1`. The final root is:

```text
H(
  "OUTBE_OCOMP_LIST_ROOT_V1",
  u16_be(k) || u32_be(real_count) || u16_be(tree_height) || tree_root
)
```

For an empty list:

```text
H("OUTBE_OCOMP_LIST_EMPTY_V1", u16_be(k))
```

Count, position and list purpose are therefore committed. There is no
duplicate-last, unordered-set or alternate empty-tree convention.

### 4.3 OCOMP result signatures

The PoC signature profile is:

| Property | Rule |
|---|---|
| scheme | ECDSA over secp256k1 |
| dependency | locked workspace `k256` profile |
| public key | exactly 33-byte compressed SEC1; valid non-identity curve point |
| message | the exact 32-byte `ResultDigest`, signed as a prehash |
| nonce | deterministic RFC 6979 |
| signature | exactly 64-byte `r || s`, each 32-byte big-endian |
| malleability | both scalars valid/non-zero; `s` must be low |
| recovery byte | absent |
| key purpose | `ResultSignature = 1` only |
| PoC key epoch | `1` |

The result digest already binds bundle, JobId and attempt; JobId binds the exact
chain/genesis/fork intent and finalized state. The committee snapshot in the
intent binds the key epoch and identity mapping.

The certificate bitmap is one byte. Only bits `0..3` may be set; bits `4..7`
must be zero. Its population count is exactly three. The certificate contains
exactly three `(validator_index:u8, signature:[u8;64])` entries in ascending
index order, and the entries must equal the set bitmap bits. Aggregate or
recoverable signatures are not accepted.

`OcompKeyRegistrationV1` proof of possession is a signature by the registered
OCOMP key over:

```text
H(
  "OUTBE_OCOMP_KEY_POP_V1",
  canonical_nested(OcompKeyRegistrationCoreV1)
)
```

`canonical_nested` follows the same fixed field grammar but has no OCB1
top-level envelope. This is one explicitly registered PoP transcript, not a
second accepted encoding of `OcompKeyRegistrationV1`.

Result keys are separate from Commonware finality, TEE, EVM account and worker
keys.

The durable sign-once key and value are:

```text
key =
  chain_id || purpose=ResultSignature || JobId || attempt

value =
  protocol_bundle_hash || committee_snapshot_hash ||
  key_epoch || ResultDigest || signature
```

The record is fsynced before the signature is released. Exact retries return
the stored signature; any different value for the same key refuses signing,
including after restart.

## 5. Exact canonical schemas

Names below are semantic type names. Field order in each block is canonical
byte order. `Hash` means `B256`.

### 5.1 Profiles and protocol bundle

```text
CorrectnessProfileV1 {
  profile_id: Hash,
  program: enum u8 = LYSIS_V1(1),
  arithmetic_profile_id: Hash,
  object_codec_registry_hash: Hash,
  list_root_scheme_id: Hash,
  result_signature_profile_id: Hash,
  finality_verifier_profile_id: Hash
}

CapacityProfileV1 {
  profile_id: Hash,
  max_tributes_per_work_shard: u32,
  max_workers_per_domain: u8,
  max_pending_jobs: u8,
  max_intents_per_block: u8,
  max_activations_per_block: u8,
  max_ready_inspections_per_block: u8,
  max_expirations_per_block: u8,
  retry_backoff_blocks: u64,
  max_terminal_job_records: u16,
  max_reference_currencies: u16,
  max_fidelity_cohorts_per_owner: u16,
  max_oracle_wwd_pair_entries: u32,
  max_active_scurve_entries: u32,
  result_deadline_blocks: u64,
  source_retention_after_terminal_blocks: u64,
  generated_limits_manifest_hash: Hash
}

ProtocolBundleV1 {
  protocol_version: u16,
  fork_id: Hash,
  intent_codec_id: Hash,
  finalized_intent_proof_codec_id: Hash,
  tribute_body_codec_id: Hash,
  fidelity_opening_codec_id: Hash,
  oracle_opening_codec_id: Hash,
  result_codec_id: Hash,
  action_codec_id: Hash,
  activation_codec_id: Hash,
  evidence_codec_id: Hash,
  request_semantics_version: u16,
  lysis_program_semantics_hash: Hash,
  planner_spec_version: u16,
  reducer_spec_version: u16,
  activation_apply_semantics_hash: Hash,
  effect_contract_registry_hash: Hash,
  object_codec_registry_hash: Hash,
  correctness_profile_id: Hash,
  capacity_profile_id: Hash,
  result_signature_profile_id: Hash,
  finality_verifier_and_vote_domain_id: Hash,
  consensus_committee_history_schema_version: u16,
  ocomp_committee_schema_version: u16,
  proof_system_and_verifier_key_id: Option<Hash>,
  da_codec_and_binding_verifier_id: Option<Hash>,
  anti_equivocation_journal_schema_hash: Hash,
  mode_pause_revocation_semantics_hash: Hash,
  upgrade_fsm_semantics_hash: Hash,
  release_requirement_catalog_sequence: u64,
  release_requirement_catalog_hash: Hash,
  release_requirement_catalog_parent_hash: Hash,
  release_gate_authority_envelope_hash: Hash,
  release_approval_policy_hash: Hash,
  release_validator_command_artifact_hash: Hash,
  consensus_state_schema_version: u16,
  migration_manifest_hash: Hash,
  required_upgrade_handler_set_hash: Hash
}
```

Proof and DA options are `none`. Pause/upgrade handlers are fixed PoC no-ops.
Release and migration fields use one generated genesis-placeholder artifact
hash, not an unset zero. Replacing any placeholder requires a new bundle.

### 5.2 Pre-admission, budget split and activation preconditions

```text
PreAdmissionEnvelopeV1 {
  chain_id: u64,
  genesis_hash: Hash,
  fork_id: Hash,
  wwd: u32,
  sealed_tribute_collection_root: Hash,
  sealed_tribute_count: u32,
  sealed_tribute_canonical_body_bytes: u64,
  distinct_owner_count: u32,
  distinct_reference_currency_count: u16,
  max_fidelity_cohorts_observed: u16,
  oracle_wwd_pair_entries_observed: u32,
  active_scurve_entries_observed: u32,
  auction_entry_price: U256,
  auction_entry_price_source:
    enum u8 = LAST_CLOSED_DAY_VWAP(1) | CURRENT_VWAP_FALLBACK(2),
  auction_entry_price_source_day: u32,
  oracle_state_version: u64,
  fidelity_opening_upper_bound: u32,
  oracle_opening_upper_bound: u32,
  input_encoded_bytes_upper_bound: u64,
  output_record_upper_bound: u32,
  action_stream_bytes_upper_bound: u64,
  activation_bytes_upper_bound: u64,
  retained_bytes_upper_bound: u64,
  correctness_profile_id: Hash,
  capacity_profile_id: Hash
}

FrozenMetadosisValuesV1 {
  day_type: enum u8 = GREEN(1) | RED(2),
  day_limit: U256,
  previous_vwap: U256,
  current_vwap: U256,
  gratis_demand: U256,
  gratis_supply: U256,
  lysis_budget: U256,
  auction_base: U256,
  auction_entry_price: U256,
  request_budget_split_receipt_hash: Hash
}

TributeInputBindingV1 {
  wwd: u32,
  source_generation: u64,
  collection_key: Hash,
  sealed_collection_root: Hash,
  exact_count: u32,
  exact_nominal_total: U256
}

NodTargetPreconditionV1 {
  wwd: u32,
  target_generation: u64,
  namespace_root_before: Hash,
  max_nod_count: u32
}

ContributorTargetPreconditionV1 {
  series_id: u32,
  expected_series_version: u64,
  max_contributor_count: u32,
  max_eligible_nominal_total: U256
}

MetadosisAttemptPreconditionV1 {
  wwd: u32,
  pending_nonce: u64,
  expected_status: enum u8 = OFFCHAIN_PENDING(1),
  state_version: u64
}

ActivationPreconditionsV1 {
  tribute: TributeInputBindingV1,
  nod: NodTargetPreconditionV1,
  contributors: ContributorTargetPreconditionV1,
  metadosis: MetadosisAttemptPreconditionV1
}
```

These records are immutable predicates inside `JobIntentV1`, not copies stored
in each owner. Desis and PromisLimit have no activation reservation.

At request time:

```text
day_limit = lysis_budget + auction_base
```

GREEN commits one Desis brief for `auction_base`. RED commits the same amount
to carry-over. A retry never repeats this effect.

At activation:

```text
lysis_budget = nod_gratis_consumed + unused_lysis
```

The verifier checks live Nod, contributor and Metadosis predicates inside the
outer checkpoint. Tribute remains bound by its immutable sealed input identity.

### 5.3 Intent, finalized proof and input artifacts

```text
JobIntentV1 {
  chain_id: u64,
  genesis_hash: Hash,
  fork_id: Hash,
  wwd: u32,
  pending_nonce: u64,
  attempt: u32,
  protocol_bundle_hash: Hash,
  ce_sealed_root: Hash,
  sealed_tribute_collection_key: Hash,
  sealed_tribute_collection_root: Hash,
  authenticated_day_count: u32,
  authenticated_day_nominal: U256,
  pre_admission_envelope_hash: Hash,
  source_availability_policy_id: Hash,
  frozen_metadosis_values: FrozenMetadosisValuesV1,
  logical_evaluation_height: u64,
  logical_evaluation_time: u64,
  activation_preconditions: ActivationPreconditionsV1,
  result_committee_snapshot_hash: Hash,
  custody_committee_epoch_hash: Option<Hash>,
  deadline_height: u64
}

CertifiedParentAccountingMetadataV2 {
  finalized_block_number: u64,
  finalized_block_hash: Hash,
  finalized_epoch: u64,
  finalized_view: u64,
  parent_view: u64,
  ordered_committee: Vec<bounded bytes>,
  signer_bitmap: bounded bytes,
  canonical_commonware_finalization_proof: bounded bytes,
  committee_set_hash: Hash,
  vrf_material_version: u16,
  vrf_group_public_key_hash: Hash,
  proof_kind: enum u8 = FINALIZATION(1),
  missed_proposers: Vec<Hash> = []
}

FinalizedIntentProofV1 {
  chain_id: u64,
  genesis_hash: Hash,
  fork_id: Hash,
  protocol_bundle_hash: Hash,
  canonical_request_header_rlp: bounded bytes,
  parent_accounting: CertifiedParentAccountingMetadataV2,
  historical_committee_membership_proof: bounded bytes,
  canonical_job_intent: bounded bytes,
  intent_account_proof: bounded bytes,
  intent_storage_proof: bounded bytes
}

CheckpointIdentityV1 {
  finalized_block_number: u64,
  finalized_block_hash: Hash,
  finalized_state_root: Hash,
  finalized_ce_root: Hash,
  ce_schema_version: u16
}

InputChunkRefV1 {
  kind: enum u8 = TRIBUTE(1) | FIDELITY(2) | ORACLE(3),
  ordinal: u32,
  record_count: u32,
  first_key: bounded bytes,
  last_key_inclusive: bounded bytes,
  encoded_bytes: u64,
  semantic_digest: Hash,
  transport_digest: Hash
}

AuthenticatedOpeningV1 {
  source_kind: enum u8 = FIDELITY(1) | ORACLE(2),
  canonical_subject_key: bounded bytes,
  canonical_value: bounded bytes,
  opening_codec_id: Hash,
  canonical_opening: bounded bytes
}

AuthenticatedInputChunkV1 {
  protocol_bundle_hash: Hash,
  JobId: Hash,
  kind: enum u8 = TRIBUTE(1) | FIDELITY(2) | ORACLE(3),
  ordinal: u32,
  canonical_records_or_openings: Vec<bounded bytes>
}

InputManifestV1 {
  protocol_bundle_hash: Hash,
  JobId: Hash,
  attempt: u32,
  checkpoint: CheckpointIdentityV1,
  wwd: u32,
  sealed_tribute_collection_key: Hash,
  sealed_tribute_collection_root: Hash,
  tribute_count: u32,
  tribute_nominal_total: U256,
  input_chunk_count: u32,
  input_chunk_list_root: Hash,
  fidelity_opening_root: Hash,
  oracle_opening_root: Hash,
  exact_encoded_bytes: u64,
  exact_record_count: u32,
  body_codec_id: Hash,
  opening_codec_registry_hash: Hash,
  compression: enum u8 = NONE(0)
}
```

Nested Commonware, Ethereum header/account/storage proof, CE body and opening
bytes retain their existing canonical codecs. Their exact codec IDs and byte
caps are in `ProtocolBundleV1` and the generated limits manifest. OCB1 does not
reinterpret those protocols; it length-bounds and binds their exact bytes.

The three nested input codec fields are the only authority for these bytes:

```text
InputManifestV1.body_codec_id =
  ProtocolBundleV1.tribute_body_codec_id

OpeningCodecRegistryHash =
  H(
    "OUTBE_OCOMP_OPENING_CODEC_REGISTRY_V1",
    u16_be(2) ||
    u8(FIDELITY=1) || ProtocolBundleV1.fidelity_opening_codec_id ||
    u8(ORACLE=2)   || ProtocolBundleV1.oracle_opening_codec_id
  )

InputManifestV1.opening_codec_registry_hash = OpeningCodecRegistryHash
```

Each codec ID is generated from a checked-in complete descriptor:

```text
CodecId =
  H(
    "OUTBE_OCOMP_CODEC_DESCRIPTOR_V1",
    u16_be(role) ||
    u16_be(version) ||
    u32_be(len(canonical_schema_descriptor)) ||
    canonical_schema_descriptor
  )
```

The descriptor binds the complete byte grammar, strict decoder and canonical
re-encoding rule, source/contract role, subject grammar, raw-slot semantics,
proof witness version and applicable caps. Rust type names, source paths and
free-form labels are not descriptors. Fidelity and Oracle use distinct codec
IDs even though both contain `RawContractOpeningProofV1`.

The node opening handoff is bounded, not job-capped. Canonically sorted unique
owners are partitioned into consecutive batches of at most 256 owners. Every
batch becomes one Fidelity `AuthenticatedOpeningV1`; owner 257 is the first
owner of the second batch and is never rejected because of total job size.
Oracle is one job-wide `AuthenticatedOpeningV1` over the complete sorted ISO
set, including 840. Oracle material repeated by bounded node requests must be
byte-identical and is published once.

For both sources, `canonical_subject_key` is the frozen typed owner-batch or
`(WWD, ISO-set)` encoding, `canonical_value` is the canonical ordered
`(slot,value)` vector, and `canonical_opening` is the canonical nested
`RawContractOpeningProofV1`. `fidelity_opening_root` and
`oracle_opening_root` are `ordered_list_root` values over the canonical
`AuthenticatedOpeningV1` bytes using the distinct registered list kinds
`FIDELITY_OPENINGS(9)` and `ORACLE_OPENINGS(10)`.

The exact production source of each finality/opening byte sequence is decision
ticket 5. That research may select an existing adapter or add a narrow read adapter,
but it cannot change this outer byte contract.

### 5.4 Deterministic plan and unit artifacts

```text
CanonicalInputRefV1 {
  purpose: enum u8 =
    INPUT_MANIFEST(1) | TRIBUTE_STREAM(2) |
    FIDELITY_OPENINGS(3) | ORACLE_OPENINGS(4) |
    ENUMERATED_TRIBUTES(10) | FIDELITY_PARTIALS(11) |
    FI_FRACTION_TABLE(12) | AMOUNT_RECORDS(13) |
    GRATIS_PREFIX_TABLE(14) | FINALIZED_OUTPUT_RECORDS(15) |
    OWNER_ORDERED_RECORDS(16) | BUCKET_ORDERED_RECORDS(17) |
    ROOT_SUMMARY(18),
  source_kind: enum u8 =
    AUTHENTICATED_ROOT(1) | UNIT_OUTPUT(2) | CANONICAL_EMPTY(3),
  source_id: Hash,
  record_count_limit: u32,
  max_encoded_bytes: u64,
  max_decoded_bytes: u64
}

EntityIdHalfOpenRange { start: EntityId36, end: Option<EntityId36> }
FidelityIndexHalfOpenRange { start: u32, end: u32 }
CanonicalRunSpan { start_run: u32, end_run: u32 }
BinaryReducerNode { level: u16, index: u32 }

PlanCommitmentV1 {
  protocol_bundle_hash: Hash,
  JobId: Hash,
  attempt: u32,
  input_manifest_hash: Hash,
  wwd: u32,
  lysis_budget: U256,
  logical_evaluation_time: u64,
  tribute_count: u32,
  max_tributes_per_work_shard: u32,
  primary_work_unit_count: u32,
  primary_work_unit_root: Hash,
  planner_spec_version: u16,
  reducer_spec_version: u16
}

UnitSpecV1 {
  protocol_bundle_hash: Hash,
  JobId: Hash,
  attempt: u32,
  phase: enum u8 =
    ENUMERATE(1) | FIDELITY_MAP(2) | FIXED_REDUCE(3) |
    AMOUNT_MAP(4) | GRATIS_PREFIX(5) | OUTPUT_FINALIZE(6) |
    OWNER_SHUFFLE(7) | BUCKET_SHUFFLE(8) | ROOT_REDUCE(9) |
    GRATIS_PREFIX_DOWN(10),
  interval: closed tagged union =
    ENTITY_ID_RANGE(1, EntityIdHalfOpenRange) |
    FIDELITY_INDEX_RANGE(2, FidelityIndexHalfOpenRange) |
    CANONICAL_RUN_SPAN(3, CanonicalRunSpan) |
    BINARY_REDUCER_NODE(4, BinaryReducerNode),
  canonical_ordered_inputs: Vec<CanonicalInputRefV1>,
  lysis_program_semantics_hash: Hash,
  planner_spec_version: u16,
  reducer_spec_version: u16
}

UnitArtifactV1 {
  protocol_bundle_hash: Hash,
  JobId: Hash,
  attempt: u32,
  UnitId: Hash,
  phase: u8,
  interval_commitment: Hash,
  input_root: Hash,
  output_record_count: u32,
  canonical_output_bytes: bounded bytes,
  output_semantic_digest: Hash,
  coverage_or_permutation_commitment: Hash
}

ShuffleRunArtifactV1 {
  protocol_bundle_hash: Hash,
  JobId: Hash,
  attempt: u32,
  UnitId: Hash,
  kind: OWNER(1) | BUCKET(2),
  run_span: CanonicalRunSpan,
  page_span: ShufflePageSpanV1,
  first_record_ordinal: u32,
  record_count: u32,
  source_coverage_root: Hash,
  source_coverage_count: u32,
  ordered_record_root: Hash,
  payload: closed tagged union =
    OWNER_LEAF(1, Vec<ContributorActionV1>) |
    BUCKET_LEAF(2, Vec<ShuffleBucketRecordV1>) |
    NODE(3, left: ShuffleRunChildV1, right: ShuffleRunChildV1)
}
```

For `AUTHENTICATED_ROOT`, `source_id` is the exact authenticated semantic root
and `record_count_limit` is exact. For `UNIT_OUTPUT`, `source_id` is the
producer `UnitId` and the count is an upper bound; the consumer must load and
fully verify that producer artifact. For `CANONICAL_EMPTY`, `source_id` must
equal `EmptyUnitInputId(purpose)` and the count is zero. This producer binding
allows the complete plan and every `UnitId` to be derived before execution;
an unknown future output digest is never represented by a zero or placeholder.

For `OWNER_SHUFFLE` and `BUCKET_SHUFFLE`,
`UnitArtifactV1.canonical_output_bytes` is exactly one canonical
`ShuffleRunArtifactV1` root. Each leaf has at most 256 records. Internal nodes
contain exactly two content-addressed child references; an odd last subtree is
promoted unchanged. Page spans start at zero for a root, are adjacent, and use
the unique largest-power-of-two canonical split. Child summaries bind page
span, first record ordinal, record count and ordered-record root. A leaf
`ordered_record_root` is the frozen ordered-list root for its canonical owner
or bucket records. A node `ordered_record_root` is
`H(OUTBE_OCOMP_SHUFFLE_RUN_NODE_V1,
canonical(kind,page_span,first_record_ordinal,record_count,
left_ordered_record_root,right_ordered_record_root))`; it is recomputed during
decode/adoption rather than trusted as metadata. Every child
object repeats the exact bundle/job/attempt/UnitId/kind/run/source-coverage
binding of its root. The consumer traverses and verifies the complete tree
before admitting the producer artifact. This keeps every OCB1 object bounded
without imposing any limit on the parent Tribute population and without an
unbounded vector of CAS references.

The phase fixes the interval tag:

| Phase | Required interval |
|---|---|
| `ENUMERATE`, `AMOUNT_MAP`, `OUTPUT_FINALIZE` | `EntityIdHalfOpenRange` |
| `FIDELITY_MAP` | `FidelityIndexHalfOpenRange` |
| `FIXED_REDUCE`, `GRATIS_PREFIX`, `GRATIS_PREFIX_DOWN`, `ROOT_REDUCE` | `BinaryReducerNode` |
| `OWNER_SHUFFLE`, `BUCKET_SHUFFLE` | `CanonicalRunSpan` |

For `FIDELITY_MAP`, the canonical Fidelity opening index is the raw Tribute
ordinal after the opening owner is mapped to its unique Tribute. For
`S = max_tributes_per_work_shard`, range `j` is
`[S*j, min(S*(j+1), T))`; missing, duplicate or non-gap-free indexes are
invalid. Storage/chunk order is never an index. `AMOUNT_MAP(j)` consumes both
`FIDELITY_MAP(j)` and the fixed-reduce root: the matching leaf supplies each
Tribute's league observation, while the root supplies the global fraction
table. The fraction table alone cannot recover the Tribute-to-league mapping.

Unit specifications are uniquely derived in topological phase order, then by
interval `(start,end)` or `(level,index)`. They are enumerated by bounded cursor
and committed by roots/counts; no caller supplies or materializes the complete
vector. Each phase has an exact positional input-purpose grammar.

`wwd`, `lysis_budget` and `logical_evaluation_time` are immutable execution
inputs, not supervisor configuration. They are copied from the finalized
`JobIntentV1` context into `PlanCommitmentV1` and covered by `PlanHash`. A worker
authenticates the exact plan bytes/hash and their job/manifest bindings before
execution. Before signing, the node attestation gate independently reloads the
finalized intent and checks all three values against it.

Every `canonical_output_bytes` starts with the nested common header:

```text
RawTributeCoverageItemV1 {
  raw_ordinal: u32,
  tribute_id: EntityId36
}

WorkOutputHeaderV1 {
  source_coverage_root: Hash,
  output_coverage_root: Hash,
  source_coverage_count: u32,
  output_coverage_count: u32
}
```

Both coverage roots are list-kind `8` over canonical nested
`RawTributeCoverageItemV1` items; ordinals are strictly increasing and gap-free
in the represented raw-input subset. Empty coverage uses the list-kind `8`
empty root. Which exact subset is source/output coverage is fixed by the phase
schema; a worker cannot substitute a count-only claim.

The remaining nested bytes are selected solely by `phase` and the pinned Lysis
work-output schema manifest. The artifact fields must equal
`UnitIntervalCommitment`, `UnitInputRoot`, `UnitOutputSemanticDigest` and
`UnitCoverageCommitment` recomputed from the specification and decoded output.

`unit_artifact_root` in `LysisResultV1` is list-kind `7` over
`UnitArtifactDigest` values for every plan unit except the final
`ROOT_REDUCE` unit, in exact plan order. Excluding that final carrier avoids a
self-referential result/artifact digest. The `ROOT_REDUCE` specification is
still committed by `PlanHash`, and its complete output is committed by the
activation result/digest.

### 5.5 Actions and result

```text
NodActionV1 {
  raw_ordinal: u32,
  tribute_id: EntityId36,
  nod_id: EntityId36,
  owner: Address20,
  wwd: u32,
  league_id: u16,
  floor_price_minor: U256,
  gratis_load_minor: U256,
  entry_price_minor: U256,
  cost_amount_minor: U256,
  issuance_currency: u16,
  reference_currency: u16,
  issued_at: u64,
  bucket_key: Hash
}

ContributorActionV1 {
  owner: Address20,
  source_tribute_id: EntityId36,
  nominal_amount_minor: U256
}

CarryOverCreditActionV1 {
  source_wwd: u32,
  reason: enum u8 = UNUSED_LYSIS(1),
  amount: U256
}

MetadosisCompletionSummaryV1 {
  wwd: u32,
  pending_nonce: u64,
  day_type: u8,
  tribute_nominal_total: U256,
  day_limit: U256,
  gratis_demand: U256,
  gratis_supply: U256,
  lysis_budget: U256,
  auction_base: U256,
  nod_gratis_consumed: U256,
  unused_lysis: U256,
  carry_over_credit: U256,
  status: enum u8 = COMPLETED(1),
  logical_evaluation_height: u64,
  logical_evaluation_time: u64
}

ResultChunkV1 {
  protocol_bundle_hash: Hash,
  JobId: Hash,
  attempt: u32,
  chunk_ordinal: u32,
  first_nod_ordinal: u32,
  ordered_nod_actions: Vec<NodActionV1>,
  ordered_eligible_contributors: Vec<ContributorActionV1>
}
```

Within each result chunk, Nod actions are strictly ordered by
`(raw_ordinal, tribute_id)`, ordinals are gap-free from
`first_nod_ordinal`, and IDs/owners are unique. Contributors are strictly
ordered by `(owner, source_tribute_id)`. The ordered chunk catalog proves global
gap-free coverage and ordering. The bucket list used for `bucket_root` is
derived from Nod actions and sorted by `(bucket_key, raw_ordinal)`.

```text
ExactCountsV1 {
  tribute_count: u32,
  nod_count: u32,
  bucket_count: u32,
  contributor_count: u32,
  semantic_event_count: u32
}

ConservationTotalsV1 {
  tribute_nominal_total: U256,
  eligible_nominal_total: U256,
  day_limit: U256,
  gratis_demand: U256,
  gratis_supply: U256,
  lysis_budget: U256,
  auction_base: U256,
  nod_gratis_consumed: U256,
  unused_lysis: U256,
  carry_over_credit: U256,
  nod_cost_total: U256
}

LysisArithmeticSummaryV1 {
  input_manifest_hash: Hash,
  PlanHash: Hash,
  unit_artifact_root: Hash,
  fidelity_fraction_root: Hash,
  gratis_prefix_root: Hash,
  roots: ResultRootsV1,
  counts: ExactCountsV1,
  conservation: ConservationTotalsV1,
  first_error_ordinal: Option<u32>
}

ResultRootsV1 {
  nod_root: Hash,
  bucket_root: Hash,
  contributor_root: Hash,
  output_manifest_root: Hash
}

LysisResultV1 {
  protocol_bundle_hash: Hash,
  JobId: Hash,
  attempt: u32,
  input_manifest_hash: Hash,
  PlanHash: Hash,
  unit_artifact_root: Hash,
  fidelity_fraction_root: Hash,
  gratis_prefix_root: Hash,
  result_chunk_count: u32,
  result_chunk_list_root: Hash,
  carry_over_credit: CarryOverCreditActionV1,
  metadosis_completion_summary: MetadosisCompletionSummaryV1,
  tribute_count: u32,
  tribute_nominal_total: U256,
  unused_lysis: U256,
  roots: ResultRootsV1,
  counts: ExactCountsV1,
  conservation: ConservationTotalsV1,
  arithmetic_commitment: Hash,
  event_summary_hash: Hash
}

ActivationPayloadV1 {
  protocol_bundle_hash: Hash,
  JobId: Hash,
  attempt: u32,
  result_chunk_count: u32,
  result_chunk_list_root: Hash,
  roots: ResultRootsV1,
  counts: ExactCountsV1,
  conservation: ConservationTotalsV1,
  arithmetic_commitment: Hash,
  event_summary_hash: Hash
}
```

`ActivationPayloadV1` is reconstructed from the result; it is never an
independently trusted tuple. The result-chunk count/root commit all action bytes
without placing them in the activation transaction.

For a successful result, `first_error_ordinal` is `none` and:

```text
arithmetic_commitment = H(
  "OUTBE_OCOMP_LYSIS_ARITHMETIC_V1",
  canonical(LysisArithmeticSummaryV1)
)
```

The summary is reconstructed from the explicit result fields. A local failed
execution is never signed or activated and therefore has no
`LysisResultV1`.

### 5.6 Committee, certificate and activation

```text
OcompMemberV1 {
  validator_index: u8,
  validator_identity_hash: Hash,
  ocomp_public_key_sec1: [u8;33],
  key_epoch: u64,
  allowed_purpose_bitmap: u32,
  valid_from_height: u64,
  valid_until_height_exclusive: u64,
  proof_of_possession: [u8;64]
}

OcompCommitteeSnapshotV1 {
  chain_id: u64,
  genesis_hash: Hash,
  fork_id: Hash,
  protocol_bundle_hash: Hash,
  snapshot_epoch: u64,
  threshold: u8,
  ordered_members: Vec<OcompMemberV1>
}

OcompKeyRegistrationCoreV1 {
  chain_id: u64,
  genesis_hash: Hash,
  fork_id: Hash,
  protocol_bundle_hash: Hash,
  validator_index: u8,
  validator_identity_hash: Hash,
  ocomp_public_key_sec1: [u8;33],
  key_epoch: u64,
  allowed_purpose_bitmap: u32,
  valid_from_height: u64,
  valid_until_height_exclusive: u64
}

OcompKeyRegistrationV1 {
  core: OcompKeyRegistrationCoreV1,
  proof_of_possession: [u8;64]
}

OrderedSignatureV1 {
  validator_index: u8,
  signature_rs: [u8;64]
}

ExecutionCertificateV1 {
  result_committee_snapshot_hash: Hash,
  signer_bitmap: u8,
  ordered_signatures: Vec<OrderedSignatureV1>,
  ResultDigest: Hash
}

PoCActivationV1 {
  IntentId: Hash,
  finalized_intent_proof: FinalizedIntentProofV1,
  activation_payload: ActivationPayloadV1,
  result: LysisResultV1,
  certificate: ExecutionCertificateV1
}

CandidateAnnouncementV1 {
  protocol_bundle_hash: Hash,
  JobId: Hash,
  attempt: u32,
  result: LysisResultV1,
  ResultDigest: Hash,
  validator_index: u8,
  key_epoch: u64,
  signature_rs: [u8;64]
}

SignOnceRecordV1 {
  chain_id: u64,
  purpose: enum u8 = RESULT_SIGNATURE(1),
  JobId: Hash,
  attempt: u32,
  protocol_bundle_hash: Hash,
  committee_snapshot_hash: Hash,
  key_epoch: u64,
  ResultDigest: Hash,
  signature_rs: [u8;64]
}
```

The committee has exactly four members with indexes `0,1,2,3` in that order,
threshold `3`, unique validator identities and unique OCOMP keys.

### 5.7 Apply binding, receipts and active generation

```text
ActivationCallCoreV1 {
  IntentId: Hash,
  JobId: Hash,
  attempt: u32,
  protocol_bundle_hash: Hash,
  ResultDigest: Hash,
  activation_preconditions_hash: Hash,
  terminal_pending_nonce: u64
}

EffectBindingV1 {
  IntentId: Hash,
  JobId: Hash,
  attempt: u32,
  protocol_bundle_hash: Hash,
  ResultDigest: Hash,
  activation_preconditions_hash: Hash,
  activation_call_id: Hash
}

NodBatchReceiptV1 {
  binding: EffectBindingV1,
  nod_target_precondition: NodTargetPreconditionV1,
  nod_count: u32,
  nod_root: Hash,
  nod_amount_total: U256,
  nod_gratis_consumed: U256,
  issued_at: u64,
  state_event_digest: Hash
}

ContributorReceiptV1 {
  binding: EffectBindingV1,
  contributor_target_precondition: ContributorTargetPreconditionV1,
  contributor_count: u32,
  contributor_root: Hash,
  eligible_nominal_total: U256,
  state_event_digest: Hash
}

TributeReceiptV1 {
  binding: EffectBindingV1,
  tribute_input_binding: TributeInputBindingV1,
  sealed_collection_root: Hash,
  consumed_count: u32,
  consumed_nominal_total: U256,
  retired_generation: u64,
  state_event_digest: Hash
}

RequestBudgetSplitReceiptV1 {
  protocol_bundle_hash: Hash,
  wwd: u32,
  pending_nonce: u64,
  day_type: enum u8 = GREEN(1) | RED(2),
  day_limit: U256,
  lysis_budget: U256,
  auction_base: U256,
  destination: enum u8 = DESIS_AUCTION(1) | CARRY_OVER(2),
  desis_brief_hash: Option<Hash>,
  carry_over_credit: U256,
  auction_entry_price: U256,
  logical_anchor: u64
}

CarryOverReceiptV1 {
  binding: EffectBindingV1,
  source_wwd: u32,
  before_value: U256,
  credited_unused_lysis: U256,
  after_value: U256,
  state_event_digest: Hash
}

NodStateEventProjectionV1 {
  wwd: u32,
  target_generation: u64,
  namespace_root_before: Hash,
  nod_count: u32,
  nod_root: Hash,
  nod_amount_total: U256,
  nod_gratis_consumed: U256,
  issued_at: u64
}

ContributorStateEventProjectionV1 {
  series_id: u32,
  series_version_before: u64,
  series_version_after: u64,
  contributor_count: u32,
  contributor_root: Hash,
  eligible_nominal_total: U256
}

TributeStateEventProjectionV1 {
  wwd: u32,
  source_generation: u64,
  sealed_collection_root: Hash,
  consumed_count: u32,
  consumed_nominal_total: U256,
  retired_generation: u64
}

CarryOverStateEventProjectionV1 {
  source_wwd: u32,
  before_value: U256,
  credited_unused_lysis: U256,
  after_value: U256
}

ActiveGenerationV1 {
  JobId: Hash,
  program_semantics_hash: Hash,
  nod_root: Hash,
  bucket_root: Hash,
  contributor_root: Hash,
  output_manifest_root: Hash,
  exact_counts: ExactCountsV1,
  result_evidence_hash: Hash,
  availability_certificate_hash: Option<Hash>
}

AggregateActivationReceiptV1 {
  binding: EffectBindingV1,
  outcome: enum u8 = APPLIED(1) | CONFLICT_RESOLVED(2),
  nod_receipt_hash: Option<Hash>,
  contributor_receipt_hash: Option<Hash>,
  tribute_receipt_hash: Option<Hash>,
  carry_over_receipt_hash: Option<Hash>,
  request_budget_split_receipt_hash: Hash,
  active_generation_hash: Option<Hash>,
  effect_commitment: Hash,
  event_summary_hash: Hash,
  activated_at_height: u64,
  activated_at_time: u64
}
```

The four activation projection structs are nested canonical types, not tagged
OCB1 objects. Each receipt's `state_event_digest` is
`OwnerStateEventDigest(owner_kind, binding, projection)`. The aggregate
receipt's `event_summary_hash` is `ApplyEventSummaryHash` over those four
digests in the same fixed owner order. `LysisActivated` is emitted only after
the aggregate receipt exists and is checked directly against its binding and
terminal receipt hash, so it is intentionally not included in the summary and
cannot create a hash cycle.

An exact retry of a completed binding/digest returns the already recorded
`AggregateActivationReceiptV1`; it does not create a second receipt or effects.
`availability_certificate_hash` is `none` in the PoC.

For `APPLIED`, all four activation receipt hashes and `active_generation_hash`
are `some`. The request split receipt hash is always present.

For `CONFLICT_RESOLVED`, all five optional hashes are `none`; no activation
owner effect has run.

`effect_commitment` is:

```text
H(
  "OUTBE_OCOMP_EFFECTS_V1",
  nod_receipt_hash ||
  contributor_receipt_hash ||
  tribute_receipt_hash ||
  carry_over_receipt_hash
)
```

and exists only for `APPLIED`; the conflict receipt uses the registered empty
effects commitment generated by the same domain with an empty payload.

### 5.8 Job FSM record

The complete public job record is:

```text
OcompCompletedBindingV1 {
  JobId: Hash,
  activation_call_id: Hash,
  ResultDigest: Hash,
  result_evidence_hash: Hash,
  terminal_receipt_hash: Hash,
  terminal_receipt: AggregateActivationReceiptV1
}

OcompJobTerminalV1 {
  outcome:
    enum u8 = COMPLETED(1) | EXPIRED(2) | CONFLICTED(3) | CANCELED(4),
  terminal_height: u64,
  terminal_time: u64,
  next_pending_nonce: Option<u64>,
  completed_binding: Option<OcompCompletedBindingV1>
}

OcompJobRecordV1 {
  intent: JobIntentV1,
  status:
    enum u8 = OFFCHAIN_PENDING(1) | COMPLETED(2) |
              EXPIRED(3) | CONFLICTED(4) | CANCELED(5),
  terminal: Option<OcompJobTerminalV1>
}
```

The shape is closed:

- pending has `terminal=none`;
- completed has no next nonce and has an `APPLIED` completed binding;
- expired has a next nonce and no completed binding;
- conflicted has a next nonce and has a `CONFLICT_RESOLVED` completed binding;
- canceled is reserved in the codec but has no PoC producer.

The intent is retained in every terminal record. The bounded
`max_terminal_job_records=365` cap never authorizes silent eviction: when full,
new work defers. A later retention policy requires a new governed bundle.

## 6. Budget, precondition and result equations

Request creation commits one immutable split:

```text
day_limit = lysis_budget + auction_base
JobIntent.attempt = checked_u32(JobIntent.pending_nonce)

GREEN:
  request split destination = DESIS_AUCTION
  Desis supply = auction_base
  request carry_over_credit = 0

RED:
  request split destination = CARRY_OVER
  Desis supply = 0
  request carry_over_credit = auction_base
```

The request effect occurs once per WWD, not once per retry. The receipt hash is
part of `FrozenMetadosisValuesV1`.

Activation preconditions bind only live facts that can invalidate apply:

```text
tribute.wwd = nod.wwd = metadosis.wwd = JobIntent.wwd
contributors.series_id = JobIntent.wwd
metadosis.pending_nonce = JobIntent.pending_nonce

tribute.exact_count = authenticated_day_count
tribute.exact_nominal_total = authenticated_day_nominal
nod.max_nod_count = tribute.exact_count
contributors.max_contributor_count = tribute.exact_count
contributors.max_eligible_nominal_total = tribute.exact_nominal_total
```

These predicates are stored once in the intent. No owner reservation copy is
written. Intex and PromisLimit change only during certified activation; Desis
changed in the request phase.

For a valid result:

```text
T = tribute_count = nod_count
0 <= contributor_count <= T
0 <= bucket_count <= T

sum(NodAction.gratis_load_minor) + unused_lysis
  = frozen lysis_budget

carry_over_credit = unused_lysis

sum(ContributorAction.nominal_amount_minor)
  = eligible_nominal_total

nod action issued_at
  = request logical_evaluation_time

carry_over before + unused_lysis
  = carry_over after, using checked addition
```

No action may modify the already started Desis auction.

Nod IDs, bucket keys and Lysis arithmetic remain frozen by Lysis V1. The
activation verifier recomputes IDs, ordering, roots, counts, totals,
precondition membership and receipts.

It does not enumerate Tribute, call Fidelity/Oracle, dispatch Desis or execute
Lysis economics.

A valid activation uses one runtime-only `CertifiedLysisActivation` bound to
the checkpoint, selector, job, result and activation preconditions. It permits:

```text
NodFactory -> Intex -> Tribute -> PromisLimit -> Metadosis completion
```

The first four owners return typed receipts. The aggregate verifier consumes
them and produces the terminal permit.

A completed exact retry returns the stored receipt and performs no owner call
or event. A different completed binding rejects.

Unexpected target state becomes `CONFLICT_RESOLVED` only after complete evidence
and result verification. It does not undo or repeat the request split.

Owner failure or receipt mismatch reverts the activation checkpoint and leaves
the job pending.

## 7. Deadline and phase rules

If the request is created in block `R`:

```text
deadline_height = checked_add_u64(R, 64)
```

The deadline is exclusive:

- activation is valid only at block height `< deadline_height`;
- begin-zone at `deadline_height` expires/releases/requeues first;
- an activation transaction in that block observes the expired nonce and
  rejects.

The stable phase slots are:

```text
begin-zone:
  existing begin kinds through LateFinalizeCredits
  OcompLifecycleBegin:
    1. reserved mode/revocation barrier (PoC no-op)
    2. bounded OCOMP expiry/reset
  CycleTick
ordinary transactions, including activateLysis
CE sealing
end-zone OcompTerminalRequest:
  bounded READY inspection/request creation
commit; no later semantic writer
```

`OcompLifecycleBegin` is SystemTx V2 with exact four-byte selector ASCII
`OSE2` (`0x4f534532`) and an empty body. It is mandatory on the active fork,
ordered after `LateFinalizeCredits` and before `CycleTick`.
`OcompTerminalRequest` is SystemTx V2 with selector ASCII `OSR2`
(`0x4f535232`) and an empty body. It is the sole end-zone system kind. Import
and replay reject missing, duplicated, misordered or non-empty envelopes.

When the executor reaches the end-zone envelope it requires all ordinary
transactions to be consumed, invokes the existing idempotent compressed-entity
finalizer, requires the final CE root to be written, and only then executes the
request transaction. No user or system transaction may follow it. This makes
`JobIntentV1.ce_sealed_root` the root produced by the actual request block.

Expiry processes at most `max_expirations_per_block=1`, increments the pending
nonce once and inserts READY at `current_height + retry_backoff_blocks`.

It does not release owner reservations because none exist. It preserves the
frozen Lysis budget and the already committed request split.

With one-block backoff, the same block cannot create the next attempt. Conflict
uses the same increment only after complete evidence and result verification.

A future terminal no-retry producer must credit the full `lysis_budget` to
carry-over exactly once. The PoC retry path has no such producer.

Pre-fork behavior remains synchronous. At and after the PoC fork, an eligible
non-empty WWD uses the off-chain path and has no synchronous fallback. Empty
and ineligible cases retain their pre-fork semantics.

## 8. Generated capacity closure

### 8.1 Why the final numbers are generated

The final safe per-shard/per-chunk and activation byte/work caps depend on the
actual encoding, finality/storage proof, receipt/log shape, gas schedule, block
envelope and declared minimum devnet machine. A handwritten `256` per shard or
`512 KiB` per result chunk does not prove that each relevant interface safely
accepts its maximum object. Total Tribute count is not a generated admission
constant.

Therefore the candidate constants are upper bounds, not release claims.

### 8.2 Minimum PoC machine and headroom rule

Capacity is measured against one declared PoC class, not against an
unspecified developer laptop:

```text
OcompPocDevnetMachineV1
  architecture                  x86_64
  operating_system              Ubuntu 24.04
  logical_cpu_count             4
  nominal_memory_bytes          17179869184       # 16 GiB
  minimum_process_memory_bytes  12884901888       # 12 GiB after runner reserve
  nominal_root_disk_bytes       139586437120      # 130 GiB
  minimum_free_workspace_bytes  107374182400      # 100 GiB
  minimum_block_iops            8000
  minimum_block_throughput      250000000 bytes/s
  init_and_resource_manager     systemd + unified cgroup v2
  enclave_mode                  mock Gramine; no SGX claim
```

This matches the repository's already-used
[`depot-ubuntu-24.04-4`](https://depot.dev/docs/github-actions/runner-types)
class while making the actual minimum independent of a mutable provider label.
An equivalent/self-hosted runner may be used only when its observed facts meet
or exceed every literal. The run manifest records those facts and derives
`minimum_devnet_hardware_profile_hash`; a smaller or unknown machine cannot
produce capacity evidence.

The exact PoC headroom rule is:

```text
benchmark_cold_runs = 5
required_headroom_basis_points = 2000  # 20%
```

All five runs use the same clean binaries/config/profile and cold job/CAS
namespace. A failed/timeout run rejects the candidate and is not retried away.
For the worst observation across the five runs, maximum-shaped `cap` must use
at most 80% of every declared transaction/block/gas/internal-work/network
budget, assigned memory/cgroup limit, CAS quota and measured block-processing
time budget. The node/OS reserved floor is excluded before assigning the OCOMP
slice. `cap+1` must reject at the intended semantic/interface bound before
unbounded allocation, host OOM or disk exhaustion. These measurements select
the disposable PoC cap; they are not a production SLO or supported-network
capacity claim.

### 8.3 Required generated fields

The checked-in `OcompPocLimitsV1` manifest must contain exact literals for at
least:

```text
max_tributes_per_work_shard
max_inputs_per_work_unit
max_records_per_input_chunk
max_result_chunk_bytes
max_result_summary_bytes
max_activation_payload_bytes
max_finalized_intent_proof_bytes
max_execution_certificate_bytes
max_activation_ocb1_bytes
max_activation_calldata_bytes
max_transaction_rlp_bytes
max_receipt_bytes
max_log_count
max_log_bytes
max_input_manifest_bytes
max_input_chunk_bytes
max_fidelity_openings_per_work_shard
max_oracle_openings
max_oracle_wwd_pair_entries
max_active_scurve_entries
max_opening_bytes
max_nod_actions_per_result_chunk
max_contributor_actions_per_result_chunk
max_bucket_records_per_result_chunk
max_protocol_collection_items
max_activation_storage_reads
max_activation_storage_writes
max_activation_root_transitions
max_signature_verifications
activation_base_gas
activation_per_input_byte_gas
activation_per_root_transition_gas
activation_per_opening_or_proof_byte_gas
activation_per_signature_gas
max_activation_gas
max_activation_internal_work
source_retention_after_terminal_blocks
minimum_devnet_hardware_profile_hash
benchmark_report_hash
benchmark_cold_runs
required_headroom_basis_points
```

Every parser checks the applicable bytes/count field before allocating or doing
cryptography. The smallest enforced value across RPC, transaction input,
txpool, P2P, block, gas, CE and replay becomes the protocol value.

### 8.4 Generation algorithm and acceptance

The generator:

1. starts with work-shard capacity `S=256` and constructs maximum-shaped
   individual input/result chunks plus a maximum-shaped constant-size
   activation summary, including the worst permitted Oracle ranges, receipts,
   finality/storage proof and three signatures;
2. proves the planner maps `S-1`, `S` and `S+1` inputs to complete canonical
   shard sets, with `S+1` producing a second shard and no rejection or
   omission; it also derives exact unit counts for 10,000 and 1,000,000,000
   without proportional plan allocation;
3. derives every object and transaction size from production encoders;
4. runs activation-byte `cap-1`, `cap` and `cap+1` through the public
   four-node path;
5. performs the five required cold runs and records CPU/block-processing time,
   peak memory, disk writes, network bytes, gas and finality latency on the
   declared minimum machine;
6. lowers per-interface bounds when necessary until the worst run satisfies the
   exact 20% headroom rule at every layer; activation/chunk `cap+1` rejects at
   the intended earliest bound while `S+1` remains an accepted two-shard job;
7. emits the limits manifest, Rust constants, network manifest and evidence;
8. reruns the maximum block on proposer, validator/import and historical replay.

The fork cannot activate if:

- any generated field is absent, zero where zero is not semantic, or differs
  from the compiled constants;
- any layer accepts a larger semantic object than consensus can replay;
- cap behavior differs between RPC, txpool, P2P, proposal, import or replay;
- `S` exceeds 256, any bounded chunk/summary exceeds its generated cap, or any
  generated field introduces a total Tribute/unit/chunk-count ceiling;
- the benchmark/environment/evidence hash is missing.

Local supervisor/worker quotas may be smaller and cause abstention. They cannot
change consensus eligibility, object bytes or compiled maxima.

## 9. Mandatory shape-freeze and arming gates

Final capacity cannot be measured before the real public runtime exists, while
that runtime cannot safely be implemented against unfrozen schemas. The
dependency is resolved with two fail-closed gates, not a guessed capacity and
not one circular task.

Before job FSM, exporter, worker, signer or activation runtime tasks merge,
`P0-PROTOCOL-SHAPE-FREEZE` must check in:

```text
protocol-identifiers registry
crypto-profile registry
OCB1 object registry
canonical schema manifest
domain/preimage manifest
correctness profile
candidate compile-ceiling manifest (never a release claim)
ProtocolBundleV1 field template with no checked-in armable network binding
positive and negative schema/hash vectors using a marked measurement fixture
generator invocation and source revision
```

Dependent code may merge only while no checked-in network/fork schedule can
select the provisional fixture. The capacity harness may generate a disposable
ephemeral measurement chain to exercise the real public path; its manifest and
history are retained only as measurement evidence and can never satisfy PoC
closure. Runtime code reads generated registries and compile ceilings; it must
not hardcode a provisional genesis hash, bundle hash, committee or final
capacity value.

After the complete public four-node vertical slice exists,
`P1-POC-CAPACITY-AND-ARMING` runs the section 8.3 algorithm and checks in:

```text
generated capacity profile
ProtocolBundleV1 bytes and ProtocolBundleHash
fresh-devnet base genesis and genesis hash
network-binding artifact
four-member OCOMP committee/key-registration public artifacts
final chain manifest binding fork height, bundle and committee snapshot
final cap-1/cap positive vectors and cap+1 negative vectors
all final chain/bundle-bound positive and negative golden vectors
benchmark/environment/evidence manifest
generator invocation and source revision
```

Only this second task may check in and arm the PoC fork in the canonical
fresh-devnet schedule. It
regenerates every chain/genesis/bundle-dependent vector and proves that no
provisional measurement manifest is accepted as final closure evidence. Because
the provisional bytes exist only in disposable measurement history, lowering a
candidate ceiling and replacing its fixture does not reinterpret authoritative
history.

Every vector record in either gate contains:

```text
vector_id
object_kind
schema_version
semantic JSON display value
canonical_hex
expected_digest_or_id
expected_decode_outcome
expected_error_code for negative vectors
generator artifact hash
independent verifier result
```

Required positive vectors include every top-level kind, every `UnitSpecV1`
phase/interval pair, empty/non-empty lists, all four signer indexes, each valid
three-signer quorum and cap-1/cap objects.

Required negative vectors include:

- wrong magic/kind/version/body length;
- every truncation point and trailing byte;
- invalid boolean/option/enum;
- duplicate/out-of-order vectors and signatures;
- wrong chain/genesis/fork/bundle/JobId/attempt/deadline;
- wrong domain or one-byte payload mutation;
- high-`s`, zero/overflow scalar, invalid key, bad proof of possession;
- wrong bitmap, unused bitmap bits, duplicate/unknown/missing signer;
- malformed header/finality/storage/opening nested bytes;
- root/count/total/precondition/budget mismatch;
- cap+1 before allocation/cryptography.

The production Rust codec/hasher/verifier and one test-only implementation that
does not call the production encoder must agree on accepted bytes and hashes.
Foundry/Alloy independently confirm the ABI selector/calldata envelope.

CI also scans production code for unregistered OCOMP domain, selector, tag and
codec literals. Regenerating without a byte change must be reproducible; any
byte change requires a new object/bundle version, never reinterpretation.

## 10. Minimality and deferred work

This freeze deliberately selects:

- one codec instead of per-object serialization libraries;
- one existing public Metadosis address and one Lysis-specific selector instead
  of a new generic execution address;
- no compression, proof system, external DA or aggregate signature;
- one static committee/key epoch;
- bounded authenticated result chunks outside the transaction and their
  constant-size count/root inside it;
- typed activation preconditions, result chunks, root transitions and receipts
  rather than maps or generic write sets;
- one request-phase budget split and no live-auction top-up.

It does not add:

- `ProgramRegistry`, `TaskAdapter`, uploaded code or dynamic dispatch;
- `execute(program_id, bytes)`;
- arbitrary storage keys/calls;
- a second off-chain program;
- remote mTLS, production custody/DA, proof verification or supported-network
  rollout;
- production GC/recovery/upgrade hardening.

Decision ticket 5 selected the exact existing finalized-checkpoint/opening
producers. Decision ticket 6 selected the smallest sibling-process/CAS
topology and, while doing so, made the already-present `transport_digest`
field explicit as `keccak256(exact stored bytes)`. Decision tickets 7 and 8 map
these frozen bytes to signer and activation code. None may change this
wire contract without reopening ticket #4 and producing a new bundle version.

## 11. Completion evidence for decision ticket #4

This decision is resolved because it now fixes:

- fork/profile/protocol identity;
- canonical object grammar, tags, public ABI, error codes and event topics;
- hash domains, ID derivations, list roots and signatures;
- exact outer field order for intent, proof, input, unit, result, certificate,
  activation, preconditions, budget split, receipts, job FSM and active generation;
- exclusive deadline, retry and exact begin/end system phase behavior;
- candidate limits and the only allowed way to obtain final literals;
- a blocking golden-vector/capacity gate before dependent runtime work.

No ubiquitous-language term changed: the existing `CONTEXT.md` terms remain
consistent, so this ticket does not modify the domain glossary.
