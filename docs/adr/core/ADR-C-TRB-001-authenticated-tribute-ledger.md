# ADR-C-TRB-001: Tribute owns the authenticated per-owner daily receipt ledger

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `crates/core/tribute`, its compressed-entity body and typed
  off-chain projection
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-C-MET-001, ADR-C-LYS-001, ADR-B-CLI-001
- **Supersedes:** The Tribute-ledger portions of the deleted pre-space TEE/Tribute aggregate

## Context

A Tribute is the authenticated economic receipt created after an encrypted offer
has been validated. The ledger must not also own decryption, TEE policy or offer
admission: those belong to TributeFactory and the enclave boundary. Tribute owns
the canonical body, its existence, daily aggregates, query indexes and terminal
removal. The multi-module creation and materialization sequence is specified by
PFS-001; the WorldwideDay transformation is specified by PFS-002.

## Decision

Tribute is a non-transferable, immutable-body ledger. Its canonical identity is
the Poseidon-derived 36-byte entity id for `(owner, worldwide_day)`, so an owner
can have at most one live Tribute in a day. Issuance and deletion use the generic
compressed-entity engine and retain a verified body capability for authenticated
mutation. The contract separately owns consensus-state `total_supply` and one
`DayTotals` record per day.

The public EVM precompile is read-only. TributeFactory is the normal issuance
authority; Metadosis owns opening, sealing and partition retirement; Lysis owns
the one bulk consumption transition. These authorities must eventually be
represented by closed typed capabilities rather than convention around public
Rust methods.

## Authoritative interface and production entrypoints

`ITribute` exposes metadata, supply, owner, balance, token metadata, day totals,
and owner/day identity queries. It rejects value and checks dynamic entity-id
length before decoding. It exposes no EVM mutation selector.

The production mutation paths are:

- `TributeFactory::offer_tribute` calls `TributeContract::issue` after enclave
  validation and canonical-identity checks;
- Metadosis calls `seal_day`, `unseal_day`, and
  `retire_completed_partition` as its WorldwideDay FSM advances;
- Lysis calls `consume_lysis_partition` only after producing exactly one Nod per
  verified Tribute;
- `burn`, `burn_loaded`, and `burn_all_by_wwd` are generic/internal maintenance
  seams and are not public ABI commands.

Reads require an active `ExecutionScope` and a `ParentBodySource`, because bodies
may come from the current block overlay or the authenticated finalized parent.
Repository readers and writers are an off-chain persistence boundary, not an
alternative consensus mutation API.

## State and invariants

The immutable canonical body contains entity id, owner, WorldwideDay, issuance
amount/currency, nominal amount/reference currency, Tribute price, and the Intex
exclusion flag. Owner must be nonzero, issuance amount must be positive, and the
id must equal the canonical derivation from owner and day.

For every stable ledger state:

- every live body has exactly one compressed-entity catalog entry and appears in
  exactly one owner index and one day index;
- `total_supply` equals the number of live Tribute bodies;
- an initialized day's `tribute_count` and `tribute_nominal_amount` equal the
  count and checked nominal sum of its live bodies;
- issuance is allowed only for an initialized, unsealed day and an absent id;
- a sealed day admits no new Tribute;
- a retired day has no Tribute partition and has zero count and nominal total.

The typed projection owns three namespaces: primary bodies, owner indexes and day
indexes. A projection session first loads repository-owned prior state, derives
all index changes from decoded canonical bodies, and emits one atomic write batch.
Malformed bodies, keys, cursors, dangling indexes and index/body disagreement are
corruption, never empty results.

## Lifecycle and commands

```text
Absent --issue while day open--------------------------> Live
Live   --authenticated burn----------------------------> Absent
Open day --Metadosis closes offering-------------------> Sealed
Sealed nonempty --Lysis verifies all and consumes------> Consumed
Consumed --Metadosis COMPLETED and partition retirement> Retired
```

`issue` validates identity and day state, increments day totals and supply, mints
the canonical body, and emits `TributeIssued`. `burn_loaded` accepts the exact
`VerifiedBody` capability returned by authenticated read, decrements totals and
supply, deletes that body and emits `TributeBurned`. Bulk burn iterates the day
query inside one checkpoint.

Lysis does not delete bodies individually. It proves its observed count and
nominal sum against a sealed `DayTotals`, decrements supply once, and zeroes the
day bucket. After Metadosis has committed `COMPLETED`, partition retirement removes
the authenticated catalog partition and emits `TributePartitionRetired`.

## Ordering, atomicity, replay and failure

