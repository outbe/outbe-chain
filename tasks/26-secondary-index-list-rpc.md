# T26 — Secondary-index list RPC over MongoDB

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §11.2 (Q20 context), §3.2
Depends on: T18, T20, T21 (leaf-check gate on served bodies), T30 (bounds + DTO contract), T36 (approved port map fixes the product surface — audit-final B-03)
Blocks: T27

## Summary

**Classification: product-surface preservation for the T27 cutover — integration support, NOT a spec
deliverable.** The storage spec does not require a list RPC (§11.2 is permissive; the §18 placement table
lists only proof/body RPC); this task exists because T27 removes the existing `tributes_by_owner/by_day`,
`nods_by_owner` product surfaces, and they must be reproducible from the projection (`gems_by_owner`
stays on legacy Gem storage — Gem deferred).
Scope is strictly the EXISTING surfaces: `by_owner` and `by_wwd`/`by_day` listings in the `outbe_*`
namespace, with per-record verifiability through optional point-proof attachment (the one list-related
guarantee §11.2 does state).

## Context

§11.2: secondary indexes are projection features, not core storage primitives; the list has no guarantee of
completeness, ordering, or freedom from omissions (authenticated completeness is an explicit non-goal).
Each returned record is individually verifiable when accompanied by its point-proof package. The current
product surface (e.g. `tributes_by_owner`, `tributes_by_day`, `nods_by_owner`) must be
reproducible from the projection once bodies leave per-record EVM storage.

## Scope

- RPC methods over the T20 Mongo indexes: list by owner / by wwd (Tribute partition) — the existing
  product surfaces only; no net-new generic `by_domain` listing (dropped as scope expansion). Pagination
  offset/limit per repo conventions; the count field is named `projected_total` — it counts projection
  rows, not a completeness claim.
- Exact RPC contract fixed BEFORE implementation (audit v5 P1-4, via T30): method names for the existing
  by-owner/by-day/by-WWD surfaces, request/response DTOs, the stable ordering key, pagination
  continuation/offset semantics, numeric status/error codes, the `with_proof` response shape, and
  max-response behavior. The SET of surfaces comes from the T36-approved port map (audit-final B-03) —
  T26 implements, never decides.
- Resource bounds (audit-v2 P1-7; values from T30): max `limit` per page, max response bytes, request
  timeout — enforced for plain list pages too, not only `with_proof`; on projection/proof checkpoint
  mismatch the response surfaces both checkpoints with a typed status instead of silently mixing heights.
- Response shape: canonical identity + canonical body bytes (or decoded projection row) + the projection
  checkpoint `{height, block_hash}` the row set was served from; explicit `unverified list` semantics in
  the response envelope.
- Optional `with_proof` flag: attach the T18 point-proof package per returned record (bounded page size when
  enabled; proofs assembled at `proof_ready_height` — height mismatch with the projection cursor is
  surfaced, not hidden).
- Served bodies pass the same per-key leaf check as point reads (T21 gate) before leaving the node; a row
  failing the check KEEPS its page position and is returned as a per-row `unavailable` marker (identity
  without body) — never silently skipped, never breaking pagination arithmetic.
- Stale-projection behavior: responses carry the Mongo high-water; no claim of completeness anywhere in the
  API contract or docs.

## Out of scope

- Authenticated/complete list queries (non-goal §1.2); new index kinds beyond the T20 set; domain-owned
  consensus indexes (outside the storage concept).

## Acceptance criteria

1. List queries return correct pages against a projector-populated Mongo (Docker harness), stable ordering
   rule documented per index.
2. `with_proof` pages verify record-by-record via the T18 verifier; mixed page with one corrupt row yields
   that row as `unavailable`, remaining rows intact.
3. Pagination conformance with existing repo conventions (offset/limit + total counts).
4. Response envelope carries projection checkpoint and unverified-list semantics; README documents the
   no-completeness contract.
5. Bounds enforcement (audit v3 P1-8): over-limit page request rejected; max response bytes enforced
   (page truncation is typed, not silent); timeout returns a typed status; projection/proof checkpoint
   mismatch surfaces both checkpoints and never mixes heights in one page.

## Invariants

- No list response implies completeness; Mongo remains a non-authority; every served body passed the leaf check.

## Tests

- RPC integration tests with Dockerized Mongo; corrupt/stale-row fixtures; pagination edge cases.

## Files

- `crates/core/compressed_entities/src/rpc.rs` (list methods)
- `bin/outbe-chain` RPC wiring; README RPC section
