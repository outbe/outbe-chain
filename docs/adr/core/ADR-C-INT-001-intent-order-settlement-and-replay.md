# ADR-C-INT-001: Intent orders settle once across origin and destination authorities

- **Status:** Proposed; current Solidity implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `contracts/intent` Origin/Destination settlers and Router
- **Depends on:** ADR-B-XCH-001, ADR-C-INT-002, ADR-C-INT-003

## Context

The ERC-7683 intent router locks origin input in The Compact, selects/fills on a
destination, and dispatches settlement or refund back to the origin. Same-chain paths
call locally while cross-chain paths use ERC-7786 transport.

## Decision

Canonical `OrderData` and its domain-separated hash are the sole order identity. Origin
nonce is consumed atomically with opening and Compact allocation. Destination status is
an explicit FSM:

```text
Unknown -> Claimed -> Filled -> SettlementDispatched
Unknown ---------------------> RefundDispatched
Claimed --expiry/slash-------> RefundDispatched
```

Origin independently records `Open -> Settled | Refunded` and validates message source,
order hash, original sender/recipient/token/amount/domains/deadline and expected remote
router before allocated transfer. Same-chain dispatch has identical guards/effects and
cannot bypass replay state.

## Authoritative interfaces

Origin `open`, `resolve`, `invalidateNonces`; destination `claimOrder`, `fill`, `settle`,
`refund`; Router peer configuration, receive and dispatch are the closed commands.

## Invariants

- One `(sender,nonce)` opens at most one order and one order reaches one terminal origin
  disposition.
- Settlement releases origin input only to the authenticated winning solver after valid
  destination fill; refund returns it only to the original owner.
- Output delivered is at least the committed floor and matches token/recipient/domain.
- Same-chain and cross-chain paths preserve identical value/replay semantics.

## Atomicity, replay and failure

Open plus Compact lock and nonce use are atomic. Destination fill plus status/collateral
effect is atomic. Cross-chain dispatch is a durable outbox; origin inbox plus transfer is
atomic. Duplicate terminal messages are harmless explicit outcomes, not second transfers.

## Determinism and bounds

Order bytes, filler data, batch settlement/refund size and message gas are bounded.
Deadlines define equality precisely. Hash encoding rejects ambiguous/trailing forms.

## Compatibility, trust and activation

Order type hash, codec, status enum, Compact lock tag, router peers, local domain and
transport version are one activation profile.

## Production-interface verification evidence

Inspected order codec/validator, origin and destination bases/implementations, local and
cross-chain dispatch, nonce/status storage and Router receive. Foundry E2E tests exist,
but deployment-shaped two-chain replay/failure evidence is incomplete.

## Consequences

Settlement correctness is independent of transport delivery count and solver services.
It requires explicit origin and destination terminal records.

## Rejected alternatives

- Event-only order status is rejected.
- Trusting an order id without revalidating encoded fields is rejected.
- Treating dispatch attempt as settlement completion is rejected.

## Open questions and technical debt

- **Critical:** prove origin settlement/refund has durable per-order terminal replay state
  before Compact transfer; transport authentication alone is insufficient.
- Model `SettlementDispatched`/`RefundDispatched` explicitly; current destination statuses
  may permit repeated dispatch attempts without a durable outbox identity.
- Prove nonce invalidation/open ordering and order hash domain separation across chains,
  router deployments and versions.
- Bound batch and arbitrary `data/fillerData`; add malformed codec and gas-limit tests.
- Add two-chain failure injection for duplicate/reordered terminal messages, Compact
  failure, route rotation, same-chain parity and retry.

