# ADR-C-DES-001: Desis owns the cross-chain auction, bid relay and clearing FSM

- **Status:** Proposed; current implementation profiled; critical atomicity gap
- **Date:** 2026-07-17
- **Owners/scope:** `crates/core/desis`, its OriginRouter ABI, auction storage and
  uniform-rate clearing algorithm
- **Depends on:** ADR-C-MET-001, ADR-C-PRM-003, ADR-C-INX-001, ADR-C-INX-002
- **Related flow:** PFS-004

## Context

Desis bridges a Metadosis-derived Intex supply to a demand auction on another
chain. It sends stage messages to a fixed OriginRouter, accepts unordered batches
of revealed bids back from that router, clears them deterministically, issues Intex
to winners, returns unsold Promis capacity and sends result/refund instructions.
This is one state owner with an asynchronous cross-chain FSM; it must not be hidden
inside the broader WorldwideDay or Intex settlement documents.

## Decision

Desis is the sole owner of per-series auction configuration, stage, relayed bid
generation/completeness, pending supply and the latest-clearing inputs used for the
next minimum quantity. Metadosis may signal start, reveal/cancel and clearing
through typed Rust commands. Only the fixed OriginRouter address may submit bid
batches or invoke final clearing through the public ABI.

The normative implementation must use explicit dispatch intents/outbox receipts:
local state transition and cross-chain send either commit as one acknowledged
command, or the local state records a retryable `DispatchPending` state. A helper
must never convert an error to “best effort succeeded/fell back” while retaining
partial writes.

## State and invariants

The FSM stores configuration (currencies, Promis load, price/call parameters,
minimum rate/quantity and bond), stage, source endpoint/generation, dense bid
records, batch total/arrival bitmap, pending whole-Intex supply and clearing marker.
Global state records the most recently cleared series and issued count.

For each nonzero series:

- configuration is immutable after `None -> Started` and all numeric/currency
  relationships satisfy the IntexFactory profile;
- stage follows the explicit FSM and terminal stages never reopen;
- a current relay generation has one source endpoint, one total in `1..=256`, an
  arrival mask with no out-of-range bits and bids from each batch exactly once;
- `BidsReceived` means every current-generation batch arrived;
- clearing is possible only when Metadosis supplied a pending amount, including an
  explicitly initiated zero supply;
- issued count is at most pending supply, winner and quantity arrays align, and
  unsold whole units plus rounding remainder conserve the supplied Promis budget;
- every bidder has exactly one paid/refunded accounting result whose sum equals
  its escrow basis under the specified rounding policy;
- `Cleared` commits Intex issuance, PromisLimit return and outbound result/refund
  receipts together.

## State machine and authorities

```text
None --Metadosis start----------------------------> Started
Started --green reveal + outbound ack-------------> Revealing
Started --red reveal + outbound ack---------------> Cancelled (terminal)
Revealing --Metadosis supplies clearing capacity--> Revealing/SupplyReady
Revealing --all router bid batches received-------> BidsReceived
BidsReceived --router clear------------------------> Cleared (terminal)
```

The current schema represents `SupplyReady` with `clearing_initiated` plus pending
supply instead of a separate stage. A higher bid generation supersedes incomplete
lower-generation bids; lower generations reject. Duplicate batch indices are
idempotent. Deliveries after `BidsReceived`, `Cleared` or `Cancelled` are no-ops so
the bridge can stop retrying.

Metadosis APIs intentionally return `bool` or a Promis remainder instead of
halting its lifecycle. That fallback is safe only if a failed Desis command has no
remaining state, event or outbound effect, or returns a durable receipt describing
exactly what did commit.

## Clearing algorithm and ordering

Bids sort by descending rate and ascending timestamp. Eligible bids meet minimum
rate and quantity; allocation proceeds until supply is exhausted. All winners pay
the last allocated bid's uniform clearing rate. Each bidder locked at its submitted
rate and receives `locked - paid`; losers receive the full lock. Empty bids or zero
supply produce a no-sale result and return all whole supply.

Equal rate and timestamp require a final canonical tie-breaker independent of
unordered batch arrival, such as source-chain bid id. Saturation is not an accepted
economic overflow policy: every multiplication, sum and narrowing must either
prove bounds or fail before state transition.

## Atomicity, cross-chain effects and replay

Start/reveal/begin-clearing currently write state and then call OriginRouter.
Clearing writes terminal state, returns Promis, issues Intex, then calls the router
twice. All are EVM subcalls and ordinarily share the transaction journal, but the
Metadosis-facing “best effort” wrapper catches errors inside the same call and thus
can commit earlier writes unless it creates and reverts its own checkpoint.

