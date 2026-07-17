# ADR-012: Carry the execution-sealed compressed-entity root in every block header

- **Status:** Superseded; historical input only
- **Canonical mapping:** [`docs/adr/legacy-reconciliation.md`](../docs/adr/legacy-reconciliation.md)
- **Date:** 2026-07-18
- **Depends on:** ADR-011

## Context

ADR-010 commits every compressed-entity collection through one Root Catalog into `R_sealed`, and ADR-011 adds atomic Tribute partition retirement. Execution stores the post-block value in `0xEE0D.slot1`; validators independently recompute it, and finalized CE MDBX persists the matching rebuildable tree materialization.

That EVM slot is sufficient for execution but is inconvenient as an external trust anchor. A client selecting finalized block `B` would otherwise need an EVM account/storage proof merely to discover `R_sealed(B)` before it could verify the compressed-entity proof introduced by ADR-013. Snapshot verification would have the same extra dependency.

Outbe already carries hash-committed records in the standard Ethereum header `extra_data` through `OutbeBlockArtifacts`. Tag `0x07` is assigned to committee pre-announcement; `0x08` is the next unused tag. The active pre-production envelope version is `0x0A`, and the planned coordinated testnet reset discards all earlier header history.

## Starting system

After ADR-011:

- `CompressedEntitiesLifecycle::end_block()` returns `SealOutput { parent_root, new_root, staged_tree_batch }`;
- the lifecycle writes `new_root` to EVM `0xEE0D.slot1` before state-root calculation;
- the proposer retains the immutable staged tree batch until block assembly supplies the final block hash;
- validator execution independently produces the same `SealOutput` but does not publish speculative CE state;
- finalized apply already binds scheme, height, block hash, parent hash, parent root, and new root in one marker-last CE MDBX transaction;
- `OutbeBlockArtifacts` carries execution summary, consensus/DKG records, timestamp milliseconds, and late-finalize credits under the shared 64 KiB `extra_data` cap;
- the current proposer encodes final `extra_data` before CE finalization, so it cannot yet carry the execution-produced CE root;
- external consumers have no direct header carrier for `R_sealed`.

## Added capability

Every post-genesis block carries its own execution-computed post-state `R_sealed` and commitment-scheme identifier in a fixed `OutbeBlockArtifacts` record. Validators require byte-exact equality among the header carrier, independently computed `SealOutput`, and post-state EVM slot. A finalized header therefore becomes the direct trust anchor used by later proofs and snapshots.

## Decision

### Fixed tag `0x08` payload

The new record is:

```text
COMPRESSED_ENTITIES_ROOT_TAG = 0x08

payload_len = 36
payload =
    commitment_scheme_version_BE4
    || R_sealed_BE32
```

The typed model is conceptually:

```rust
CompressedEntitiesRootArtifact {
    commitment_scheme_version: u32,
    r_sealed: B256,
}
```

`OutbeBlockArtifacts` carries it as an optional field at the structural codec layer because empty genesis data, pre-final proposer artifact fragments, and standalone artifact helpers must still decode. Mandatory presence is a block-execution rule, not a generic byte-decoder rule.

The field uses the same unsigned big-endian `u32` scheme identifier already used by CES1 body commitments and finalized CE markers. `R_sealed` is the exact 32-byte canonical field encoding written to EVM; it is not hashed, reversed, RLP-wrapped, or converted through a `U256` surrogate inside the record.

The generic codec rejects duplicate tag `0x08`, any payload length other than 36, truncation, unknown tags, and trailing bytes. It remains structurally round-trippable; semantic validation rejects zero/unsupported scheme values and any root that differs from locally computed execution. The codec does not depend on block height, chain state, CE MDBX, or the fork schedule.

The encoder has one fixed record order and one implementation of framing/size accounting. Exact bytes, including the chosen position among existing records, are pinned by golden vectors during the implementation seam review. No second CE-root-only parser or encoder is introduced; convenience extraction delegates to the complete `OutbeBlockArtifacts` codec.

### The carrier contains the current block's post-state root

For every executed block `B >= 1` in the reset chain:

```text
header(B).compressed_entities_root.r_sealed
    == SealOutput(B).new_root
    == post_state(B)[0xEE0D.slot1]
```

The artifact is mandatory even when no compressed entity changed and the value equals the parent root. Absence never means "unchanged" and a verifier never searches backward for an earlier carrier.

