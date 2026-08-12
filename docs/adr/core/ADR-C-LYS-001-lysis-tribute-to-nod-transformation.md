# ADR-C-LYS-001: Lysis deterministically commits the Tribute-to-NOD transformation

- **Status:** Accepted
- **Date:** 2026-08-12
- **Owners:** `crates/core/lysis`, OCOMP result construction and verification
- **Depends on:** ADR-C-TRB-001, ADR-C-NOD-001, ADR-C-FID-001,
  ADR-S-OCM-002, ADR-S-OCM-003, ADR-S-OCM-004

## Context

Lysis transforms one sealed WorldwideDay of Tributes into a deterministic NOD
generation. The computation requires the authenticated Tribute population,
Fidelity leagues, frozen Oracle values, and the Metadosis budget. It is too large
for direct on-chain execution, but its result must remain independently
verifiable and canonically applicable.

## Decision

Lysis V1 is a typed OCOMP program. Given one finalized `JobIntentV1` and its
authenticated inputs, every validator and FullNode computes the same result
without writing chain state.

The program:

1. verifies the sealed Tribute partition, count, and nominal total;
2. reads Tribute bodies in canonical key order;
3. resolves the committed Fidelity and Oracle inputs;
4. computes deterministic fixed-point allocations within the frozen budget;
5. derives at most one `NodActionV1` for each owner and WorldwideDay;
6. derives contributor and Tribute-retirement effects;
7. emits bounded `ResultChunkV1` objects; and
8. returns constant-size roots, counts, totals, conservation commitments, and
   `unused_lysis` in `LysisResultV1`.

The NOD entry price and all other economics are fixed by this computation. They
are not read again during later materialization.

## Certification and activation

Validators vote for the complete `LysisResultV1`. The quorum-forming vote makes
that result canonical and atomically installs the certified generation metadata,
roots, scalar effects, Tribute retirement, contributor state, carry-over, and
Metadosis completion. On-chain activation does not iterate through all NOD
actions and does not rerun Lysis.

The certified `nod_root` is the authority for the ordered `NodActionV1` list.
Individual NOD ledger entries are then created through the bounded materializer
defined by ADR-C-NOD-002. Certification and materialization are therefore two
parts of one user-visible outcome:

```text
sealed Tributes
  -> deterministic Lysis result
  -> OCOMP quorum certification
  -> proof-backed NOD batches
  -> ordinary enumerable and mineable NODs
```

The generation is incomplete for mining until every certified NOD action has
been materialized. This completion gate does not change the canonical Lysis
result and does not introduce a replacement generation.

## Merkle contract

The ordered NOD action list uses the canonical OCOMP list hashing rules:

- `ListKind::NodActions` leaf, node, pad, and root hashes;
- global action ordinals;
- power-of-two padding;
- bounded result chunks;
- a shared upper path for a materialization batch.

The materializer rederives the NOD identity from owner and WorldwideDay and the
bucket identity from WorldwideDay and floor price. A proof can authorize only
the exact action at the exact ordinal under the certified root.

## Invariants

- The input partition is finalized, initialized, sealed, and consistent with
  its committed totals.
- The program is deterministic for the pinned intent and protocol semantics.
- There is at most one NOD action per owner per WorldwideDay.
- Allocation never exceeds the frozen Lysis budget.
- Result chunks, manifest, roots, counts, totals, and conservation equations
  describe the same ordered result.
- Quorum activation commits only constant-size certified state.
- Materialization never changes the certified action or its economics.
- A FullNode whose local Lysis result disagrees with the canonical quorum result
  stops rather than accepting inconsistent local data.

## Non-goals

This decision does not change the Lysis algorithm, work-shard size, ValidatorSet
membership, voting quorum, Worker protocol, or FullNode replay model. It does not
materialize all NODs inside the quorum-forming transaction and does not introduce
on-demand NOD creation.
