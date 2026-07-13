# T36 — Gate: read-surface product decisions (port map)

Status: todo
Source: `audit_plan_final.md` B-03; formerly T27's approved pre-code port map (audit v5 P1-5 / owner Q4) —
lifted into a standalone gate so the product decision PRECEDES T30's list-RPC section and T26's
implementation (previously T27 owned the keep/move/remove decision while sitting DOWNSTREAM of both)
Depends on: T29
Blocks: T26, T27, T30

## Summary

Author and approve the legacy read-surface port map — the product/ABI decisions of the tribute/nod
cutover — before any surface it decides is frozen (T30 item 6) or implemented (T26, T27).

## Contents (decision artifact, no production code)

- (a)/(b)/(c) classification of every legacy view — `ownerOf`/`tokenURI`/`by-owner`/`by-day`/`by-WWD`/
  `getDayTotals`/`supply` and the Nod equivalents: (a) keep as a precompile view backed by bounded
  domain-owned consensus state the domain's rules genuinely require (§11.2, e.g. day totals feeding
  emission), re-derived from the new write path; (b) move to the RPC/CLI surface (T18/T19/T26) with an
  ABI breaking-change note; (c) remove. Nothing keeps the legacy schema alive.
- Which aggregates remain consensus state and for what CONCRETE runtime rule (§11.2 boundary decided per
  domain, not left implicit). Aggregate AUTHORITY is T35 (postfix PF-B02): every (a)-classified
  consensus-state view here cites the T35 aggregate-contract row it relies on; this artifact never
  redefines an aggregate — a conflict is resolved by revising T35 first.
- The list-RPC product surface feeding T30 item 6: WHICH by-owner/by-day/by-WWD methods exist for T26
  (method-name/DTO details remain T30's wire-spec deliverable; the surface SET is decided here).
- Hard read-context split restated: during consensus execution a precompile reads ONLY `read_commitment`
  and domain-owned EVM state — proof packages and the Mongo projection never back a precompile view.
- ERC721-surface consequences (enumeration/tokenURI semantics after the port) recorded per domain.

Artifact: `docs/ces-read-surface-port-map.md` (stable path — moved from T27; audit v5 P1-12).

## Acceptance criteria (gate-artifact completion — own deliverables ONLY)

1. Port map merged and approved: every legacy view/aggregate classified (a)/(b)/(c) with breaking-change
   notes; every remaining domain-owned aggregate justified by a concrete consensus rule.
2. T30 item 6 (list-RPC surface) and T26 derive their surface set from this artifact; T27 implements the
   approved classification without re-deciding.

## Invariants

- Product/ABI decisions live here; T26/T27/T30 implement and never unilaterally amend them.
