# T31 — Gate D2: conservative provisional resource bounds (pre-Q11)

Status: todo
Source: `audit_plan.md` §4 P1-0b, §8 Gate D2; concept §8.3/§15.1 (attempt cap alone does not bound a
single huge body or staged batch)
Depends on: T30 (schema bounds feed byte limits)
Blocks: T07 (runtime integration onward), T10 (provisional counters + estimator), T12 (cache bounds), T20 (recovery-store bounds), T24 (Part B re-baselines the values — audit-final L-01)

## Summary

Approve conservative provisional bounds for every resource the 50k-gas/600-attempt guard does not cover,
so runtime integration cannot start with unbounded inputs while Q11 remains open.

## Bounds to fix (provisional, superseded by Q11/T24 outputs)

- max body bytes per operation (per domain, from T30 schemas);
- aggregate body/calldata/event bytes per block;
- max unique keys per block;
- max staged-tree bytes;
- speculative cache count/bytes and eviction rule (deterministic node-local policy);
- exact maximum `K_domain` candidates for the benchmark;
- system-lane (10B gas) resource policy: how the bounds apply to receipt-visible system transactions;
- READ bounds (audit-final B-10): Lysis prefetch page bounds — max rows and max bytes per checkpoint-bound
  page (continuation-key pagination); point-read reservation — max body-dependent point reads and read
  bytes per tx/block. AGGREGATE bound (postfix PF-B01): the per-block Lysis prefetch total is bounded by
  the mutation-cursor budget — the prefetch fetches only the rows the block's cursor window will process
  plus a bounded lookahead constant, NEVER the whole WWD in one block; continuation across blocks is
  deterministic. T31 is the SINGLE numeric owner of every read value; T24 benchmarks and replaces each
  one (none stays provisional at Q11 closure).

## Wiring — enforcement OWNERS per bound (audit v5 P0-4; resolves the former T10 contradiction)

| Bound | Owner / enforcement point |
| --- | --- |
| per-operation body bytes (per domain) | T07 store entry (values via T23/T30 registry) |
| aggregate body/calldata/event bytes | T10 (active provisional counter) |
| max unique keys per block | T10 (active provisional counter) |
| system-lane policy (10B lane) | T10 |
| staged-tree estimator implementation + Stage A/B reservation | T10 `resource.rs` (formula from this artifact) |
| max staged-tree bytes: seal-time `actual <= reserved` conformance | T12 |
| speculative cache count/bytes + eviction | T12 (node-local) |
| Lysis page rows/bytes; point-read count/bytes | T10 reservation seam; enforced by the T33 adapters BEFORE any Mongo I/O (audit-final B-10) |
| replacement of all values (and structure if evidence demands) | T24 Part B re-baseline |

