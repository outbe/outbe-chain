# ADR-003: Publish complete off-chain records in receipt events

- **Status:** Proposed
- **Date:** 2026-07-15
- **Depends on:** ADR-002

> **Supersession note:** ADR-006 replaces the explicit-field Stored/Delete ABI defined here during the single coordinated ADR-003–010 first CES1 testnet reset. ADR-003 remains authoritative for receipt visibility, complete mutation publication, owning emitters, journal/revert atomicity, system-receipt inclusion, and deterministic log ordering. The active ADR-006 wire form publishes strict-canonical Protobuf payloads, 36-byte IDs, schema/commitment versions, and previous/new commitments instead of duplicating every body field as Solidity arguments.

## Context

ADR-001 introduced the domain-neutral off-chain storage facade. ADR-002 added typed Tribute and Nod repositories for:

- `TributeData`;
- `NodItemState`;
- `NodBucketState`;
- Tribute owner/day indexes;
- Nod owner/global enumeration.

The live node still stores and reads these records through EVM state. The repositories cannot yet be rebuilt from canonical chain data because the existing product events do not contain every repository field:

- `TributeIssued` omits `reference_currency`, `tribute_price_minor`, and `exclude_from_intex_issuance`;
- `NodIssued` omits `bucket_key`, `issuance_currency`, `reference_currency`, and `issued_at`;
- `NodBucketQualified` describes one business transition but does not describe bucket creation, count changes, or deletion.

The next step is to publish every complete off-chain record mutation in normal transaction receipts. ADR-004 can then project those receipts into MongoDB without re-executing Tribute or Nod business logic.

This ADR defines only the receipt contract and emission ownership. It does not add ExEx, Mongo writes, runtime Mongo reads, body commitments, or a generic compressed-entity event engine.

## Starting system

The node has working Memory/Mongo storage adapters and typed repositories, but:

- live Tribute/Nod execution still uses EVM bodies;
- repository writers are exercised only in tests;
- MongoDB has no canonical chain input;
- current product events are insufficient to reconstruct all repository records;
- Nod bucket state can change through issue, mining, and lifecycle qualification.

## Added capability

A complete, receipt-visible, ordered mutation stream for all three future off-chain record types.

For every successful record change, receipts contain either:

- the complete resulting record (`Stored`); or
- the canonical record identity (`Deleted`).

## Decision

### Domain-specific emitters

Keep projection events domain-specific at this stage.

- Tribute record events are emitted from `TRIBUTE_ADDRESS` and declared in `ITribute.sol`.
- Nod item and bucket events are emitted from `NOD_ADDRESS` and declared in `INod.sol`.
- `INodFactory.sol` keeps its existing product-level `NodIssued` and `NodBurned` events.

Do not introduce a generic compressed-entity emitter, reserved engine address, runtime domain registry, or domain ID. Those concepts belong to the later generic lifecycle and commitment ADRs.

The state-owning domain address is the authoritative emitter for its projection events. ExEx will use both event signature and emitter address when ADR-004 defines its filter.

### Projection events are separate from product events

Keep all existing product events:

- `TributeIssued`;
- `TributeBurned`;
- `TributeWorldwideDaySealed`;
- `NodIssued`;
- `NodBurned`;
- `NodBucketQualified`.

Add separate projection events rather than changing existing event signatures or repurposing their semantics.

Product events explain a business operation to external consumers. Projection events describe the complete resulting materialized record. A successful operation may emit both kinds.

### Stored and Deleted semantics

Projection does not distinguish create from update.

```text
*BodyStored   complete current record; projector performs upsert
*BodyDeleted  record identity; projector performs delete
```

Do not add an operation enum. Existing product events already distinguish issuance, mining, and qualification where that distinction matters to external consumers.

`Stored` always contains the complete post-mutation record, never a patch. `Deleted` contains only the indexed identity.

### Tribute events

Add to `ITribute.sol`:

```solidity
event TributeBodyStored(
    uint256 indexed tokenId,
    address owner,
    uint32 worldwideDay,
    uint256 issuanceAmountMinor,
    uint16 issuanceCurrency,
    uint256 nominalAmountMinor,
    uint16 referenceCurrency,
    uint256 tributePriceMinor,
    bool excludeFromIntexIssuance
);

event TributeBodyDeleted(uint256 indexed tokenId);
```