Genesis block `0` is the sole exception:

```text
genesis header.extra_data:
  empty under the existing genesis convention

genesis EVM state:
  0xEE0D.slot1 = R_sealed(0)

genesis CE MDBX marker:
  new_root = R_sealed(0)
```

A proof or snapshot at height `B >= 1` uses header `B`, not header `B+1`. Genesis-specific verification remains anchored in the chain specification/genesis state rather than a synthetic tag `0x08` record.

The root remains inside standard Ethereum `Header.extra_data`. ADR-012 adds no Outbe-specific top-level RLP header field, so the block hash remains `keccak256(rlp(standard_ethereum_header))`; changing the carrier bytes changes that ordinary header hash.

### Scheme field asserts the fork schedule

The artifact does not negotiate a commitment scheme. The protocol schedule selects exactly one scheme for the post-state of block `B`:

```text
artifact(B).commitment_scheme_version
    == commitment_scheme_at_height(B)
```

For the reset chain, every block `B >= 1` carries scheme `1`.

At a future scheme activation block `H`:

```text
B < H:
  artifact labels the old-scheme post-state

B = H:
  execution transition produces the new-scheme post-state
  artifact labels the new scheme and new R_sealed(H)

B > H:
  artifact continues to label the new scheme
```

Thus the field always describes the root beside it, not the parent root or the scheme used by block `H-1`. ADR-018 must define any logical-state migration/rebuild between old parent materialization and new post-state scheme; ADR-012 does not permit a proposer, operator, CLI flag, or local database to select another version.

Unknown, old, or future schemes outside their scheduled interval are deterministic invalid-block errors even if the implementation knows how to decode them. There is no try-another-scheme fallback.

### Execution remains authority; the header is a carrier

`0xEE0D.slot1` remains the execution authority and exact-parent input. The proposer never supplies a root that execution writes into that slot. Instead:

```text
compressed-entity execution
    -> computes R_sealed
    -> writes EVM slot1
    -> returns the same value in SealOutput

block builder
    -> copies SealOutput.new_root into tag 0x08
```

Validator execution ignores the header root as a state-transition input. It independently executes all system/user work, seals compressed entities, and then requires:

```text
artifact.scheme == fork-active scheme
artifact.root   == local SealOutput.new_root
artifact.root   == post-state 0xEE0D.slot1
```

The lifecycle already writes and returns one value under the same checkpoint; implementation should expose that result rather than recompute the tree or add another root provider. Explicit tests still verify the three representations cannot diverge.

A root in an unfinalized candidate header is only a hash-committed claim. It becomes an external trust anchor when consensus finalizes that exact block hash. ADR-012 makes no claim that an arbitrary header returned by an untrusted adapter is finalized.

### Narrow `SealOutput` to block-builder seam

The compressed-entity module remains unaware of header encoding. Its interface ends at the existing typed result:

```text
CompressedEntitiesLifecycle::end_block()
    -> SealOutput.new_root
```

The block builder owns the adapter from that result to `OutbeBlockArtifacts`. This keeps collection/shard/Catalog preparation behind the CE module while header representation, artifact coexistence, size limits, and block hashing remain local to the block assembly module.

The proposer path is reordered conceptually:

1. execute begin-zone, user, and remaining block work;
2. run CE `end_block` while the state-root hook is still attached;
3. obtain the one `SealOutput` and require its root equals post-state `0xEE0D.slot1`;
4. decode the pre-final artifact fragment;
5. overwrite execution-produced fields: execution summary, timestamp millis, and compressed-entity root;
6. encode final `OutbeBlockArtifacts` within the 64 KiB limit;
7. set the executor's final artifact bytes and run final semantic validation;
8. compute/finalize the EVM state root, including CE root/overlay-cleanup writes;
9. assemble and hash the standard header;
10. freeze/publish the staged CE candidate under that exact block hash.

The parallel trie and synchronous state-root paths consume the same stored `SealOutput`. A path that already finalized CE to deliver cleanup writes to the parallel trie must not finalize it a second time; a path that has not finalized it must do so before final artifact encoding. Missing, duplicated, or dropped `SealOutput` is fatal block assembly.

There is no circular dependency: `R_sealed` depends on deterministic post-block CE state, not on the current block hash. The final block hash binds the already-computed root and is then attached to the immutable staged candidate.

If final block-size/RLP validation rejects the assembled payload, the existing exact candidate-discard path removes only that block hash/height candidate. No candidate is published under pre-root artifact bytes.

