# ADR-S-TEE-003: Startup TEE delivery is typed, acknowledged and bounded

- **Status:** Proposed; implementation present, production evidence incomplete
- **Date:** 2026-07-21
- **Scope:** TEE identity exchange, enclave DKG and block-1 bootstrap gossip
- **Depends on:** ADR-B-CNS-001, ADR-B-WIR-001, ADR-S-TEE-001, ADR-S-TEE-002

## Context

TEE startup previously retained every outbound message and replayed the complete
transcript every 750 ms. Receivers semantically deduplicated messages, but the
sender had no delivery state, could not stop retransmitting acknowledged messages
and used a fixed retry interval. The outer startup timeout bounded total duration,
but did not bound amplification inside that duration.

## Decision

Startup TEE gossip uses a delivery envelope distinct from semantic DKG messages.
Every data envelope has a canonical identity derived from a ceremony scope and
payload. The authenticated P2P recipient returns a delivery receipt directly to
the authenticated sender. State is tracked independently for each message and
recipient; acknowledged recipients are never targeted again.

Retries use capped exponential backoff on the consensus runtime clock. Pending
message count and total bytes are bounded. Broadcasts use discovery-wide delivery
until the expected remote peers are known, then target only unacknowledged peers.
The existing enclosing `tee_bootstrap_timeout_secs` remains the single ceremony
deadline. Expiry returns an explicit startup error; it does not create an
independent inner deadline or a background retrying process.

Transport receipts mean only that a peer received the envelope. They do not
replace the cryptographic DKG `Ack`, finalized dealer log or validator signature.
Receipt identities are scope-bound so stale receipts from another startup
ceremony cannot retire current work.

## Invariants

- one pending state entry per canonical `(scope, payload)` identity;
- receipt authority comes from the authenticated P2P sender identity;
- an acknowledged `(message, peer)` is not retransmitted;
- retry delay grows monotonically to a fixed cap;
- pending messages and bytes never exceed configured constants;
- semantic messages remain idempotent because transport receipt and protocol
  acceptance are separate facts;
- ceremony deadline/error ownership remains in the consensus startup supervisor.

## Consequences

Transient registration races remain recoverable without replaying the full
transcript at a fixed rate. Delivery adds one envelope/receipt wire contract, so
mixed binaries cannot share a startup ceremony and must be governed by protocol
version activation.

## Verification evidence

Unit tests cover per-peer retirement and the `1,2,4,8,16` capped retry schedule.
Existing bootstrap and DKG codec tests continue to cover semantic payloads.

## Open questions and technical debt

- Add fault-injection integration tests that drop each message and receipt type,
  assert a byte/send upper bound and prove terminal startup error at the existing
  deadline.
- Run those tests under mock and hardware SGX and record enclave side-effect
  counts, not only eventual ceremony success.
- Add delivery queue/receipt/retry metrics and expose the terminal reason in node
  readiness diagnostics.
- Pin an explicit delivery-envelope version before mixed-version deployment.
