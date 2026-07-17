# ADR-B-XCH-001: ERC-7786 transport authenticates routes and provides replay-safe delivery

- **Status:** Proposed; current Solidity implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `contracts/crosschain`, ERC-7786 bridge facade and Hyperlane/LayerZero adapters
- **Depends on:** ADR-B-CRY-001, ADR-B-CAP-001, ADR-B-DEP-001
- **Related flows:** PFS-003, PFS-004

## Context

Outbe protocols exchange typed messages across chains through an ERC-7786-shaped
gateway. `ERC7786Bridge` wraps a configured gateway and remote bridge registry;
Hyperlane and LayerZero adapters translate interoperable addresses, fees, gas
attributes and authenticated inbound transport into that interface. Every Core
cross-chain protocol depends on this substrate, but transport must not interpret
auction, token or intent business state.

## Decision

The transport boundary is a versioned envelope carrying message identity, source
domain, authenticated source endpoint, destination endpoint, payload commitment,
gas policy and transport-specific delivery identity. Applications accept a message
only after the adapter proves both the configured gateway and configured remote
peer. Transport delivery is at-least-once; exactly-once business effects require a
durable `(route_version, source_domain, source_peer, message_id)` inbox owned by the
receiving application or a shared transport inbox with equivalent atomicity.

Owner changes to gateway, routes, peers, default gas and pause state are protocol
configuration changes. They emit old/new values, are time-delayed or governed, and
cannot silently reinterpret already dispatched messages. Quote is advisory; send
must validate the same payload, route and attributes and return the transport id.

## Authoritative interfaces

- `IERC7786GatewaySource.sendMessage`, `quote`, and `supportsAttribute` are the
  outbound capability.
- `IERC7786Recipient.receiveMessage` is the authenticated inbound capability.
- `ERC7786Bridge`, `HyperlaneGatewayAdapter` and `LayerZeroGatewayAdapter` own route
  translation, remote-peer admission, fee forwarding and transport callbacks.
- Application routers own payload decoding, replay state and economic effects.

## Invariants

- A route resolves to exactly one `(transport, remote domain, remote peer)` version.
- An inbound callback from any other gateway/mailbox/endpoint or peer has no effect.
- The application sees an unambiguous source domain and message id; chain-id
  narrowing cannot alias two domains.
- Pausing blocks new sends and inbound application dispatch according to one stated
  emergency policy without making already locked value unrecoverable.
- Native fee surplus/refund ownership is explicit; contracts do not accumulate
  owner-sweepable user value accidentally.

## Replay, ordering and failure

Transport ordering is not a business guarantee. Messages may duplicate, delay or
arrive out of order. A callback either commits inbox identity plus the full local
effect atomically, records a durable retry item, or reverts so the underlying
transport can retry. Configuration changes retain enough route history to
authenticate in-flight messages. Unsupported attributes, malformed interoperable
addresses, fee shortfall and downstream rejection are typed separately.

## Determinism and bounds

Payload bytes, attribute count/size, destination gas, batch fan-out and callback
work are bounded before value leaves the source. Gas attributes use one canonical
selector and integer width. Domain/chain conversions are checked. No adapter loops
over attacker-controlled unbounded arrays during inbound authentication.

## Security and activation

Gateway, mailbox/endpoint, route owner and remote peers are explicit trust roots.
Deployment manifests bind bytecode hash, constructor immutables, owner, route table,
chain ids and transport versions. Peer rotation is a coordinated two-chain change
with rollback and in-flight-message policy. Emergency pause, recovery and ownership
transfer are exercised before activation.

## Production-interface verification evidence

Inspected production contracts include `ERC7786Bridge.sendMessage/receiveMessage`,
Hyperlane `handle`, LayerZero `_lzReceive`, route setters, quote paths and gas-limit
attribute translation. Contract tests exist under `contracts/crosschain/test`, but
the catalog has no transport-agnostic adversarial conformance suite proving the
same authentication/replay behavior for both adapters.

## Consequences

Business ADRs can reason in terms of authenticated at-least-once messages without
depending on Hyperlane or LayerZero internals. The cost is a durable replay boundary,
versioned route operations and explicit treatment of in-flight messages.

## Rejected alternatives

- Treating `msg.sender == gateway` alone as source authentication is rejected.
- Assuming transport-level exactly-once delivery is rejected.
- Letting each business module invent address/domain parsing and gas attributes is
  rejected because it creates incompatible trust boundaries.

## Open questions and technical debt

- **Critical:** `ERC7786Bridge.receiveMessage` and both adapters need one audited,
  end-to-end proof that gateway plus remote peer are checked before application
  dispatch and that domain encodings cannot alias.
- **Critical:** define durable replay ownership. The generic bridge exposes a
  delivery id but does not itself prove exactly-once downstream effects.
- Route/gateway setters are immediate owner operations; add timelock/governance,
  two-step ownership and an in-flight-message rotation policy.
- Define pause semantics for inbound messages. Rejecting delivery can strand value;
  accepting while paused can defeat incident containment.
- Bound payload and attributes and reject duplicate/unknown attributes. Document
  whether excess `msg.value` is refunded, forwarded or recoverable.
- Add a shared conformance suite covering forged gateway/peer/domain, duplicate and
  reordered delivery, route rotation, underfunded/overfunded fees, reentrancy,
  destination OOG, retry and pause/unpause for Hyperlane and LayerZero.
- Pin external transport dependency versions and deployment bytecode hashes in the
  cryptographic/protocol manifest governed by ADR-B-CRY-001.