### Payload attributes cannot select the root

Consensus/DKG artifacts and late-finalize credits may arrive as pre-final payload artifact fragments. A CE root found in proposer input or next-block payload attributes is stale/untrusted and is always removed during proposer sanitization, like the execution summary and timestamp-millis fields that execution recomputes.

The locally computed root is inserted only after CE sealing. Proposer input cannot choose, preserve, merge, or override it. Standalone artifact helpers may structurally encode/decode an object without tag `0x08`, but they do not produce a valid final post-genesis block by themselves.

Validator/import execution receives the sealed header bytes unchanged; it does not sanitize them. It checks mandatory presence and scheme before expensive block execution, then performs root equality after CE `end_block`.

### Structural codec, centralized semantic execution validation

Validation responsibilities remain separated:

```text
OutbeBlockArtifacts codec:
  framing, lengths, duplicates, known tags, full consumption

OutbeBlockExecutor for an existing block:
  pre-exec mandatory presence for B >= 1
  pre-exec scheme == fork schedule
  post-exec root == SealOutput == EVM slot1

proposer block builder:
  strip input root
  insert local SealOutput root
  run the same final semantic equality before assembly

finalized persistence coordinator:
  recheck canonical header == historical EVM == candidate/MDBX root
```

The consensus/DKG module may structurally decode the complete artifact envelope for its own records, but it does not implement a second compressed-entity transition verifier. Missing/wrong scheme/root is a deterministic invalid-block condition, not a soft system-transaction receipt and not a local Mongo/MDBX availability outcome.

If a supposedly finalized canonical block later fails header/EVM/candidate equality during local persistence, the node treats that as local corruption/inconsistent durable input, stops readiness/ACK progression, and enters ADR-015 recovery policy. It does not reinterpret the already-finalized network decision or emit a negative consensus vote.

### Finalized marker remains unchanged

The existing `FinalizedMarker` already stores:

```text
commitment_scheme_version
height
block_hash
parent_block_hash
parent_root
new_root
```

ADR-012 adds no duplicate `header_root` or `header_scheme` fields and does not change the CE MDBX local schema. Before marker-last apply, the coordinator loads the exact canonical header and historical EVM slot and requires:

```text
header.number/hash                    == finalized target
header artifact scheme/root           == fork-active scheme/historical slot
candidate parent/new roots            == durable parent/header root
marker to be written scheme/new_root  == header artifact scheme/root
```

After those preconditions, the ordinary marker's `new_root` is the header root. Storing it twice would only create additional corruption states. The block hash already binds the complete header artifact bytes.

Candidate-present and canonical-replay-without-candidate paths perform the same comparison before CE MDBX commit and Marshal ACK. ADR-015 later expands restart reconciliation evidence but does not need a new marker field to discover the finalized header commitment.

### Fixed 39-byte reservation inside the existing cap

The mandatory record consumes:

```text
record tag/length framing = 3 bytes
scheme                    = 4 bytes
R_sealed                  = 32 bytes
-------------------------------------
mandatory record          = 39 bytes
```

`OUTBE_MAX_EXTRA_DATA_SIZE` remains exactly 64 KiB. After reset, variable/non-root artifact producers must reserve 39 bytes:

```text
pre-final non-root encoded size
    <= OUTBE_MAX_EXTRA_DATA_SIZE - 39

final encoded artifacts including tag 0x08
    <= OUTBE_MAX_EXTRA_DATA_SIZE
```

The central artifact module exports one size constant/helper; DKG, late-finalize, payload, and test code do not duplicate `39`. The final encoder remains the authoritative aggregate size check.

This reservation prevents an honest boundary/dealer/credit artifact that fits alone from failing only after full execution when the mandatory root is appended. Existing exact-limit and maximum-variable-payload tests move down by 39 bytes. The overall block transport and RLP caps do not increase.

### No new root RPC

ADR-012 adds no `outbe_*` root endpoint. The trust-anchor bytes are already available from the standard block header `extraData`:

```text
block header
  -> OutbeBlockArtifacts
  -> tag 0x08
  -> { scheme, R_sealed }
```

A shared typed decoder avoids parser duplication inside node modules and external libraries, but an RPC wrapper over the same bytes would add no cryptographic guarantee. The client remains responsible for selecting/verifying the exact finalized header.

