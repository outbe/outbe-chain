# ADR-C-NOD-002: NodFactory materializes certified NOD generations and orchestrates PayNote-discharged Gratis mining

- **Status:** Accepted; cost discharge remains subject to the technical debt below
- **Date:** 2026-08-12
- **Owners:** `crates/core/nod`, `crates/core/nodfactory`
- **Depends on:** ADR-C-LYS-001, ADR-C-NOD-001, ADR-C-GRT-002,
  ADR-C-FID-001, ADR-C-VLT-001, ADR-C-LBM-001

## Context

Lysis commits a complete generation of NOD actions through `nod_root`. A root is
not itself a usable NOD ledger: owners must be able to enumerate ordinary NODs
and read their data through the existing public ABI. Materializing an unbounded
generation in the quorum-forming transaction would make block execution
unbounded, while accepting an unproved action would break the certified
generation authority.

NodFactory also owns the economically critical boundaries around NOD issuance,
third-party settlement, and owner-authorized Gratis mining. The NOD module owns
the authenticated ledger; NodFactory coordinates the bounded cross-module
effects.

## Certified generation materialization

NodFactory materializes each certified generation in FIFO order through bounded,
proof-backed batches. The current genesis profile uses a subtree height of three,
which yields eight NODs per full batch. Capacity is always derived from the
resolved profile and is not duplicated as a consumer constant.

The first certified generation for a WorldwideDay atomically installs:

- the certified roots, counts, totals, `job_id`, and `program_semantics_hash`;
- `next_nod_ordinal = 0` and the activation height;
- one FIFO entry for the WorldwideDay.

An exact installation replay is an idempotent no-op while the generation is
pending. Metadosis terminal state is the permanent authority that rejects a
repeated or different result after completion. Replacement and delta generations
do not exist.

The FIFO starts at `head_sequence = tail_sequence = 1` in genesis. Sequence zero
is invalid. Missing queue entries, invalid bounds, projection mismatches, and
counter overflow are canonical storage corruption and therefore fatal.

### Materialization transaction

`materializeCertifiedNods(bytes canonicalBatch)` accepts a canonical
`NodMaterializationBatchV1` containing:

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
7. cursor advancement and one progress event, or, for the final batch, FIFO
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
envelopes are invalid for inclusion. Canonical storage corruption remains fatal.

The pending projection consists of the generation selector, roots, packed
counts and issuance metadata, totals, job and program bindings, cursor, and
last-progress height. The final valid batch clears every one of those fields in
the same checkpoint that issues the remaining NODs and advances the FIFO head.
No NOD-module terminal marker is retained. Ordinary NOD bodies, owner and global
indexes, buckets, and supply are the terminal operational state.

Metadosis retains `ActiveGenerationV1`, the terminal job, completed binding,
quorum, receipt, and canonical events. Those records are the historical result,
replay, and conflict authority after the NOD projection has been cleared.

### Authorization, liveness, and restart

The proposer is only a local, nonblocking wake source; it is not execution
authority. Any current ACTIVE validator's OCOMP delegate may submit the first
valid batch, and deterministic execution verifies it on every node. Duplicate or
delayed wakes are harmless because the canonical FIFO cursor is authoritative.

`materializationHead()` returns `exists` plus canonical
`NodMaterializationHeadV1` bytes while work is pending. `certifiedGeneration`
is also a pending-materialization view: after completion both views report
absence. Historical certification remains available from Metadosis.

Supervisor transaction journals and artifact references are durable local
state. Startup and finalized-block reconciliation release a reference when the
corresponding transaction is already finalized, including a crash after
finalization but before the submitting thread performed its local release.

## Ordinary NOD issuance and mining

`issue_nod` is a system-only typed command intended for Lysis materialization.
It validates owner and uniqueness, derives the canonical NOD and currency-bound
bucket identities, stamps canonical block time, delegates authenticated ledger
mutation to ADR-C-NOD-001, and emits `NodIssued`.

