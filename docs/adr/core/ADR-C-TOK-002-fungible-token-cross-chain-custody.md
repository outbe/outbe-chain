# ADR-C-TOK-002: Fungible token bridge conserves lock/unlock and burn/mint routes

- **Status:** Proposed; current Solidity implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `contracts/tokens/src/ERC7786TokenBridge.sol`
- **Depends on:** ADR-C-TOK-001, ADR-B-XCH-001, ADR-B-CAP-001
- **Related flow:** PFS-004

## Context

Each token route pairs a canonical `LockUnlock` endpoint with a synthetic `BurnMint`
endpoint over ERC-7786. Users can send plain transfers or composed transfers invoking a
destination receiver hook after credit.

## Decision

Mode, local token and gateway are immutable. A versioned remote bridge table maps each
domain to one interoperable peer. Outbound validates recipient/amount/payload/gas, then
locks or burns exactly the sent amount before dispatch. Inbound authenticates gateway,
source domain and exact remote peer, consumes a durable receive id, unlocks or mints,
then optionally calls the receiver. A hook failure reverts the complete delivery so the
same message remains retryable without duplicated credit.

## Authoritative interfaces

`send`, `sendAndCall`, `quoteSend`, authenticated `receiveMessage`, and owner
`setRemoteBridge` are the complete bridge commands. Token contracts own balances;
ADR-B-XCH-001 owns transport authentication below this layer.

## Invariants

- Canonical custody plus canonical circulating supply is conserved across lock/unlock.
- Synthetic mint minus burn equals net authenticated cross-chain inflow.
- One receive id credits at most once and only from its configured peer/domain.
- Payload sender, recipient, amount and composed-call data cannot be substituted.
- Mode and token never change after deployment.

## Atomicity, replay and failure

Lock/burn plus send is one source transaction. Inbox consumption, unlock/mint and hook
are one destination transaction. Duplicate callbacks consult durable replay state.
Remote rotation defines in-flight-message behavior and retains historical authentication
until a safe cutoff.

## Determinism and bounds

Extra data and destination gas are bounded. Domain narrowing is checked. Hook gas is
explicit and reentrancy is blocked across the whole receive effect.

## Compatibility, trust and activation

Mode, token pair, peer/domain table, gateway, payload codec and gas selector activate as
one two-chain route manifest. Owner rotation is governed and observable.

## Production-interface verification evidence

Inspected both modes, peer lookup, payload encoding, gateway/remote authentication,
composed hook and token effects. Package documentation contains deployment flows; tests
do not yet provide a complete two-chain duplicate/reorder/rotation conservation matrix.

## Consequences

Applications can compose token delivery without weakening conservation. The bridge must
maintain durable replay state and route-version operations.

## Rejected alternatives

- Gateway-only authentication is rejected.
- Crediting before durable replay consumption is rejected.
- Catching a receiver hook revert while retaining token credit is rejected.

## Open questions and technical debt

- **Critical:** no explicit replay mapping is visible in `ERC7786TokenBridge`; prove the
  configured transport guarantees durable deduplication or add local receive-id state.
- Add two-chain conservation invariants for duplicates, reorder, destination failure,
  peer rotation and retry in both modes.
- Define fee surplus/refund ownership and prevent trapped native value.
- Bound `extraData` and hook gas; reject malicious receiver reentrancy across token and
  application state.
- Replace immediate owner peer changes with governed two-step route activation and an
  in-flight-message cutoff.