ADR-013's proof response binds its evidence to a block number/hash but does not echo scheme/root; the client obtains those only from tag `0x08` in the selected finalized header and compares the recomputed proof root directly. ADR-012 does not prematurely define proof RPC availability or error semantics.

### Pre-production reset retains envelope version `0x0A`

Adding tag `0x08` changes the accepted post-reset schema but does not consume a new `OutbeBlockArtifacts` version. The coordinated testnet reset discards every earlier header, candidate, Mongo projection, and CE MDBX materialization; only one wire history exists from the new genesis.

Therefore:

```text
genesis:
  extra_data empty

block 1+:
  envelope version 0x0A
  tag 0x08 mandatory
```

An old binary using the earlier `0x0A` decoder still fails closed on the unknown `0x08` tag, so reusing the pre-production version does not allow silent mixed interpretation. The reset and coordinated binary rollout prevent old/new `0x0A` blocks from coexisting in one canonical history.

This deliberately supersedes the earlier concept note that every addition must bump the envelope version. Once a wire history must be preserved, a future schema change uses an explicit new version and height-aware historical decoding. ADR-012 adds no dual decoder, legacy fallback, or operator compatibility mode.

## Working result

After implementation:

- every block `B >= 1` contains `{ scheme(B), R_sealed(B) }` under tag `0x08`;
- the carrier is included even on zero-change blocks;
- proposer assembly derives it only from local CE `SealOutput` after end-block sealing;
- validators independently reproduce and compare the same root with EVM slot 1;
- the standard header hash commits the carrier without changing Ethereum RLP shape;
- finalized apply requires header, historical EVM, staged/replayed candidate, and marker equality before CE MDBX commit/ACK;
- standard block retrieval is sufficient to extract the trust anchor without a dedicated RPC;
- ADR-013 and ADR-016 can bind proof/snapshot evidence directly to the selected finalized header.

## Accepted limitations

- ADR-012 carries a root but does not serve entity proofs, verified bodies, or snapshots.
- A header artifact is not proof of finality by itself; clients need the chain's finalized block selection/certificate assumptions.
- Genesis has no tag `0x08`; its root is verified through genesis configuration and EVM/CE initialization.
- The first chain supports only commitment scheme `1`; cross-scheme transition mechanics remain ADR-018 work.
- `extra_data` consumers must understand Outbe's OART codec; there is no root-only convenience RPC.
- The root consumes a permanent 39-byte share of the existing 64 KiB artifact budget.

## Consequences and trade-offs

Benefits:

- one finalized header directly anchors compressed-entity state at the same height;
- proof/snapshot verification no longer needs an EVM storage proof merely to discover `R_sealed`;
- execution remains the sole state-transition authority;
- the existing deep `SealOutput` interface is reused rather than exposing tree internals to block assembly;
- EVM, header, candidate, and finalized marker mismatches fail closed at explicit lifecycle seams;
- no new RPC, top-level header field, CE MDBX format, or operator setting is introduced.

Costs:

- proposer finalization/extra-data assembly ordering changes;
- valid blocks require CE sealing even when the root is unchanged;
- finalization performs another header/EVM/candidate equality check;
- variable consensus artifacts lose 39 bytes of budget;
- the testnet reset discards the earlier header history to avoid permanent dual codec support.

Rejected alternatives:

- carrying only `R_sealed`, because external verifiers need an explicit algorithm identifier;
- placing the root inside `ExecutionSummaryArtifact`, because fee settlement and compressed-state commitment are separate modules;
- carrying the parent root, because proofs/snapshots would be shifted by one block;
- omitting the artifact on unchanged blocks, because absence would require backward search and another semantic state;
- allowing the header to write EVM slot 1, because proposer input is not execution authority;
- letting consensus/payload attributes supply an expected root, because that creates a second source and a pre-execution cycle;
- duplicating CE root validation in consensus/DKG code, because semantic transition validation belongs with execution;
- storing header root separately in the finalized marker, because `new_root` plus `block_hash` already contains the information;
- adding a root-only RPC, because the bytes are already in the standard header;
- increasing the 64 KiB cap, because a fixed 39-byte carrier does not justify changing transport limits;
- bumping OART to `0x0B` despite discarding all prior history, because it would consume a pre-production version without adding safety;
- retaining both old and new `0x0A` histories, because that would require ambiguous or height-dependent compatibility behavior under one version.

## Verification

### Codec and wire vectors

