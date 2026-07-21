# ADR-C-NOD-001: Nod owns authenticated items, shared price buckets and qualification

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `crates/core/nod`, its compressed-entity bodies, qualification
  lifecycle and typed projection
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-C-LYS-001, ADR-C-TRB-001, ADR-S-ORC-001, ADR-B-CLI-001
- **Supersedes:** Nod-ledger assumptions previously embedded in Lysis documentation

## Context

A Nod is the authenticated, non-transferable output of Lysis and the consumable
input to Gratis mining. Many Nods may share a `(WorldwideDay, floor price)` bucket;
Oracle price movement qualifies that bucket once for every member. The ledger owns
items, buckets, membership counts, global supply and the bounded qualification
worklist. It does not own issuance economics, payment, PoW or Gratis minting.

## Decision

Nod is a read-only EVM token surface backed by compressed entities. Item identity is
the 36-byte Poseidon-derived id for `(owner, worldwide_day)`, limiting one live Nod
per owner/day. Bucket key is `keccak256(day || floor_price)` and its compressed-
entity id is the day prefix plus that digest. Bucket qualification is a monotonic
false-to-true latch driven at begin-block by canonical `COEN/0xUSD` Oracle state.

Lysis/NodFactory issue through `add_nod`; NodFactory consumes through verified item
and bucket capabilities. The block lifecycle is the sole normal qualification
authority. These commands must become typed authorities rather than remain broadly
callable Rust functions.

## State and invariants

An item stores owner, day, Gratis load, league, floor price, bucket key, cost,
issuance/reference currencies and block timestamp. A bucket stores day, floor,
qualification, member count and entry price. The compact consensus state stores
total supply, a three-level radix-256 bitmap of nonempty unqualified price
bins, per-bin dense bucket arrays and scan cursors, plus `bucket_key -> day`.

In every stable state:

- each item id equals `Poseidon(owner, day)` and each item points to exactly one
  bucket derived from its day/floor;
- each live bucket has `total_nods > 0`, and that count equals live items pointing
  to it; `total_supply` equals all live items;
- all items in a bucket agree on day and floor; bucket entry-price semantics are
  uniform and cannot silently depend on insertion order;
- every unqualified bucket occurs exactly once in the bin matching its floor, its
  day lookup is present, and the bitmap path/count/cursor agree;
- a qualified bucket occurs in no unqualified worklist and never becomes
  unqualified again;
- deleting the last item deletes its bucket and all compact index state.

The off-chain projection owns item primaries, owner indexes and bucket primaries.
Projection sessions load repository-owned prior state, validate embedded identities
and derive one atomic primary/index mutation batch. Mongo is rebuildable and never
authoritative for execution.

## Commands and lifecycle

```text
Absent item/bucket --issue--------------------> Live item + Unqualified bucket
Unqualified --Oracle rate strictly above floor> Qualified
Qualified item --NodFactory mine--------------> Absent item
Bucket with final item removed-----------------> Absent bucket
```

Issuance rejects an existing item, checks canonical item identity, increments
supply, mints the item, and creates or updates its bucket. A new bucket is inserted
into the logarithmic price-bin tree. Removal requires exact `VerifiedBody`
capabilities for both item and bucket, checks their relationship, decrements supply
and membership, deletes the item and updates/deletes the bucket.

The begin-block hook reads one Oracle rate, walks price bins in ascending order and
inspects at most 256 buckets. Bins strictly below the rate bin qualify wholesale;
the rate bin compares exact floors so equality remains unqualified. Dense arrays
use swap-remove, and a persisted per-bin cursor resumes partial work.

## Atomicity, replay and failure

`add_nod` and `remove_nod` each establish a storage checkpoint around compact
state, compressed bodies and events. Lysis supplies the larger atomic boundary for
Tribute consumption and all Nod issues. NodFactory supplies the larger boundary for
payment, Nod removal and Gratis/Fidelity effects. Qualification runs inside the
block lifecycle checkpoint and is monotonic, so replaying committed work observes
qualified buckets and cannot emit a second transition.

Missing/mismatched bodies, dangling bin entries, wrong payload domains, count
underflow/overflow and tree/count disagreement are corruption, not absence.
User-facing missing ids and index bounds revert. Parent backend unavailability and
deadlines retain typed local-read failure semantics.

## Determinism and bounds

Identity, bucket hashing, price-to-bin math, strict comparison, ascending tree walk,
dense-array mutation and the 256-inspection cap are consensus surfaces. Integer
math and shared constants must be version-pinned. Qualification work is bounded per
block, but full collection/owner reads accumulate every page into a `Vec`; ERC-721
enumeration currently performs those unbounded scans even for one index.

## Compatibility and production evidence

Item/bucket V1 codecs, 36-byte identity, Poseidon/Keccak recipes, storage slot
orders, bin step and price math, query keys, events and ABI are activation-controlled
formats. Changing any of them requires migration of compact worklists and
authenticated/projection bodies together.

Evidence inspected includes Nod schema/state/runtime/API/hooks/precompile,
repository/projection code and tests, Lysis and NodFactory callers, Oracle rate API,
compressed-entity rollback tests and projection event tests. Closure requires
production-interface tests for issue-to-qualification-to-mine, restart with partial
cursors, corrupt worklists, replay, projection rebuild and maximum-cardinality gas.

## module audit profile

The intended closed commands are `IssueNod`, `QualifyEligibleBuckets` and
`ConsumeQualifiedNod`. Each must own all affected item, bucket, supply and worklist
invariants or return a typed capability/receipt that makes the next owner prove
them. Raw record mutation and removal without lifecycle preconditions are outside
the intended interface.

## Consequences and rejected alternatives

Shared buckets make one Oracle comparison qualify many items while authenticated
bodies preserve exact membership. The compact bin tree bounds consensus work and
avoids scanning all Mongo/projected Nods. Per-item qualification was rejected as
duplicated state/work. Making NodFactory own bucket state was rejected because
qualification and item consistency belong to the ledger. Folding Nod and
NodFactory into one ADR was rejected because mining has separate external-call and
payment authority.

## Open questions and technical debt

- Close `add_nod`, `remove_nod`, `qualify_bucket` and test-oriented qualification
  seams behind typed Lysis/NodFactory/lifecycle authorities.
- `remove_nod` can delete the final unqualified bucket without removing its bin
  entry; production NodFactory checks qualification first, but the public Rust API
  does not. Enforce this invariant inside the ledger command.
- When adding to an existing bucket, validate its embedded key/day/floor and define
  whether the supplied `entry_price_minor` must equal the stored value. Today the
  first insertion silently determines entry price for later members.
- Validate nonzero owner, Gratis load, floor/entry prices, currencies, league and
  cost relationships at the deepest ledger boundary rather than relying only on
  orchestrators.
- Prove swap-remove plus scan-cursor behavior neither starves nor skips buckets
  under repeated tail-bin partial processing and concurrent same-block issuance.
- Bound `read_all`, `balanceOf`, `tokenByIndex` and `tokenOfOwnerByIndex`; fetching
  the complete collection for one indexed read is not a bounded ERC-721 surface.
- Define zero Oracle rate as explicit no-op/readiness behavior and distinguish
  missing price from a legitimate zero value.
- Reconcile error classification: wrong Nod payload domains are currently `Fatal`,
  while analogous authenticated-body errors elsewhere use body corruption.
- Prove supply, bucket counts, bin tree, cursors, authenticated bodies and Mongo
  indexes converge across rollback, restart, replay and reprojection.
- Add migration rules for bin constants/tree layout and item/bucket codecs; a
  partial migration can strand unqualified buckets permanently.