The `TributeBodyStored` fields correspond one-for-one and in the same order to `TributeData` after the indexed `token_id`:

```text
token_id
owner
worldwide_day
issuance_amount_minor
issuance_currency
nominal_amount_minor
reference_currency
tribute_price_minor
exclude_from_intex_issuance
```

`TributeBodyDeleted` contains only `token_id`. ADR-004 will load the current projected body before repository deletion so owner/day indexes remain hidden behind `TributeRepositoryWriter::delete(token_id)`.

### Nod item events

Add to `INod.sol`:

```solidity
event NodBodyStored(
    uint256 indexed nodId,
    address owner,
    uint256 gratisLoadMinor,
    uint32 worldwideDay,
    uint16 leagueId,
    uint256 floorPriceMinor,
    bytes32 bucketKey,
    uint256 costAmountMinor,
    uint16 issuanceCurrency,
    uint16 referenceCurrency,
    uint64 issuedAt
);

event NodBodyDeleted(uint256 indexed nodId);
```

The `NodBodyStored` fields correspond one-for-one and in the same order to `NodItemState` after the indexed `nod_id`:

```text
nod_id
owner
gratis_load_minor
worldwide_day
league_id
floor_price_minor
bucket_key
cost_amount_minor
issuance_currency
reference_currency
issued_at
```

`NodBodyDeleted` contains only `nod_id`. The future projector delegates index cleanup to `NodRepositoryWriter::delete_nod(nod_id)`.

Nod item projection events are emitted by the Nod state owner at `NOD_ADDRESS`, even when the business operation originates in NodFactory. `NodIssued` and `NodBurned` remain emitted at `NOD_FACTORY_ADDRESS` as product events.

### Nod bucket events

Add to `INod.sol`:

```solidity
event NodBucketBodyStored(
    bytes32 indexed bucketKey,
    uint32 worldwideDay,
    uint256 floorPriceMinor,
    bool isQualified,
    uint64 totalNods,
    uint256 entryPriceMinor
);

event NodBucketBodyDeleted(bytes32 indexed bucketKey);
```

The `NodBucketBodyStored` fields correspond one-for-one and in the same order to `NodBucketState` after the indexed `bucket_key`:

```text
bucket_key
worldwide_day
floor_price_minor
is_qualified
total_nods
entry_price_minor
```

A bucket `Stored` event is emitted after:

- creation with its first Nod;
- increment when another Nod joins the bucket;
- decrement when a Nod is mined but the bucket remains non-empty;
- qualification from unqualified to qualified.

A bucket `Deleted` event is emitted when mining the final Nod removes the bucket.

### One event per changed off-chain record

Emit exactly one projection event for every record whose final value changes.

```text
operation             projection records
--------------------  -----------------------------------------------
Tribute issue         TributeBodyStored
Tribute burn          TributeBodyDeleted
Nod issue             NodBodyStored + NodBucketBodyStored
Nod mine              NodBodyDeleted + NodBucketBodyStored
Nod mine final item   NodBodyDeleted + NodBucketBodyDeleted
Bucket qualification  NodBucketBodyStored
```

A repeated no-op that does not change the record emits no projection event.

A mutation of EVM-only control state does not create a body event. For example, changing Tribute day seal state remains represented by `TributeWorldwideDaySealed` but does not emit `TributeBodyStored` because no `TributeData` record changed.

### Emission order

Complete all EVM state writes first. Then emit projection events in a fixed order, followed by existing product events.

```text
Tribute issue:
  state writes
  -> TributeBodyStored
  -> TributeIssued

Tribute burn:
  state writes
  -> TributeBodyDeleted
  -> TributeBurned

Nod issue:
  state writes
  -> NodBodyStored
  -> NodBucketBodyStored
  -> NodIssued

Nod mine:
  state writes
  -> NodBodyDeleted
  -> NodBucketBodyStored | NodBucketBodyDeleted
  -> NodBurned

Bucket qualification:
  state writes
  -> NodBucketBodyStored
  -> NodBucketQualified
```