Bid replay is keyed by `(series, generation, batch_index)`. A higher generation is
a replacement, not a replay. Cross-chain result/refund sends require their own
message identity and durable acknowledgement/retry policy; relying only on terminal
stage prevents retry after a remote send was not durably accepted.

## Determinism and bounds

The 256-batch bitmap bounds batch count but neither bids per batch nor total bids.
Bid ingestion appends to storage; clearing loads and stable-sorts every bid, creates
several O(n) arrays and sends all bidders in one outbound call. Explicit maximum
bids/bytes/gas and bridge frame limits are required.

Series id is currently the UTC date key derived from Metadosis scheduled auction
time. OriginRouter address, source chain/endpoint, time offsets, fixed-point scale,
Promis load, 4% rule and floor/call formulas are protocol compatibility constants.

## Compatibility, trust and production evidence

The router is an authenticated transport endpoint, not an economic truth oracle.
Desis must validate the expected source endpoint/domain and canonical message
identity in addition to `msg.sender`. ABI structs, enum values, packed bid layout,
generation semantics, sort/tie-break, rounding and result/refund arrays require
coordinated two-chain activation.

Evidence inspected includes Desis schema/state/runtime/API/precompile/tests and
Solidity interfaces, OriginRouter send/receive seams, Metadosis callers,
PromisLimit and IntexFactory effects. Tests cover main stages, origin gate,
generation replacement/idempotency, zero bids/supply, uniform price and fallback
returns. They do not prove rollback of partial best-effort commands or bounded
production cross-chain execution.

## module audit profile

The intended commands are `StartAuction`, `RevealOrCancel`, `SupplyAuction`,
`AcceptBidBatch` and `ClearAuction`, each returning a typed state/effect receipt.
architectural closure requires explicit transition types, repository-owned state,
atomic/outbox effects, canonical message replay ids, bounded work and conservation
properties over the pure clearing function.

## Consequences and rejected alternatives

Desis can evolve its bridge and clearing rules without making Metadosis own remote
bid state or making Intex own auction transport. A fixed router caller gate reduces
the public mutation surface but does not replace source-message validation.
Best-effort fallbacks remain useful for liveness, but swallowing an error with
partial state was rejected. Folding Desis into the Metadosis or Intex ADR was
rejected because its asynchronous generations, escrow accounting and outbound
receipts form a separate state machine.

## Open questions and technical debt

- **Critical:** `best_effort` catches failures without a checkpoint. `start_auction`
  or `reveal_auction` may retain stage/config/events after the router call failed
  while returning `false` to Metadosis.
- **Critical:** `dispatch_stage_clearing` returns the entire Promis supply on error,
  but `begin_clearing` writes `clearing_initiated` and pending supply before its
  router call. A failed call can therefore return budget to PromisLimit while
  leaving the same supply clearable later. Revert partial state immediately and add
  a conservation regression test.
- Replace synchronous router sends with a versioned outbox/acknowledgement model or
  specify exactly why an EVM child-call success is durable cross-chain acceptance
  and how result/refund sends retry after downstream failure.
- Add a canonical bid id tie-breaker. Equal rate and timestamp currently preserve
  storage order, which depends on unordered batch delivery order.
- Bound bids per batch, total bids per series, encoded result/refund size, sorting
  work and child-call gas. The current 256 bound applies only to batch count.
- Replace saturating `rate_lock`, saturating time arithmetic and unchecked bid-count
  increment with checked domain bounds; saturation can fabricate non-conserving
  payments instead of rejecting invalid input.
- Validate bidder nonzero, positive quantity, rate range, timestamp window,
  duplicate bid identity and expected `src_chain_id`; today OriginRouter address is
  the principal admission check.
- Require a positive/nonzero relay generation or explicitly initialize generation
  zero. Default generation metadata makes a first generation-zero batch fail its
  stored-total check.
- Define whether a newer generation may replace already accumulated economic bids
  without an authenticated replacement reason/hash, and clear obsolete bid slots
  for state hygiene/migration.
- Make clearing order for global `last_cleared_series_id` explicit. An older series
  cleared late can become “last” and affect the next 4% minimum quantity.
- Reject unexpected `msg.value` in payable `clearAuction` or account/refund it; the
  current runtime does not use the value.
- Use persisted configured issuance/reference currencies when issuing Intex or
  prove they must equal compiled constants; current clearing constructs issuance
  with fixed constants after storing per-series currencies.
- Add property tests for allocation/refund conservation, permutation invariance,
  duplicate bidders, maximum values, rounding dust and overflow, plus E2E tests for
  router retry/reorder/replacement and failures at every downstream effect.