Pin:

- tag `0x08`;
- payload length 36 and total record cost 39;
- `u32` big-endian scheme and exact BE32 root bytes;
- root-only, root plus every existing artifact kind, and maximum combined envelopes;
- deterministic encoder record order and complete envelope checksum;
- exact post-reset block-1 `extra_data` bytes.

Reject duplicate root tags, payload lengths 0/35/37/maximum, truncation at every byte boundary, unknown tags, wrong envelope version, trailing data, record-count mismatch, and total size above 64 KiB. Structural decode may represent zero/unknown values; semantic execution tests must reject them.

### Proposer ordering and candidate binding

Test both parallel-trie and synchronous-root paths:

- CE finalization occurs exactly once;
- cleanup state changes reach the trie before root calculation;
- `SealOutput.new_root` populates tag `0x08`;
- stale/malicious roots from payload attributes are removed and cannot survive final encoding;
- execution summary, timestamp millis, consensus artifacts, late credits, and root coexist without loss;
- block hash changes when only tag `0x08` changes;
- staged candidate publication uses the hash of the header containing that exact root;
- final size/RLP rejection discards only the matching candidate.

### Validator parity and invalid blocks

Using independent parent MDBX/Mongo materializations, assert proposer and validators produce equal:

- post-state EVM slot 1;
- `SealOutput.new_root`;
- header artifact scheme/root;
- state root, receipts, gas, logs, and block hash.

Reject before execution when a post-genesis header omits tag `0x08` or advertises the wrong scheme. Reject after execution for wrong root, parent root instead of current root, bit-flipped root, zero root, correct root under a wrong scheme field, or EVM/SealOutput divergence. These failures are fatal invalid-block results, never soft receipts.

Cover zero-change blocks, multi-collection mutations, Tribute retirement plus Nod updates, the structurally empty Catalog wrapper, block 1, and future activation-edge fixtures where block `H-1` labels the old scheme and block `H` labels the new post-state scheme.

### Finalized persistence and recovery seam

For candidate-present and replay-without-candidate paths, inject mismatches among:

- finalized block number/hash;
- tag `0x08` scheme/root;
- historical `0xEE0D.slot1`;
- staged/replayed parent and new roots;
- existing CE MDBX marker.

No mismatch may change CE MDBX or advance Marshal ACK. Successful apply writes the existing marker fields only, and commit-before-ACK redelivery remains idempotent. Restart/open-parent tests require the parent header carrier, historical EVM slot, and marker identity to agree; ADR-015 later broadens classification and repair evidence.

### Artifact and block size budgets

Exercise exact `OUTBE_MAX_EXTRA_DATA_SIZE - 39` non-root payloads, final envelopes exactly at 64 KiB, and one-byte-over rejection. Include maximum boundary, dealer, committee pre-announcement, late-credit, execution-summary, and timestamp combinations allowed by their protocol caps. Confirm the final Outbe transport/RLP block guards include the root record.

### External extraction

Retrieve a finalized block through the existing standard block interface, decode tag `0x08`, and compare it with the execution/post-state fixture without calling a new root RPC. Tampering with `extraData`, block hash, scheme, or selected height must invalidate the fixture binding.

## Reset policy

ADR-012 is included in a coordinated full testnet reset. The reset rebuilds genesis EVM state, CE MDBX, Mongo projection, candidates, and checkpoints under the existing scheme-1 ADR-010/011 topology. It preserves the formulas and provisional K=16 but discards all headers that used `0x0A` without mandatory tag `0x08`.

The post-reset genesis verifies the same non-zero `R_sealed(0)` among derivation, `0xEE0D.slot1`, and the CE MDBX height-0 marker while leaving genesis `extra_data` empty. Block 1 is the first mandatory carrier. There is no activation-height compatibility branch, dual envelope decoder, backfill, synthetic genesis event, or in-place history migration.

Before implementation, ADR-012 must be reviewed against the actual ADR-010/011 `SealOutput`, block-builder parallel-root ordering, OART codec/version then active in the branch, candidate publication/discard path, historical EVM reader, and finalized coordinator. Concrete encoder record order and fixture bytes are pinned only after that seam review without changing the decisions above.

## Next unlocked step

ADR-013 defines collection/shard/Catalog membership and non-membership proofs plus verified point reads bound to `{block_number, block_hash, commitment_scheme_version, R_sealed}` extracted from the finalized header.
