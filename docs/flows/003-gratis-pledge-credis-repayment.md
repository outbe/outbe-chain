# PFS-003: Gratis pledge opens Credis and installments release reclaim notes

- **Status:** Draft
- **Actors:** Gratis owner/borrower bundle, Gratisfactory, GratisPool, Gratis,
  CredisFactory, Credis, Oracle, VaultProvider and reserve asset/vault
- **Trigger:** User pledges Gratis, then requests Credis with the shielded note
- **Topology/services:** Finalizing network with configured Oracle, reserve asset,
  vault, source/target registrations and proof verifier
- **Referenced ADRs:** ADR-C-GRT-001, ADR-C-GRT-002, ADR-C-GRT-003, ADR-C-FID-001, ADR-C-CRD-001, ADR-C-CRD-002,
  ADR-C-VLT-001, ADR-S-ORC-001
- **Supersedes:** None

## Outcome

One shielded Gratis pledge backs one uniquely identified Credis position, exact
stablecoin liquidity reaches the borrower bundle, and each of ten repayments
returns one independently spendable reclaim note without losing conservation or
permitting replay.

## Acceptance contract

- **Source:** Gratis owner operating through its borrower bundle.
- **Trigger:** A user pledges an eligible Gratis denomination, opens Credis with the shielded note, then submits ordered repayments.
- **Environment:** Finalizing network with configured proof verifier, Oracle, reserve asset, vault liquidity and registered source/target modules.
- **Canonical inputs:** Bundle-bound commitment/nullifier/proof, denomination and collateral, Fidelity eligibility, Credis terms, Oracle rates, exact reserve asset, vault shares, allowances and repayment amounts.
- **System under test:** Gratisfactory, GratisPool, Gratis, CredisFactory/Credis, Oracle, VaultProvider and reserve token/vault adapters.
- **Expected response:** Pledge/root evidence, one Credis position with ten installments, asset disbursement, spent nullifiers, repayment receipts and one reclaim commitment per installment.
- **Response measures:** Debt, collateral, token and vault equations close; every nullifier, position and installment is consumed at most once; completed debt rejects payment and collateral is reclaimable once.
- **Failure guarantee:** Failed proof, withdrawal or deposit leaves the transaction's prior root, nullifier, position, cursor, debt and token/vault balances intact.

## Preconditions and canonical inputs

- User owns sufficient liquid Gratis and satisfies accepted Fidelity eligibility.
- Denomination is pledge- and Credis-eligible; commitment/nullifier/proof inputs use
  the pinned circuit/domain version.
- Bundle has no overdue position; asset reports a registered ISO currency.
- Oracle has exchange/refinancing rates; VaultProvider has matching reserve shares
  and CredisFactory is the registered target/source as applicable.

## Success sequence

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | Gratisfactory | append pledge commitment and move denomination to Gratis escrow | pool root + escrow/pledged balances |
| 2 | CredisFactory/Pool | verify bundle-bound proof and consume nullifier | spent-nullifier state |
| 3 | CredisFactory/Oracle | calculate stable amount and snapshot refinancing/currency | position fields |
| 4 | Credis | create unique position and ten installments | position/index records |
| 5 | VaultProvider | withdraw exact asset into borrower bundle | token/vault deltas and event |
| 6 | borrower, repeated 10x | pay next asset installment into reserve | installment cursor/debt delta |
| 7 | CredisFactory/Pool | append denomination-bound reclaim note | new pool root |
| 8 | Gratisfactory, optional | spend reclaim note and release exact Gratis escrow | nullifier + liquid/pledged deltas |

## Boundaries and conservation

Pledge, request, each repayment, and each unpledge are separate user transactions.
Within each transaction every listed module/external call rolls back together.
Replay protection crosses transactions through commitment uniqueness, nullifiers,
position id and next-installment cursor.

Intended closure is:

```text
live unreclaimed position collateral <= pledged Gratis escrow backing
sum(installment debt paid + outstanding debt) = original recorded debt
sum(reclaimed collateral + outstanding collateral) = original position collateral
vault/token deltas = disbursement and repayments in the position asset
```

## Observable completion contract

After request: receipt succeeds, position is owned by the bundle, ten installments
and pinned terms are readable, original nullifier is spent, bundle token balance
rose by the disbursed amount, and vault shares fell consistently. After each
payment: only the next installment is paid, outstanding fields close, reserve
liquidity increases and one valid reclaim commitment appears. After completion no
additional payment is accepted and all intended collateral can be reclaimed once.

## Replay, retry, restart and failure

Copied request proof cannot redirect the bundle and a spent nullifier cannot open a
second position. Failed vault withdrawal rolls back position/nullifier. Failed
repayment deposit rolls back cursor and reclaim insertion. Wrong/invalid reclaim
must fail before value becomes irrecoverable—this is not true for all current opaque
commitments and remains a blocking debt.

## E2E scenario matrix

| Id | Scenario | Minimum topology | Required assertions | Automated by |
|---|---|---|---|---|
| PFS-003-01 | pledge, request, ten payments, full reclaim | 4 validators, Oracle/vault/prover | every closure equation | in-process `full_request_pay_reclaim_unpledge_flow`; verifier and Solidity vault/token effects are stubbed |
| PFS-003-02 | replay/copy request proof | same | no redirect or duplicate position | pool/runtime module tests only; cross-module replay example not yet implemented |
| PFS-003-03 | overdue borrower requests again | same | revert; old position unchanged | in-process `request_credis_rejects_overdue_anadosis` |
| PFS-003-04 | insufficient vault shares | same | nullifier and position rollback | documentation-only until the fixture provides a stateful failing VaultProvider adapter |
| PFS-003-05 | repayment token transfer/deposit fails | same | cursor/debt/root rollback | documentation-only until the fixture provides a stateful failing ERC-20/vault adapter |
| PFS-003-06 | invalid reclaim denomination | same | rejected before commit | documentation-only; current interface cannot prove the denomination and behavior is deficient |
| PFS-003-07 | early repayment | same | matches accepted due-date policy | documentation-only pending an explicit early-payment policy |
| PFS-003-08 | restart after each transaction boundary | same | identical position/proof/token state | documentation-only: no live-node Credis fixture or persistent in-process adapter exists |

## Open questions and technical debt

- Current code does not visibly reserve per-position pledged escrow; the intended
  collateral equation is not proven.
- Reclaim denomination is not verifiable at insertion and can strand collateral.
- Default/liquidation and explicit Completed state are undefined.
- Early-payment policy is undefined.
- Multi-asset selection currently depends on `assetAt(0)` in related factories;
  Credis must bind the exact position asset/currency throughout.
- The in-process lifecycle test covers the Rust module seam, but no scenario yet
  exercises production ABI, real ERC-20/vault effects and real proof verification.