For Nod issue and mining, item projection precedes bucket projection. This order is part of the receipt contract. ADR-004 will apply all projection mutations from one successful EVM receipt atomically while allowing different receipts in the same block to materialize sequentially.

If any later step in the operation fails, existing EVM journaling reverts state writes and all projection/product logs together. A failed transaction never leaves a canonical projection event.

### Receipt-visible lifecycle events

Nod qualification runs through the existing receipt-visible `HookEvents` system transaction.

`NodBucketBodyStored` emitted by `NodLifecycle` must therefore appear in the normal `HookEvents` transaction receipt and in normal block/log order. ExEx will process system-transaction receipts together with user-transaction receipts.

Do not add a raw hook path that mutates an off-chain record without a receipt. A lifecycle mutation and its projection event share the same EVM journal and success/failure outcome.

### Close non-event production bypasses

`NodContract::set_qualified` is currently used only by tests. It can modify `NodBucketState` without the canonical production qualification path.

Make it test-only or replace its use with a test helper. Production code must not retain a method that changes a future off-chain record without emitting the corresponding projection event.

After this change, production bucket mutations occur only through paths that emit complete projection events:

- Nod add;
- Nod remove;
- bucket qualification.

Direct storage manipulation in isolated tests is not a production event source.

### ABI fields, not Postcard bytes

Projection events carry explicit ABI fields, not opaque Postcard bytes.

ADR-004 will:

1. ABI-decode an event;
2. construct the existing Rust record type;
3. validate event identity against the record;
4. delegate Postcard encoding and index maintenance to the ADR-002 repository writer.

This keeps receipt data understandable to ordinary EVM tooling and prevents Postcard from becoming the public event ABI.

The event-to-record mapping must be total and checked. Narrow integer fields use their exact Solidity widths (`uint16`, `uint32`, `uint64`) so ExEx does not silently narrow `U256` values.

### No version or registry fields yet

Do not add:

- `schemaVersion`;
- `eventVersion`;
- `hashVersion`;
- `commitmentSchemeVersion`;
- `domainId`.

The Solidity event signature identifies the current format. There is no production history requiring multiple simultaneous event decoders. If the event shape changes before production, the testnet and MongoDB projection may be reset.

Canonical schema/hash versions are introduced with commitments in ADR-006. A domain registry is introduced only when a real later domain or runtime version requires it.

## Working result

After implementation:

- every successful Tribute issue/burn receipt contains the corresponding complete Stored or identity-only Deleted event;
- every successful Nod issue/mine receipt contains the complete Nod item and final bucket projection events;
- bucket qualification produces a complete bucket event in a receipt-visible `HookEvents` system transaction;
- ABI decoding reconstructs a `TributeData`, `NodItemState`, or `NodBucketState` equal to the actual post-mutation EVM record;
- event emitter addresses, event order, and field widths are stable and tested;
- failed or reverted execution produces no surviving projection event;
- existing product events and active EVM reads continue to work.

The receipts now contain enough information for ADR-004 to rebuild all six repository collections without re-executing domain business logic.

## Accepted limitations

- No ExEx consumer exists yet.
- MongoDB is not populated from receipts.
- Live runtime still reads EVM records.
- Projection events duplicate some fields already present in product events.
- Event bodies are explicit ABI tuples, while repository bodies use Postcard.
- Event formats are not versioned.
- There is no generic emitter or domain registry.
- There are no body commitments, leaf values, SMT roots, proofs, or authenticated reads.
- Receipt size and gas use are not final production capacity claims.
- Repository body/index writes are still non-atomic until ADR-004.

## Consequences

### Positive

- Canonical receipts become sufficient to reconstruct every future off-chain record.
- ExEx does not need to inspect calldata, rerun Lysis, or reproduce Nod business logic.
- Product event compatibility is preserved.
- Projection ownership follows state ownership rather than the initiating factory.
- Bucket creation, count changes, qualification, and deletion share one complete projection model.
- Hook-driven mutations become replayable through normal receipts.
- Explicit ABI fields remain inspectable by standard EVM tooling.

### Negative

- Operations emit additional logs and duplicate some product-event data.
- Nod issue and mining now emit projection logs from both Nod and NodFactory addresses.
- Every field added to an off-chain record must also be added to its projection event before production compatibility exists.
- Explicit event shapes require manual Rust/event field mapping and tests.
- The absence of versions means format changes require a coordinated testnet/projection reset.

