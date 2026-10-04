# Canonical Intex bid units

The signed bid is:

```text
RevealBid(uint32 worldwideDay,address bidder,uint16 units,uint32 bidRate,uint16 issuanceCurrency,uint16 referenceCurrency)
```

The EIP-712 domain remains `IntexAuction`, version `1`, with the target chain ID
and auction address. `units` is a whole Intex count, bounded by `uint16`;
`bidRate` is fixed point at 1e6. Renaming the signed field changes the digest.
The function selector, tuple ordering, types, escrow amount formula and storage
layout stay the same. The MCP commit and reveal tools both require `units`. The manual
`generate-commit-hash` task uses the same six signed fields, including both
currency codes, and accepts `--units`.

## Rollout boundary

Upgrade the auction implementation and MCP client together only after auctions
using the old signed schema have finished their reveal and bond-claim windows.
Do not switch an auction with outstanding legacy commits to this implementation.
Old `quantity` signatures cannot reveal under the new schema; they are deliberately
not accepted as an alternate signed type. This PR does not perform deployment.
A rejected legacy reveal preserves its commit and has no escrow or reveal effect.

The shared vector in `test/foundry/IntexAuction.signature.t.sol` and
`mcp/src/intex/bid.test.ts` fixes the digest, deterministic signature and commit
hash. The Solidity suite exercises actual new-schema commit/reveal and legacy
rejection, with chain/address replay and signature malleability checks.

## Limit and Allocation terminology audit

| Economic value | Rust authority | Receipt or event | Solidity and client surface |
| --- | --- | --- | --- |
| Lysis Limit: frozen issuance ceiling | `LysisApplyPlanV1.request_limit_split().lysis_limit_minor` | `RequestLimitSplitReceiptV1.lysis_limit_minor`; `MetadosisExecuted.lysisLimitMinor` | `IMetadosis` generated ABI |
| Lysis Allocation: actual assigned Gratis load | `NodGenerationApplyV1.lysis_allocation_minor()` | `NodBatchReceiptV1.lysis_allocation_minor`; `MetadosisExecuted.lysisAllocationMinor` | `INod` certified generation `lysisAllocationMinor` and generated ABI |
| Unused Lysis Limit | `CarryOverApplyV1.credited_unused_lysis_limit_minor()` | `CarryOverReceiptV1.credited_unused_lysis_limit_minor`; `MetadosisExecuted.unusedLysisLimitMinor` | `IMetadosis` generated ABI |
| Desis Limit: reserved ceiling before clearing | `pending_desis_limit_minor`; derived whole-count `pending_desis_limit_units` | `RequestLimitSplitReceiptV1.desis_limit_minor`; `DesisAllocationRecorded.desisLimitMinor` | `IDesis` generated ABI |
| Desis Allocation: actually issued units times PROMIS load | `desis_allocation_minor` in clearing | `DesisAllocationRecorded.desisAllocationMinor` | `IDesis` generated ABI |
| Unused Desis Limit | checked `desis_limit_minor - desis_allocation_minor` | `UnusedDesisLimitReported.unusedDesisLimitMinor` | `IDesis` generated ABI |
| Loads and costs | `gratis_load_minor`, `promis_load_minor`, `settlement_cost_minor` | product data/events carry `*LoadMinor` and `*CostMinor` | protocol amounts; payment quotes separately use asset decimals |
| Intex whole counts | `issued_units`, selected settlement/mining `units` | `AuctionCleared.issuedUnits`, series and factory `units` | signed `RevealBid.units`; MCP tool `units` |

`crates/core/lysis/src/activation_v1/receipts.rs::verify_receipts` checks actual
Lysis Allocation plus unused Lysis Limit equals the frozen Lysis Limit.
`crates/core/desis/src/runtime.rs` computes Allocation from issued whole units
and uses checked subtraction before emitting or returning unused capacity.
The names represent different values and already agree across these boundaries.
The MCP ABI registry imports whole generated precompile ABIs; it has no separate
hand-written Limit/Allocation decoder to rename.

Existing storage/wire aliases `minIntexBidQuantity` and `SubmittedBidData.intexQuantity`
remain whole-count fields. They are not members of the signed RevealBid type;
renaming these aliases across bridge records is outside this signed-artifact change.
Nod's internal `issue_nod` remains the logical issuance operation; production
certified materialization remains its bounded carrier, with no free public issuer
added. Oracle snapshot names remain duration-neutral.