`INodFactory.mineGratis` is the only user ABI command. It rejects value and
requires an exact 36-byte NOD id.

A NOD's cost is discharged by spending a PayNote, not by a transparent transfer,
and there is no separate settlement command or persisted settled flag. The
underlying assets reach the reserve vault when the note is deposited through
`IPayNote.deposit`, which routes them under `StablesSource::PayNoteDeposit`.
What `mineGratis` verifies is the proof obligation: `payNoteProof` must name the
caller as its spender, carry one of the assets VaultRouter registers under
`referenceCurrencyAssets` for the NOD's `reference_currency`, and cover the
NOD's cost. `NodPaid` names the spent nullifier rather than a payer, which is
the property the scheme exists for.

The cost is not stored on the NOD. It is derived on demand as
`floor(bucket.entry_price_minor * gratis_load_minor / 1e6)` — the same formula
Lysis mints the NOD from — so there is one definition and nothing to keep in
sync. Lysis rejects a zero cost at issuance, so every NOD has a note to burn.

Binding the proof's spender to the caller is what stops a third party from
lifting an observed proof to pay for their own NOD; notes are otherwise bearer
instruments and anyone may relay them.

`mineGratis` requires caller ownership, valid bounded PoW, a qualified bucket,
a covering PayNote spend, and no incomplete certified generation for that
WorldwideDay. It moves no value, consumes the NOD, emits `NodBurned`, and mints
exactly the recorded `gratis_load_minor` through Gratisfactory, including the
Fidelity cohort update. NOD deletion is the mining replay guard; the PayNote
nullifier is the payment's.

Materialized entries are ordinary NOD ledger entries. Supply, ownership,
enumeration, metadata, bucket, and mining behavior therefore use the existing
ABI and authenticated storage. Current materialization acceptance proves
`tokenOfOwnerByIndex` and `nodData` through the real OCOMP path; mining is outside
that acceptance lane.

## Atomicity and determinism

The outer EVM transaction journal is the rollback domain. A failure in NOD
issuance or removal, PayNote verification, nullifier booking, event emission,
Gratis mint, or Fidelity mutation reverts all preceding effects in the same
command. PayNote books its nullifier before `mineGratis` checks the claim, so
the shared checkpoint is what keeps a rejected mine from destroying the note for
nothing. Verification runs after the cheap ownership, PoW, and qualification
gates, so a doomed mine never pays for proof work.

NOD identity and bucket identity are derived rather than caller-selected.
Issuance economics come from certified Lysis output and are transported without
a second formula. Block timestamp, currency registry, and asset mapping come
from canonical execution inputs. PoW uses the shared `outbe_common::pow` scheme
over the exact encoded NOD id and a big-endian `u64` nonce.

## Consequences

- Quorum certification remains constant-size and atomic.
- NOD creation is bounded per transaction and restart-safe.
- All nodes converge through ordinary transaction execution.
- Completed generations leave no duplicate per-WWD materialization state.
- Materialization may span arbitrarily many batches without a second deadline
  state machine.
- Cost discharge and mining remain separate from the authenticated NOD ledger
  while committing their effects atomically.
- Scaling to extremely large generations requires a separate design; it is not
  hidden behind the V1 interface.

## Open questions and technical debt

- A spend that covers more than the NOD's cost is accepted and the excess is
  not refunded. The circuit can produce exact change, so the spender controls
  this; whether the runtime should instead require equality is open.
- VaultRouter share results and exact received value need an explicit receipt if
  economic conservation depends on them. The PayNote deposit path, not
  NodFactory, is now where that evidence must come from.
- `issue_nod` remains a conventional Rust capability. A future revision may bind
  it to an unforgeable Lysis receipt without adding a public issuance selector.
- Reentrancy, rollback, and nonzero-cost discharge require tests against real
  ERC20 and vault implementations end to end, through a real PayNote deposit.
- PoW difficulty and preimage versioning must state whether existing NODs retain
  issuance-era rules across a protocol update.
