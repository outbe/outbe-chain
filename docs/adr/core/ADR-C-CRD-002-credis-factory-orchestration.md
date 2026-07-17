# ADR-C-CRD-002: CredisFactory owns collateral-proof, pricing and repayment orchestration

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Credis protocol maintainers
- **Scope:** `crates/core/credisfactory`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-C-GRT-001, ADR-C-GRT-003, ADR-C-CRD-001, ADR-C-VLT-001, ADR-S-ORC-001
- **Related:** ADR-C-GRT-002, ADR-C-FID-001
- **Supersedes:** CredisFactory portions of former broad pre-space Gratis/economic aggregate (previously numbered 030)

## Context

CredisFactory turns one shielded pledged-Gratis note into a credit position and
reserve disbursement, then turns repayments into reclaim notes. It is the atomicity
owner across GratisPool, Oracle, Credis, external asset contracts and VaultProvider.
It does not own any of those modules' state.

## Decision

### Request Credis

`requestCredis` derives the borrower from a nonzero bundle account and performs:

1. validate nonzero asset/bundle and a Credis-eligible denomination;
2. reject a bundle with any overdue Credis position at canonical block time;
3. verify the GratisPool spend proof bound to the bundle, action, chain and zero
   context nonce, consuming the nullifier;
4. convert the denomination's 18-decimal Gratis amount into six-decimal stable
   amount using the pinned `COEN/0xUSD` Oracle rate and explicit decimal gap;
5. staticcall the selected asset's `isoCode()` and snapshot its refinancing rate;
6. create the Credis position using the nullifier as unique identity input;
7. persist the original denomination for reclaim derivation; and
8. withdraw exactly the position asset/amount through VaultProvider into the bundle.

The caller cannot redirect a copied proof because receiver binding uses the bundle.
All steps and the success event are one EVM rollback domain.

### Pay next Anadosis

Only the position's bundle account may pay. Before mutation the factory validates
position, next installment, asset, amount and nonzero reclaim commitment. It then:

1. advances the Credis next installment at canonical time;
2. pulls the exact recorded asset/amount from the caller;
3. approves and deposits it through VaultProvider; and
4. appends the caller-supplied reclaim commitment at the denomination one decade
   below the original pledge denomination.

Failure at any external call or commitment append rolls back cursor/debt changes.

## Cross-module invariants

- One consumed pledge nullifier opens at most one position.
- Position terms use the exact asset/currency/rate and amount delivered.
- Bundle token increase equals recorded principal under the supported token policy.
- Each successful payment advances one installment, deposits its exact debt amount
  and creates one reclaim right for its exact collateral slice.
- Live unreclaimed position collateral is backed by pledged Gratis escrow.
- Reclaim notes cannot be redirected, duplicated or created with an unverifiable
  denomination.

The last two are intended invariants that current implementation does not yet prove.

## Failure, replay and external trust

User/proof/authorization/rate/liquidity errors revert. Oracle values and asset ISO
are snapshotted for later determinism. ERC-20 and VaultProvider are adversarial
external-call boundaries: return data, actual balance deltas and asset identity must
be validated according to supported-token policy.

Pool nullifier, position id and installment cursor are replay guards. A node restart
uses canonical EVM state; no local cache may authorize a request/payment.

## Compatibility and evidence

Action tags, proof inputs, denomination ladder, ISO ABI, decimal scales, conversion
formula, position identity and reclaim rule are consensus/proof formats. Inspected
both runtime commands, ABI dispatch, Oracle/staticcall seams and tests. No full
production-interface credit lifecycle or failure-injection matrix exists.

## Consequences

CredisFactory presents two business commands while hiding proof/pricing/vault
choreography. Credis and VaultProvider remain separately auditable state owners.

## Rejected alternatives

- **Let caller supply rate/currency:** obligations become manipulable.
- **Create position after transferring liquidity without rollback:** failed state
  persistence could make an untracked loan.
- **Use transaction sender instead of bound bundle blindly:** smart-account credit
  ownership would be wrong.
- **Accept opaque reclaim forever:** valid repayments can strand collateral.

## Open questions and technical debt

1. Opening a position consumes a pool nullifier but does not visibly reserve or
   decrement per-account Gratis pledge accounting. Add a position-to-escrow
   reservation and prove aggregate backing.
2. Reclaim commitment denomination is opaque and can be wrong yet accepted. Add a
   verifiable denomination-bound insertion proof.
3. Define relationship between transaction caller and bundle account; `_caller` is
   currently unused on request, so any relayer can submit a bundle-bound proof.
4. Decide and enforce early-payment policy imported from ADR-C-CRD-001.
5. Use actual ERC-20 balance deltas or explicitly reject fee-on-transfer/rebasing
   assets for disbursement and repayment.
6. Validate that asset ISO, Oracle settlement pair, VaultProvider vault asset and
   token decimals describe the same economic currency.
7. Hard-coded six-decimal conversion and `COEN/0xUSD` symbol require a versioned
   multi-currency/decimal design.
8. Add failure injection after nullifier consumption, position creation,
   denomination write, vault withdrawal, installment advance, token pull/deposit
   and reclaim insertion.
9. Add ABI-level proof replay/front-running/redirection and restart tests matching
   PFS-003.
10. Define allowance reset/nonstandard ERC-20 safe-call policy.
11. The factory stores `position_denom` separately from Credis. Prove one-to-one
    closure and decide cleanup/retention on completed positions.
12. Add maximum loan, rate, multiplication and decimal-conversion bounds.
13. Define behavior when Oracle data changes between mempool admission and block
    execution; execution snapshot is authority.
14. Production deployment must structurally prove CredisFactory is registered as
    the correct VaultProvider source/target type.
