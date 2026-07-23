# ADR-S-OCM-002: OCOMP input is a finalized authenticated export, not a trusted database snapshot

- **Status:** Proposed; PoC not implemented
- **Date:** 2026-07-23
- **Decision owners:** System Space, compressed-entity and persistence maintainers
- **Scope:** finalized input identity, retention pins, snapshot export, raw-body
  transport, CAS manifests and worker read integrity
- **Depends on:** ADR-S-OCM-001, ADR-B-CNS-001, ADR-B-CNS-003,
  ADR-B-OCD-004 through ADR-B-OCD-015, ADR-B-CAP-001, ADR-C-TRB-001
- **Related:** ADR-S-OCM-003, ADR-C-FID-001, ADR-S-ORC-001, PFS-001,
  PFS-002
- **Supersedes:** None

## Context

Raw Tribute bodies currently travel through the Mongo projection, while their
canonical commitments, collection roots and sealed CE root are anchored in
finalized chain state. Mongo, a filesystem and a CAS can lose, omit, reorder or
mutate bytes. Treating any of them as input authority would let local storage
behavior change a consensus-relevant computation.

Workers also cannot scan a live node database through RPC: bulk transfer would
couple consensus resources to computation and would not prove that the exported
set is complete.

## Decision

An OCOMP job input is identified by its finalized request block and the exact
roots/scalars pinned by `JobIntentV1`. The authority chain is:

```text
finalized block/state root
  -> sealed CE root
  -> WWD collection root and authenticated count/totals
  -> ordered body commitments
  -> canonical Tribute bodies
  + pinned Fidelity and Oracle openings
```

MongoDB or another body store transports canonical body bytes but is never
authority. The exporter accepts a body only after canonical decoding, identity
checking and commitment verification. Completeness comes from full traversal of
the authenticated WWD collection and reconciliation of root, count and nominal
totals, not from a Mongo page count.

### Pin and export lifecycle

Each validator domain maintains:

```text
TENTATIVE(IntentId, candidate block/state root)
  -> FINALIZED(JobId, deadline)
  -> EXPORTED(InputManifestHash)
  -> RELEASED after terminal finality and retention policy

TENTATIVE -> RELEASED when the candidate block is orphaned
```

Before a validator votes for a block containing an intent, it durably records
the tentative retention pin. Semantic computation and signing wait for finality.
An orphaned request invalidates every local artifact derived from it.

The node performs an O(1) handoff of a bounded opaque read-only checkpoint/lease
capability to the exporter UID. It does not expose a live MDBX/Reth writer,
accept a caller-selected filesystem path or stream bulk bodies through
`OcompControlV1`.

### Authenticated input bundle

The exporter full-folds the PoC collection in canonical order and writes
immutable content-addressed chunks plus a canonical manifest:

```text
InputManifestV1 {
  JobId,
  finalized checkpoint identity,
  protocol bundle,
  ordered chunk digests and lengths,
  authenticated range/count/nominal commitments,
  Fidelity/Oracle opening commitments,
  manifest codec and caps
}
```

The exact byte schema is frozen with the PoC bundle before implementation.
Object paths and database locations are not part of semantic identity.

CAS objects are written to a temporary private object, hashed, durably published
under their digest and never modified in place. Security does not rely on
filesystem immutability: a worker hashes the exact byte stream it consumes and
checks the expected digest, canonical length and manifest membership. A
separate “check then reopen” is insufficient because it creates a
time-of-check/time-of-use gap.

## Authoritative interfaces

| Responsibility | Authority |
|---|---|
| job/input identity | finalized `JobIntentV1` and finality proof |
| complete Tribute enumeration | authenticated CE collection traversal |
| raw body transport | Mongo/body store, explicitly untrusted |
| immutable chain/opening view | retained finalized checkpoint capability |
| exported object identity | canonical digest and manifest membership |
| bulk consumption | job-scoped CAS capability |
| retention release | terminal/recovery policy, never worker cleanup |

## Invariants

- The event, Mongo query, filesystem path and CAS object name never establish
  job authority by themselves.
- Every consumed body matches its canonical identity and committed CE leaf.
- Traversal reconstructs the exact pinned collection root, count and nominal.
- Fidelity and Oracle values are bound to the job's logical evaluation context.
- A manifest names one `JobId` and one protocol bundle only.
- Workers consume only digest-verified manifest members.
- A fork-orphaned input is never signable.
- Export/pin ambiguity causes abstention, never best-effort live-state reads.

## Determinism and bounds

PoC uses a generated Tribute cap and one bounded full fold. Page sizes, chunk
sizes, total bytes, object counts, opening counts, checkpoint leases and disk
quota are protocol/profile bounded before allocation. Database enumeration order
is normalized to the program's canonical key order.

TargetLarge may stream counted ranges into shared object storage, but it retains
the same authority rule: a result is not signable until the complete authenticated
manifest is closed. TargetLarge mechanisms are not PoC evidence.

## Atomicity, replay and failure

Export is local and restartable by manifest/chunk digest. Duplicate publication
of identical bytes is idempotent. A corrupt, truncated, missing or changed
object is rejected and may be rebuilt from retained sources. If the source,
checkpoint or disk is unavailable, that validator abstains while consensus
continues.

The node releases the retention pin only after terminal finality and the
applicable evidence/recovery window. Worker or supervisor shutdown cannot release
it. An input export cannot mutate canonical chain, Mongo projection or CE state.

## Compatibility and migration

Body codec, commitment scheme, tree topology, manifest codec, opening schema and
caps are pinned by `ProtocolBundleV1`. New versions never reinterpret existing
objects. BoundedMVP may replace local filesystem CAS and one-entry pin journals
with production stores/recovery without changing the authenticated input
meaning.

## Production-interface verification evidence

Existing code provides Mongo `StorageReader`, typed body repositories, CE roots
and MDBX finalized snapshots, but no OCOMP retention handoff, exporter, manifest
or CAS capability exists. PoC evidence must corrupt Mongo bytes, omit a body,
change/reorder/truncate a CAS chunk, orphan the request block and restart export.
Every case must either reconstruct the exact input or produce no signature.

## Consequences

PoC intentionally performs an export/materialization pass before semantic Lysis
execution. This costs local I/O but removes the live Mongo/node database from the
computation trust and failure boundary. Future streaming optimizations preserve
the manifest/root closure rather than trusting partial work.

## Rejected alternatives

- **Workers query Mongo directly:** Mongo availability/order becomes semantic
  and every worker needs broad database authority.
- **Stream all bytes through node RPC:** consensus resources and bulk compute
  failure become coupled.
- **Trust a filesystem snapshot:** local mutation/omission remains undetected.
- **Use Mongo count as completeness:** projection state is transport, not the
  authenticated collection.
- **Hash once and later reopen the file:** mutation between verification and use
  is not detected.

## Open questions and technical debt

1. Select and prove the real PoC checkpoint/handoff primitive across Reth state,
   CE MDBX and raw body retention.
2. Define the exact relation between Mongo projection checkpoint and the retained
   finalized request block without treating Mongo as authority.
3. Freeze `InputManifestV1`, chunk codec, caps and golden corruption vectors.
4. Define crash-safe one-entry PoC pin persistence and orphan release ordering.
5. Decide whether Fidelity/Oracle openings are exported from the same checkpoint
   capability or a second authenticated source with an exact binding.
6. Specify secure file-descriptor/no-follow handling in addition to digest
   verification to reduce local attack surface.
