# ADR-C-INX-007: Intex routers use authenticated inboxes and durable settlement outboxes

- **Status:** Proposed; current Solidity implementation profiled
- **Date:** 2026-07-22
- **Owners/scope:** `OriginRouter`, `TargetRouter`, `ERC7786MessengerBase`, message codecs
- **Depends on:** ADR-B-XCH-001, ADR-C-DES-001, ADR-C-INX-002 through ADR-C-INX-006, ADR-C-TOK-002
- **Related flow:** PFS-009, PFS-004

## Context

OriginRouter connects Desis/IntexFactory to N target deployments: it keeps a
target-chain registry, freezes a per-day snapshot of it at stage start, broadcasts
stage messages to every snapshot chain and addresses result/refund/issuance sends
per chain. The origin chain itself is one of the targets, reached through the
hub's loopback gateway. TargetRouter applies stage/result/issuance/refund/lifecycle
messages, relays bid batches followed by a BIDS_DONE completeness marker, bridges
holders and routes proceeds. Both are upgradeable and maintain remote peers plus
several pending retry queues. They coordinate but do not own the underlying
ledgers.

## Decision

Every inbound message is authenticated by gateway, source domain and exact configured
remote peer, decoded through a versioned canonical codec, and recorded in a durable
inbox before effects. Each message kind has an explicit replay outcome. Outbound
business intent is recorded as a durable outbox item before/with its local transition;
send success records transport id, while failure remains retryable without recreating
the economic effect.

Pending bids, issuance mints, holder migrations, refunds and proceeds routes use typed
records with immutable payload commitment, attempt state and one terminal disposition.
Permissionless flush methods operate only on stored intents and cannot substitute data.
Admin sweep excludes value reserved by pending routes.

## Authoritative interfaces

OriginRouter's Desis/IntexFactory send methods and authenticated receive dispatch own
Outbe-side routing; proceeds receive/retry owns the composed WCOEN distribution seam (per source
chain, feeding the day's proceeds fan-in).
TargetRouter's receive dispatch, bid relay, issuance mint, holder bridge and proceeds
flush methods own target-side orchestration. `wire`, peer setters, proceeds routes,
roles, target registry membership and upgrades are privileged configuration.

## Invariants

- Message `(source domain, peer, receiveId)` is applied at most once for its kind.
- One outbox intent has at most one acknowledged terminal send/effect.
- Pending records contain the exact original series, recipients, amounts and source.
- Routers never mint, finalize escrow or distribute proceeds except through the owning
  module capability and authenticated intent.
- Native/token value held equals explicit pending liabilities plus sweepable surplus.

## Atomicity, replay and failure

Inbox plus local effect is atomic or the router records a typed pending item before
returning transport success. Self-call isolation is permitted only for bounded item
failure and must prove authorization. Flush marks/commits state in reentrancy-safe order
and rolls back on outbound failure. Duplicate, reordered and superseded messages have
specified non-effect outcomes.

## Determinism and bounds

Message and array sizes, bid pages, mint/refund/holder items, pending queue growth,
failure bytes and destination gas are bounded. Cursor-based flush avoids scanning an
unbounded queue. Codecs reject trailing/unknown fields and narrowing overflow.

## Compatibility, trust and activation

Codec/version, message tags, roles, peers/domains, immutable gateway, downstream
addresses, gas profile and UUPS storage activate atomically across both chains. Upgrade
drills prove storage preservation and in-flight/pending-message compatibility.

## Production-interface verification evidence

Inspected both routers' wire/send/receive/dispatch and all pending retry paths, composed
token proceeds callback, sweep and upgrade scripts. Foundry cross-chain and upgrade tests
exist, but the evidence ledger lacks a complete duplicated/reordered/failure matrix over
two production-shaped endpoints.

## Consequences

PFS-004 can rely on observable inbox/outbox states rather than synchronous-call hope.
Routers remain orchestration modules and cannot absorb ledger invariants.

## Rejected alternatives

- Using terminal business stage as the only evidence that all messages were sent is
  rejected.
- Best-effort catch-and-forget is rejected.
- Admin-supplied retry payloads are rejected.

## Open questions and technical debt

- **Critical:** audit every message kind for durable receive-id replay protection; peer
  authentication alone does not prevent duplicate economic effects.
- **Critical:** prove all caught downstream failures create a durable immutable pending
  record. No catch may report delivery while losing issuance/refund/proceeds work.
- Bound pending queues and define ordering, cancellation, expiry, operator monitoring and
  fee ownership for retries.
- Define safe peer/role/wiring rotation with in-flight messages and outstanding value.
- Reconcile native sweeps against pending bridge fees/proceeds liabilities.
- Add a two-chain adversarial suite for duplicate/reordered messages, wrong source,
  partial batches, destination OOG, retry, upgrade and route rotation.
