# ADR-C-NOD-002: NodFactory materializes certified NOD generations and orchestrates Gratis mining

- **Status:** Accepted
- **Date:** 2026-08-12
- **Owners:** `crates/core/nod`, `crates/core/nodfactory`
- **Depends on:** ADR-C-LYS-001, ADR-C-NOD-001, ADR-C-GRT-002,
  ADR-C-FID-001, ADR-C-VLT-001

## Context

Lysis commits a complete generation of NOD actions through `nod_root`. A root is
not itself a usable NOD ledger: owners must be able to enumerate ordinary NODs,
read their data, and mine them through the existing public ABI. Materializing an
unbounded generation in the quorum-forming transaction would make block
execution unbounded, while allowing an unproved action would break the certified
generation authority.

## Decision

NodFactory materializes each certified generation in FIFO order through bounded,
proof-backed batches. The current genesis profile uses a subtree height of three,
which yields eight NODs per full batch. The capacity is always derived from the
resolved profile; it is not a protocol literal in consumers.

The first certified generation for a WorldwideDay atomically installs:

- the certified roots, counts, totals, `job_id`, and `program_semantics_hash`;
- `next_nod_ordinal = 0` and the activation height;
- one FIFO entry for the WorldwideDay.

An exact installation replay is an idempotent no-op while the generation is
pending materialization. Metadosis terminal state is the permanent authority
that rejects a repeated or different result after completion. Replacement and
delta generations do not exist.

The FIFO starts at `head_sequence = tail_sequence = 1` in genesis. Sequence zero
is invalid. Missing queue entries, invalid bounds, projection mismatches, and
counter overflow are canonical storage corruption and therefore fatal.

## Materialization transaction

`materializeCertifiedNods(bytes canonical_batch)` accepts
`NodMaterializationBatchV1`:

- FIFO sequence;
- first NOD ordinal;
- one bounded vector of `NodActionV1` values;
- the shared Merkle path above the batch subtree.

The handler performs, in order:

1. current ACTIVE OCOMP delegate authorization;
2. consumption of the genesis-bound per-block attempt allowance;
3. canonical decoding;
4. exact FIFO head, projection, cursor, and profile validation;
5. batch shape, ordinal, identity, bucket, and Merkle-root verification;
6. one outer checkpoint containing every `issue_nod` call;
7. cursor advancement and one progress event; or, for the final batch, FIFO
   dequeue and complete removal of the per-WWD pending projection.

`issue_nod` remains the only ledger issuance implementation. Each verified
action is converted to the existing `NodIssueParams`; the ledger timestamp is
the materialization block timestamp. Prices, amounts, currencies, league, and
the logical Lysis timestamp come only from the committed action. Materialization
does not read current Oracle state or recompute economics.

If any item fails, all NOD records, owner indexes, buckets, events, cursor,
projection, and queue changes roll back. Stale races, invalid proof or shape,
duplicate NODs, and excess same-block attempts return a typed failed receipt
while the block continues. Unauthorized signers and malformed system-carrier
envelopes are not valid for inclusion. Canonical storage corruption remains
fatal.

The pending projection consists of the generation selector, roots, packed
counts and issuance metadata, totals, job and program bindings, cursor, and
last-progress height. The final valid batch clears every one of those fields in
the same checkpoint that issues the remaining NODs and advances the FIFO head.
No NOD-module terminal marker is retained. Ordinary NOD bodies, owner and
global indexes, buckets, and supply are the terminal operational state.

Metadosis retains `ActiveGenerationV1`, the terminal job, completed binding,
quorum, receipt, and canonical events. Those records are the historical result,
replay, and conflict authority after the NOD projection has been cleared.

## Public NOD behavior

Materialized entries are ordinary NOD ledger entries. The complete existing NOD
ABI continues to apply, including supply, ownership, enumeration, approval,
metadata, bucket, and mining reads and mutations. Release acceptance proves
`tokenOfOwnerByIndex` and `nodData` on NODs produced by the real OCOMP path;
mining behavior is outside this materialization acceptance.

If a WorldwideDay has a certified but incomplete generation, `mineGratis`
returns `NodGenerationNotMaterialized`. Completion removes the pending
projection, after which the ordinary NOD path applies. Legacy NOD mining is
unchanged.

## Authorization and liveness

The proposer is only a local, nonblocking wake source. It is not execution
authority. Any current ACTIVE validator's OCOMP delegate may submit the first
valid batch, and ordinary deterministic execution verifies it on every node.
Duplicate or delayed wakes are harmless because the canonical FIFO cursor is the
authority.

The public view `materializationHead()` returns `exists` plus canonical
`NodMaterializationHeadV1` bytes while work is pending. `certifiedGeneration`
is likewise a pending-materialization view: after completion both views report
absence. Historical certification remains available from Metadosis.

Supervisor transaction journals and artifact references are durable local
state. Startup and finalized-block reconciliation must release a reference when
the corresponding transaction is already finalized, including a crash after
finalization but before the submitting thread performed its local release.

## Consequences

- Quorum certification remains constant-size and atomic.
- NOD creation is bounded per transaction and restart-safe.
- All nodes converge through ordinary transaction execution.
- Completed generations do not leave duplicate per-WWD materialization state.
- Materialization can take arbitrarily many batches without inventing a second
  generation state machine or deadline.
- Scaling to extremely large generations requires a separate design; it is not
  hidden behind this V1 interface.