- Normative reserve/failure MATRIX (audit v6 P0-3, completed per audit v7 P0-1 — part of this gate's
  artifact, consumed by T07/T10/T12 ACs). TWO-STAGE RESERVE (v7 5.1.3, decided):
  Stage A (before hashing): gas sufficiency → per-operation body size → per-tx/per-block attempt slots →
  CE-byte reservation; then derive the canonical identity/tree locator;
  Stage B (before journal/event): first-touch unique-key reservation → conservative staged-tree delta
  reservation. A Stage B block-capacity overflow rolls back the WHOLE speculative tx including the
  Stage A checkpoint (explicit checkpoint contract).
  BYTE CLASSIFICATION (v7 5.1.1, decided — empty-block fit rule, no new per-tx constant):
  `tx_ce_bytes > MAX_CE_BYTES_PER_BLOCK` ⇒ `TransactionLimitExceeded` (can never fit an empty block);
  `tx_ce_bytes <= cap` but `> remaining_block_bytes` ⇒ sticky `BlockCapacityExhausted` (defer). The same
  fit classification applies to attempts, unique keys, and the staged-tree estimate.
  RETRYABLE vs PERMANENT (v7 5.1.2):
  | failure class | system behavior |
  | --- | --- |
  | remaining block capacity exhausted | cursor holds, retry next block |
  | operation/tx cannot fit an EMPTY block | deterministic producer/config error; a mandatory path fails the build; NO infinite retry |
  | malformed/oversized body | deterministic domain/core rejection; operator/code fix, no cursor spin; attempt NOT counted, charge NOT taken — Stage A precedes reservation (postfix PF-H03) |
  | local speculative-cache saturation | eviction/drop/rebuild or local liveness status; never consensus-invalid |
  Outcomes otherwise as before: per-operation body size ⇒ deterministic domain/core rejection (tx-local);
  per-tx overflow ⇒ `TransactionLimitExceeded`; remaining block attempts/bytes/unique-keys ⇒ sticky
  `BlockCapacityExhausted` (proposer defers, validator rejects an over-cap block); max staged-tree bytes ⇒
  PROTOCOL bound — a seal-time breach is a deterministic block failure signaling an estimator bug/corrupt
  proposal; speculative cache ⇒ NODE-LOCAL only. Reservation discipline: included-but-reverted work stays
  counted; excluded speculative tx restores the checkpoint; unique-key increments only on FIRST touch.
  `calldata bytes`/`event bytes` = full canonical encodings (event bytes include the body).
  STRUCTURE FREEZE (postfix PF-M08): the typed byte-accounting STRUCTURE — what counts as
  calldata/body/event bytes, the duplication rules, and the formula shape — is frozen by this gate
  BEFORE T10 consumes it; T24 replaces VALUES only, and a structural change is a formal re-baseline
  event that re-runs T10's metering suite.
  STAGED-TREE ESTIMATOR (v7 5.1.4; ownership relayered per v8 P0-1 — T12 ownership created a T07↔T12
  cycle): T31 owns the FORMULA/CONSTANTS only (per-shard insert path <= 256 nodes, collection-top
  log2(K_domain) levels, Root Catalog delta, retire vs mint/update/delete differences, safety margin);
  the pure `estimate_staged_delta(op_kind, first_key_touch, first_collection_touch, K_domain)`
  IMPLEMENTATION lives in T10's neutral `resource.rs` (with the Stage A/B counters); T07 supplies op-kind
  and first-touch metadata and calls the guard before any journal mutation; T12 owns only seal-time
  `actual <= reserved` conformance and the node-local speculative cache.
- CANONICAL UNIT (audit-final H-12; ownership corrected per postfix PF-M07 — the unit's definition
  cannot live in downstream T15): `max_staged_tree_bytes` is measured in the canonical SERIALIZED
  protocol footprint whose grammar is owned NORMATIVELY by the T30 wire spec; T15 IMPLEMENTS the grammar
  (golden vectors pin it), this artifact references it; the estimator, T12's seal-time conformance, and
  T16's commit all use this unit; process heap peak and MDBX write amplification are T24 REPORT metrics,
  never this bound.
- Recent-version recovery store bounds (audit v7 P1-12): max retained versions/rows/bytes, compaction
  batch bound, retention metrics/alarms, local disk-pressure behavior; silent early eviction below the
  release minimum window is forbidden.
- All T10-owned bounds enter through its limit-kind interface (no new machinery); enforcement parity
  proposer/validator per T10's discipline.
- Values documented as PROVISIONAL (`PROVISIONAL_Q11`) with the T24 replacement path.
- Requires concept §15.1 amendment #6 (temporary guard = attempts/gas + provisional D2 bounds).

## Acceptance criteria (gate-artifact completion — own deliverables ONLY)

1. Bounds table merged with rationale (conservative margins vs known hardware, not guesses presented as
   final); marked provisional with the T24 replacement path.
2. The bounds contract names the enforcing owner per bound (table above).
3. Artifact merged at `outbe-plan/ces-resource-bounds-provisional.md` (stable path, audit v5 P1-12).
(Enforcement implementation and tests are T07/T10/T12 acceptance criteria, not this gate's.)
