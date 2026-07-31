# ADR-C-INX-004: Intex ERC-1155 bridging is replay-safe burn/mint with explicit recovery

- **Status:** Proposed; current Solidity implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `contracts/intex/src/shared/IntexNFT1155Bridge.sol` and codec
- **Depends on:** ADR-C-INX-003, ADR-B-XCH-001, ADR-B-CAP-001
- **Related flow:** PFS-004

## Context

The upgradeable bridge burns Intex ERC-1155 balances on the source, sends single,
batch, multi-recipient or system-holder messages and mints on the paired deployment.
Individual destination mints may be parked and retried or reclaimed to source. This
recovery ledger is an independent state machine.

## Decision

Every outbound transfer validates a bounded canonical payload before burning, binds
source account, destination chain/peer, token ids, amounts, recipient(s), mode and
message version, then returns a transport id. Inbound authentication follows
ADR-B-XCH-001 and records message/item disposition before mint:

```text
Unseen -> Minted
       -> MintFailed(reason, payload commitment)
MintFailed -> Minted | Reclaimed
```

Exactly one terminal effect exists per item. Retry uses the stored immutable payload;
reclaim sends a compensating mint to the authenticated source account and cannot race
with retry. System holder migration is separately authorized and conservation-equivalent
to ordinary holder-initiated transfer.

## Authoritative interfaces

`send`, `batchSend`, `multiSend` and `systemMultiSend` are outbound commands.
Authenticated `_dispatch` plus `crosschainMintOne` owns inbound execution.
`retryCrosschainMint` and `reclaimToSource` own recovery. `setRemoteMessenger`, upgrade
and native sweep are privileged operations.

## Invariants

- Per `(receiveId,item)` exactly one of Minted, pending failure or Reclaimed is true.
- Total burn on one side equals terminal mint or reclaim amount on the other.
- Array lengths align, items are nonzero/valid and batch size never exceeds the profile.
- A remote peer, token id, amount or recipient cannot be substituted during retry.
- Only self-call may enter the isolated per-item mint trampoline.

## Atomicity, replay and failure

Outbound burn and accepted transport send share one transaction. Inbound per-item
failure is isolated deliberately and durably recorded; successful items do not retry.
Duplicate messages consult the inbox/disposition before effects. Recovery marks terminal
state before external send under reentrancy protection, with rollback on send failure.

## Determinism and bounds

All modes cap item count, encoded bytes, destination gas and storage for failure reasons.
Gas is derived by one checked profile. Failure data is truncated/hashed, not stored
unbounded. Loops cannot be driven beyond the declared maximum.

## Compatibility, trust and activation

Codec message tags, token-id semantics, remote peers, transport, bridge/token immutables,
roles and UUPS storage layout activate as one two-chain profile. Peer or implementation
rotation defines in-flight-message handling.

## Production-interface verification evidence

Inspected all outbound modes, base receiver authentication, per-item trampoline,
failed-mint storage, retry/reclaim and admin seams. Foundry tests cover single/batch/
multi/UUPS paths, but independent two-chain failure/replay evidence is incomplete.

## Consequences

Partial batch progress is explicit and recoverable without duplicating successful mints.
This requires durable per-item state and operator/user-visible recovery APIs.

## Rejected alternatives

- Reverting an entire large batch for one bad recipient is rejected for liveness.
- Swallowing mint failure without a durable item record is rejected.
- Owner arbitrary remint is rejected as a recovery mechanism.

## Open questions and technical debt

- **Critical:** prove duplicate delivery cannot overwrite a failed item, mint twice or
  race `retryCrosschainMint` with `reclaimToSource`.
- **Critical:** prove the self-call mint trampoline cannot be reached with forged payload
  and that role configuration authorizes only the paired bridge.
- Bound all batch modes, failure-reason bytes and destination gas with adversarial tests.
- Define who may choose retry versus reclaim, timeout/finality requirements and fee payer.
- Add supply-conservation tests spanning two real bridge endpoints, duplicated/reordered
  transport, partial failure, reclaim and UUPS/peer rotation.
- Audit `sweepNative` so it cannot seize user fees reserved for recovery/in-flight sends.