## Alternatives considered

### Generic compressed-entity emitter

Deferred because it would require domain IDs, generic lifecycle rules, identity derivation, and registry decisions before their ADRs. Domain-specific events are sufficient for Tribute and Nod.

### Reuse or expand existing product events

Rejected because existing events have different ownership and intent, omit fields, and do not cover all bucket mutations. Changing their signatures would also break existing consumers unnecessarily.

### Raw Postcard bytes in event data

Rejected because it would make the Rust repository encoding part of the public EVM receipt ABI. Explicit fields are transparent to standard tooling, and ADR-004 can construct the typed record before repository encoding.

### Distinguish create and update in projection events

Rejected because current-state materialization uses identical upsert behavior for both. Product events retain the business distinction.

### Include old body data in delete events

Rejected because ADR-002 writers own index cleanup and already load the current primary body by identity. Delete events remain small and identity-only.

### Include schema/domain/version fields now

Rejected because there is no production history, multiple active schema, or runtime registry. Event signatures and a testnet reset are sufficient at this stage.

### Keep production `set_qualified` without an event

Rejected because any production mutation bypass would make receipt replay incomplete.

### Emit lifecycle logs outside a system-transaction receipt

Rejected because ExEx cannot use non-receipt logs as canonical block mutation data. Existing `HookEvents` provides the required receipt-visible path.

## Verification

### ABI and record reconstruction

For all three Stored events:

- encode the Rust-generated event;
- decode through the canonical Solidity interface;
- reconstruct the existing Rust record;
- assert equality for every field;
- cover minimum and maximum integer widths and both boolean values;
- prove that indexed identity equals the reconstructed record identity.

For Deleted events, verify exact topic, emitter, and identity decoding.

### Tribute execution

Test:

- issue emits `TributeBodyStored` then `TributeIssued`;
- Stored fields equal the persisted `TributeData`;
- burn emits `TributeBodyDeleted` then `TributeBurned`;
- failed issue, duplicate issue, failed burn, nested revert, and out-of-gas leave no projection event;
- day seal changes do not emit Tribute body events.

### Nod issue and mining

Test:

- first Nod issue emits item Stored, created bucket Stored, then product Issued;
- another Nod in the same bucket emits item Stored and bucket Stored with incremented `totalNods`;
- mining from a non-final bucket emits item Deleted and bucket Stored with decremented `totalNods`;
- mining the final item emits item Deleted and bucket Deleted;
- product events retain their existing emitter and shape;
- all reconstructed records equal the final EVM records;
- failed ownership, PoW, payment, or mint paths leave no projection event.

### Lifecycle qualification

Test:

- qualification emits bucket Stored with `isQualified = true` before `NodBucketQualified`;
- the event appears in the `HookEvents` system-transaction receipt;
- no-op/non-qualifying buckets produce no projection event;
- failed hook execution retains neither the bucket change nor its event;
- proposer and validator produce byte-identical receipt logs and ordering.

### Completeness guard

Add regression coverage proving every production write/delete of `TributeData`, `NodItemState`, and `NodBucketState` passes through one of the event-emitting paths. Test-only direct storage setup is excluded.

### Existing behavior

Run existing Tribute, Nod, NodFactory, Lysis, EVM executor, and product-event tests to prove that adding projection events does not change business state transitions or remove existing logs.

## Reset policy

Projection events change receipts and therefore block results. Activate ADR-003 through a coordinated hard fork or complete testnet restart.

No historical event migration or dual decoder is required. MongoDB may be empty or deleted because ADR-004 has not yet made it a chain-derived materialization.

If an event shape changes before production, reset the testnet/projection rather than adding premature version negotiation.

## Next unlocked step

ADR-004 can install a Reth ExEx that:

- filters the six projection event signatures by their owning emitter addresses;
- reads user and `HookEvents` system-transaction receipts in block/log order;
- reconstructs existing domain records from explicit ABI fields;
- applies Stored/Deleted events through the ADR-002 typed writers;
- updates all bodies and identity indexes emitted by one successful receipt atomically;
- records a durable finalized-block projection checkpoint for replay and restart.
