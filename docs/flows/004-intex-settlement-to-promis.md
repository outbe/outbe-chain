# PFS-004: Intex issuance and settlement becomes Promis

- **Status:** Draft
- **Actors:** Desis issuer, IntexFactory, Intex ledger, OriginRouter/bridge,
  Intex ERC-1155, holder/authorized settler, VaultProvider, Oracle and PromisFactory
- **Trigger:** Desis clearing issues a series; later holder/settler settles and mines
- **Topology/services:** Outbe validators, configured Oracle/vault/asset, local
  ERC-1155 contracts and paired bridge/router deployment
- **Referenced ADRs:** ADR-B-CNS-002, ADR-B-CNS-003, ADR-S-ORC-001, ADR-C-PRM-001, ADR-C-PRM-002, ADR-C-VLT-001,
  ADR-C-INX-001, ADR-C-INX-002, ADR-B-CRY-001
- **Supersedes:** None

## Outcome

One issued Intex series is consistently represented locally and across the bridge,
qualifies/calls under canonical prices, accepts authorized settlement into reserves,
and converts consumed soulbound Settled units into the exact Promis load once.

## Acceptance contract

- **Source:** Desis clearing for issuance, followed by the holder or authorized settler.
- **Trigger:** Desis issues an Intex series; an eligible holder later settles Issued units and mines the resulting Settled units.
- **Environment:** Finalizing validators with configured Oracle/vault/asset and a paired ERC-1155 bridge/router deployment.
- **Canonical inputs:** Unique series/currencies/recipients/quantities/prices/Promis load, bridge replay identity, Oracle observations, holder/settler authorization, settlement balance/allowance and sequence-bound PoW.
- **System under test:** Desis, IntexFactory/Intex, ERC-1155 ledger and bridge, VaultProvider, Oracle and PromisFactory.
- **Expected response:** Matching Rust/ERC-1155 series, bridge evidence, qualification/call state, reserve deposit, Issued-to-Settled transition, mine sequence and minted Promis.
- **Response measures:** Series/delivery are unique; Issued burned equals Settled minted; measured settlement equals reserved value; Settled burned times load equals Promis minted.
- **Failure guarantee:** Failed or replayed issuance, delivery, settlement or mining changes no series count, reserve/token balance, ownership, sequence or Promis supply.

## Preconditions and canonical inputs

- Desis provides a unique series id, issuance/reference currencies, recipients,
  quantities, entry price and Promis load whose totals fit declared supply.
- OriginRouter, ERC-1155, bridge and VaultProvider addresses/roles are correctly
  wired; relay float covers outbound delivery.
- Oracle and call parameters are configured and settlement asset is bound to the
  series currency.
- Holder owns Issued units; settler is holder or explicitly authorized and has
  allowance/balance.

## Success sequence

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | IntexFactory | derive floor/call values and create Rust series | series record/index |
| 2 | OriginRouter | send issuance instructions to every snapshot chain (the local ERC-1155 series arrives through the loopback leg) | send ids/messages |
| 3 | per-chain ERC-1155 | create series/supply cap, mint winners | token metadata/state |
| 4 | IntexFactory | enroll floor index; later qualify/call from canonical Oracle scans | typed FSM state |
| 5 | holder | optionally authorize a distinct settler | per-series authorization |
| 6 | IntexFactory/VaultProvider | pull actual payment delta and deposit reserves | token/share deltas |
| 7 | ERC-1155 | burn holder Issued and mint settler soulbound Settled units | balances/event |
| 8 | holder of Settled | submit sequence-bound valid PoW | mine sequence |
| 9 | ERC-1155 + PromisFactory | burn Settled and mint `promis_load * amount` | closed balances/supply |

## Boundaries and conservation

Issuance is one source-chain EVM transaction, while cross-chain delivery/finality is
a separate messaging boundary with its own replay key. Settlement and mining are
separate user transactions, each internally atomic.

```text
Issued burned on settlement = Settled minted
Settled burned on mining * series promis_load = Promis minted
settlement received token delta = amount deposited to reserve (subject to explicit fee policy)
```

## Observable completion contract

The Rust series and ERC-1155 identity/parameters agree; remote issuance is delivered
once. Qualification/call state is legal. Settlement receipt succeeds, reserve
shares increase nonzero, holder Issued falls and settler Settled rises equally.
Mining advances exactly one sequence, burns exact Settled units and increases Promis
balance/supply by the recorded load product.

## Replay, retry, restart and failure

Duplicate series/delivery must be rejected or idempotent. A failed issuance send
parks as a durable pending item on the router and is permissionlessly flushed;
the Rust series is not rolled back. Failed
payment/vault/NFT step rolls back settlement. Invalid/replayed PoW or failed Promis
mint leaves sequence and Settled balance unchanged. Restart must reconstruct scans
and bridge delivery without reissuing the series.

## E2E scenario matrix

| Id | Scenario | Given / canonical inputs | When / trigger | Then / outputs and postconditions | Verification |
|---|---|---|---|---|---|
| PFS-004-01 | full Intex lifecycle | unique series, recipients, Oracle, vault, bridge and valid PoW | issue, deliver, qualify/call, settle and mine | Rust/ERC-1155 identities agree; reserve/Issued/Settled/Promis equations close | documentation-only: composed Rust/Solidity fixture absent |
| PFS-004-02 | dual-wallet settlement | holder authorizes distinct funded settler | settler pays for holder's Issued units | holder burn, settler payment/Settled ownership and reserve delta agree | runtime/Foundry fragments only |
| PFS-004-03 | settlement after deadline | Called series past canonical deadline | holder/settler attempts settlement | revert; Issued/Settled/reserve/authorization unchanged | runtime/Foundry fragments; joint rollback unproved |
| PFS-004-04 | fee-on-transfer asset | settlement token deducts transfer fee | settle eligible amount | measured received delta is reserved under explicit economics | documentation-only: token/vault fixture absent |
| PFS-004-05 | zero vault shares | payment transfer succeeds but vault returns zero shares | settle eligible amount | entire payment, reserve and NFT transition rolls back | documentation-only: failing vault fixture absent |
| PFS-004-06 | mining replay | holder has Settled units and one accepted PoW sequence | replay same/old mining proof | one burn/mint only; sequence and balances unchanged on replay | runtime fragments only; ERC-1155/Promis not composed |
| PFS-004-07 | duplicate bridge delivery | source issuance and already-consumed delivery id | relay same message again | one remote series/supply; replay result deterministic | Foundry `DuplicateProtection.t.sol`; paired Rust state absent |
| PFS-004-08 | restart during scans/distribution | durable issued series and pending qualification/distribution | restart at each checkpoint | scan cursor, delivery and contributor state resume exactly once | documentation-only: paired-network checkpoints absent |

## Open questions and technical debt

- Settlement asset is currently selected through the first VaultProvider asset in
  inspected paths; bind it to the series currency before accepting this flow.
- Define bridge delivery idempotency and recovery after source finality/remote
  failure.
- Reconcile the Rust Intex FSM with the richer ERC-1155 expiry/sweep lifecycle.
- Pin PoW format/difficulty activation and provide independent vectors.
- No production e2e currently spans qualification, reserve settlement and Promis
  mining in one walk; issuance through creator payout is covered in-process by
  PFS-009's automation.
