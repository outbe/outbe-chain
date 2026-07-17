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
| 2 | local ERC-1155 | create matching series/supply cap | token metadata/state |
| 3 | OriginRouter | send bridge issuance instructions | send id/message |
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

Duplicate series/delivery must be rejected or idempotent. Failed bridge send rolls
back local source issuance under the current synchronous call boundary. Failed
payment/vault/NFT step rolls back settlement. Invalid/replayed PoW or failed Promis
mint leaves sequence and Settled balance unchanged. Restart must reconstruct scans
and bridge delivery without reissuing the series.

## E2E scenario matrix

| Id | Scenario | Minimum topology | Required assertions | Automated by |
|---|---|---|---|---|
| PFS-004-01 | issue, qualify, settle, mine | validators + Oracle/vault/bridge | all identities and equations | GAP |
| PFS-004-02 | authorized dual-wallet settlement | same | payer/holder/token ownership semantics | GAP |
| PFS-004-03 | settlement after Called deadline | same | revert; no token/reserve changes | GAP |
| PFS-004-04 | fee-on-transfer settlement asset | same | measured delta and explicit economics | GAP |
| PFS-004-05 | vault returns zero shares | same | complete settlement rollback | GAP |
| PFS-004-06 | replay mining PoW/sequence | same | one burn/mint only | GAP |
| PFS-004-07 | duplicate bridge delivery | paired networks | one series/supply | GAP |
| PFS-004-08 | restart during qualification/distribution | same | deterministic scan/index state | GAP |

## Open questions and technical debt

- Settlement asset is currently selected through the first VaultProvider asset in
  inspected paths; bind it to the series currency before accepting this flow.
- Define bridge delivery idempotency and recovery after source finality/remote
  failure.
- Reconcile the Rust Intex FSM with the richer ERC-1155 expiry/sweep lifecycle.
- Pin PoW format/difficulty activation and provide independent vectors.
- Prove contributor/proceeds distribution is not erased or replayed by a second
  delivery.
- No production e2e currently spans issuance, bridge, qualification, reserve
  settlement and Promis mining.