Issue, loaded burn, bulk burn and retirement establish storage checkpoints.
Checked arithmetic, body mutation, aggregates and events therefore commit or
roll back together within their immediate command. The enclosing system hook must
provide the larger checkpoint for Lysis, Nod issuance, Metadosis completion and
partition retirement; no externally observable state may commit between those
steps.

Duplicate issuance reverts. Missing burn reverts. Identity mismatch and aggregate
underflow/overflow are fatal corruption. Parent storage unavailable/deadline
failures retain their typed execution classification. A replay after a completed
partition retirement must observe a terminal Metadosis day and no new mutation;
`RetirementOutcome::NotPresent` is otherwise only safe when zero totals are also
established.

## Determinism and resource bounds

Entity derivation, canonical encoding, checked arithmetic, binary index ordering
and page cursors are deterministic. Repository pages are capped by the shared
storage limit. Runtime owner/day convenience reads repeatedly fetch maximum-sized
pages into a single `Vec`, and bulk burn scales with all bodies in a day; gas and
hard cardinality bounds must close those loops before they are safe production
interfaces.

## Compatibility, trust and projection semantics

The 36-byte identity format, Poseidon derivation, canonical body V1 codec, storage
attribute ordering, query/index key layouts and event schemas are compatibility
surfaces. Changes require the format-evolution and protocol-activation policy,
not an in-place reinterpretation.

Consensus trusts the compressed-entity proof/root and finalized parent source,
not Mongo. Mongo is a rebuildable finalized projection. Event ingestion must use
the repository projection session so a store/delete updates primary and both
secondary indexes atomically and preserves provenance metadata only on primaries.

## module audit profile and closure evidence

The intended module boundary is one closed ledger authority with typed commands
`IssueValidatedTribute`, `ConsumeSealedDay`, `RetireCompletedDay`, and a narrowly
authorized repair/delete command. Structural closure requires making raw mutation
methods inaccessible to arbitrary modules, expressing the intermediate consumed
state, and proving aggregates from the same authenticated body set.

Evidence inspected includes `schema.rs`, `state.rs`, `runtime.rs`, `precompile.rs`,
`repository.rs`, `projection.rs`, Tribute unit/integration tests, TributeFactory,
Lysis and Metadosis production callers, and EVM checkpoint/event rollback tests.
Required production-interface evidence includes issue/replay, failed transaction
rollback, sparse post-delete queries, projection atomicity, parent corruption,
full-day Lysis retirement and restart/reprojection tests.

## Consequences and rejected alternatives

Separating Tribute from TributeFactory keeps encrypted admission policy outside the
receipt ledger and makes ownership auditable. Storing full bodies in ordinary EVM
maps was rejected in favor of authenticated compressed entities. Treating Mongo as
authoritative was rejected because validators must derive consensus state without
an external database. A single TEE/Tribute/Metadosis ADR was rejected because it
would hide distinct state owners and mutation authorities.

## Open questions and technical debt

- Close the public Rust mutation surface. Any crate with a storage handle can
  currently call `issue`, seal/unseal, consume, burn or retire without a typed
  TributeFactory, Metadosis or Lysis authority.
- Model the post-Lysis/pre-retirement interval explicitly or prove it cannot be
  observed outside one enclosing checkpoint. During it, bodies and owner/day
  query results still exist while `total_supply` and day totals are already zero.
- `retire_completed_partition` returns immediately on `NotPresent` without
  checking that the day totals are initialized, sealed and zero; define whether
  this is idempotency or masks corrupt/misordered state.
- Restrict burn on sealed days or define the maintenance authority and audit
  receipt required to mutate a sealed partition.
- Validate the remaining economic fields. Nominal amount, Tribute price,
  currencies, reference currency and their cross-field relationships currently
  have no ledger-level constraints.
- Bound `read_all`, owner/day ABI queries and `burn_all_by_wwd` by gas and an
  explicit maximum day/owner cardinality; page-sized backend reads do not bound
  the accumulated result.
- Prove `total_supply`, day totals, compressed catalog, owner/day indexes and
  projection indexes converge after crash, restart, replay and reprojection.
- Define migration/activation rules for `TributeBodyV1`, storage attribute orders,
  Poseidon identity derivation and repository key layouts.
- Add a production-path test showing a user cannot reach any mutation through
  ABI dispatch and that only the intended orchestrators can construct commands.
- Define projection lag/read-consistency semantics: RPC callers must know whether
  a Mongo result is finalized, checkpointed and complete for the requested block.
