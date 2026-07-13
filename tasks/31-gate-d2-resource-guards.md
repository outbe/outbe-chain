# T31 — Gate D2: conservative provisional resource bounds (pre-Q11)

Status: todo
Source: `audit_plan.md` §4 P1-0b, §8 Gate D2; concept §8.3/§15.1 (attempt cap alone does not bound a
single huge body); owner decision 2026-07-13: staged batch is bounded constructively by attempt caps
Depends on: T30 (schema bounds feed byte limits)
Blocks: T07 (runtime integration onward), T10 (provisional counters), T12 (cache bounds), T24 (Part B re-baselines the values — audit-final L-01)

## Summary

Approve conservative provisional bounds for every resource the 50k-gas/600-attempt guard does not cover,
so runtime integration cannot start with unbounded inputs while Q11 remains open.

## Bounds to fix (provisional, superseded by Q11/T24 outputs)

- max body bytes per operation (per domain, from T30 schemas);
- aggregate body/calldata/event bytes per block;
- max unique keys per block;
- speculative cache count/bytes and eviction rule (deterministic node-local policy);
- exact maximum `K_domain` candidates for the benchmark;
- system-lane (10B gas) resource policy: how the bounds apply to receipt-visible system transactions;
(Read bounds and staged-tree byte limits were removed by owner decisions of 2026-07-13: body reads are
not resource-metered in Stage 1, and the staged batch is bounded CONSTRUCTIVELY by the attempt caps —
`attempts_cap × worst_per_op_delta`; T24 measures the worst-case batch size as a REPORT metric proving
the memory/2 s budget at the final attempt caps.)

## Wiring — enforcement OWNERS per bound (audit v5 P0-4; resolves the former T10 contradiction)

| Bound | Owner / enforcement point |
| --- | --- |
| per-operation body bytes (per domain) | T07 store entry (values via T23/T30 registry) |
| aggregate body/calldata/event bytes | T10 (active provisional counter) |
| max unique keys per block | T10 (active provisional counter) |
| system-lane policy (10B lane) | T10 |
| (staged-tree bytes: constructive bound via attempt caps — measured by T24, no protocol limit) | — |
| speculative cache count/bytes + eviction | T12 (node-local) |
| replacement of all values (and structure if evidence demands) | T24 Part B re-baseline |

- Normative reserve/failure MATRIX (audit v6 P0-3, completed per audit v7 P0-1 — part of this gate's
  artifact, consumed by T07/T10/T12 ACs). TWO-STAGE RESERVE (v7 5.1.3, decided):
  Stage A (before hashing): gas sufficiency → per-operation body size → per-tx/per-block attempt slots →
  CE-byte reservation; then derive the canonical identity/tree locator;
  Stage B (before journal/event): first-touch unique-key reservation. A Stage B block-capacity overflow
  rolls back the WHOLE speculative tx including the Stage A checkpoint (explicit checkpoint contract).
  BYTE CLASSIFICATION (v7 5.1.1, decided — empty-block fit rule, no new per-tx constant):
  `tx_ce_bytes > MAX_CE_BYTES_PER_BLOCK` ⇒ `TransactionLimitExceeded` (can never fit an empty block);
  `tx_ce_bytes <= cap` but `> remaining_block_bytes` ⇒ sticky `BlockCapacityExhausted` (defer). The same
  fit classification applies to attempts and unique keys.
  RETRYABLE vs PERMANENT (v7 5.1.2):
  | failure class | system behavior |
  | --- | --- |
  | remaining block capacity exhausted | cursor holds, retry next block |
  | operation/tx cannot fit an EMPTY block | deterministic producer/config error; a mandatory path fails the build; NO infinite retry |
  | malformed/oversized body | deterministic domain/core rejection; operator/code fix, no cursor spin; attempt NOT counted, charge NOT taken — Stage A precedes reservation (postfix PF-H03) |
  | local speculative-cache saturation | eviction/drop/rebuild or local liveness status; never consensus-invalid |
  Outcomes otherwise as before: per-operation body size ⇒ deterministic domain/core rejection (tx-local);
  per-tx overflow ⇒ `TransactionLimitExceeded`; remaining block attempts/bytes/unique-keys ⇒ sticky
  `BlockCapacityExhausted` (proposer defers, validator rejects an over-cap block); speculative cache ⇒
  NODE-LOCAL only. Reservation discipline: included-but-reverted work stays
  counted; excluded speculative tx restores the checkpoint; unique-key increments only on FIRST touch.
  `calldata bytes`/`event bytes` = full canonical encodings (event bytes include the body).
  STRUCTURE FREEZE (postfix PF-M08): the typed byte-accounting STRUCTURE — what counts as
  calldata/body/event bytes, the duplication rules, and the formula shape — is frozen by this gate
  BEFORE T10 consumes it; T24 replaces VALUES only, and a structural change is a formal re-baseline
  event that re-runs T10's metering suite.
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
