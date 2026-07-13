# T35 — Gate: body/generator/aggregate feasibility preflight (design-first)

Status: todo
Source: `audit_plan_final.md` B-04/B-07/B-09; formerly T23's design-first checkpoint (audit v5 P1-7,
audit v6 P0-2) — lifted into a standalone gate so feasibility PRECEDES the T30 schema/generator freeze
Depends on: T29
Blocks: T23, T30, T36 (approval order — aggregate authority)

## Summary

Approve the consensus-state design decisions the Tribute/Nod port needs BEFORE T30 freezes canonical body
schemas and generator algorithms (audit-final B-07: freezing schemas before proving every update flow can
construct a complete body — and every generator can guarantee lifetime uniqueness — is a late decision
that would force a post-freeze spec revision).

## Contents (design artifact, no production code)

- Body-source tables per mutating entrypoint — the feasibility go/no-go: core never reads an old body
  (§3.1) and has no patch operation, so every update must construct the COMPLETE new canonical body from
  `transaction inputs + domain-owned consensus state` (+ under Stage 1 Variant A, the VERIFIED body read —
  recorded per operation in the T33 matrix). Where a legacy flow relied on reading the stored record, the
  replacement source is named per flow.
- Generator persistent-state contracts, per domain: Nod — `keccak(nod||owner||wwd)` repeats after burn
  unless domain state keeps a lifetime tombstone/monotonic guard → define it; Tribute — enclave-produced
  ID needs a consensus-visible uniqueness contract. Each with collision domain, rollback semantics
  (counter/nonce writes share the mint journal), and the persistent non-reuse state named.
- WWD retirement aggregate contract: how supply/day/owner domain aggregates update on atomic WWD
  retirement WITHOUT O(N) per-entity deletes.
- Domain-owned aggregates are DECIDED HERE — single owner (postfix PF-B02): T36's port map REFERENCES this
  artifact's aggregate contract and never redefines it; a conflict is resolved by revising T35 first.
- Upstream self-sufficiency (postfix PF-B02): this gate PRECEDES T30 — its only inputs are the concept and
  the T29 profile; no T30 draft exists yet and none is consumed. T35 OUTPUTS the candidate registry rows
  (encoding kinds, partition policies, generator versions, body-size candidates) that T30 turns into the
  normative table; if feasibility fails, the schema/generator design changes HERE — never after the T30
  freeze. T30's (or any downstream task's) completion is not part of this gate.

Artifact: `docs/ces-body-source-matrix.md` (stable path — moved from T23; audit v5 P1-12).

## Acceptance criteria (gate-artifact completion — own deliverables ONLY)

1. Artifact merged and approved: every mutating Tribute/Nod entrypoint has a complete body-source row;
   each generator has a written lifetime-uniqueness contract naming its persistent non-reuse state;
   the retirement aggregate contract is specified. (The former `ActiveTributePartitionsView` deliverable
   is removed — scope re-cut 2026-07-13: no readiness/coverage consumer exists.)
2. T30's body-schema/generator sections and T23's implementation reference this artifact as their
   normative input (T23 implements it without re-deciding).

## Invariants

- Design-first: no storage layout or production code edits belong to this gate.
