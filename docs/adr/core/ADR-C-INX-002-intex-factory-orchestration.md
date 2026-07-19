# ADR-C-INX-002: IntexFactory owns issuance, qualification, settlement and Promis mining

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Intex protocol maintainers
- **Scope:** `crates/core/intexfactory`, its begin-block hooks and external
  ERC-1155, OriginRouter, VaultProvider and token seams
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-S-CYC-001, ADR-S-ORC-001, ADR-C-PRM-001, ADR-C-PRM-002, ADR-C-VLT-001, ADR-C-INX-001
- **Related:** ADR-C-LYS-001, ADR-B-CRY-001 and PFS-004
- **Supersedes:** IntexFactory portions of former broad pre-space Gratis/economic aggregate (previously numbered 030)

## Context

IntexFactory orchestrates the complete Intex business interface while Intex owns
series/progress records. It derives issuance parameters, coordinates local and
cross-chain representations, scans Oracle-indexed candidates, accepts authorized
settlement into reserves, distributes proceeds and consumes Settled units to mint
Promis.

## Decision

### Issuance and autonomous lifecycle

Only the authorized Desis/internal issuance seam may issue. Factory derives floor
and call prices with checked fixed-point arithmetic, creates the Intex ledger
record, creates the corresponding local ERC-1155 series, sends issuance instructions
through the pinned OriginRouter and enrolls the series in the unqualified floor-bin
index. These effects share one source-chain transaction.

Begin-block qualification scans only due price bins and transitions eligible Issued
series to Qualified consistently in both Rust and ERC-1155 representations. The
scheduled call scan uses canonical WorldwideDay/Oracle history and a per-series
window/threshold to transition eligible Issued or Qualified series to Called and
remove/update indexes. Each candidate is checkpoint-isolated, but deterministic
errors and retry/skip policy must be explicit.

### Settlement

Settlement is allowed in Qualified, or in Called no later than
`called_at + call_period`. Caller is the Intex holder or its nonzero per-series
authorized settler. Amount must not exceed holder's Issued ERC-1155 balance.

Factory derives exact payment from immutable entry price and Promis load, pulls the
settler's payment asset, measures actual received balance delta, approves and
deposits it through VaultProvider, requires nonzero shares, then tells ERC-1155 to
burn holder Issued and mint settler soulbound Settled units. Settle count and event
commit last in the same EVM frame.

### Promis mining and proceeds

`minePromis` verifies holder's Settled balance and sequence-bound PoW, increments
the per-series/holder sequence, burns Settled units and mints exactly
`promis_load * amount` through PromisFactory.

Only the pinned OriginRouter may deliver nonzero native proceeds. Factory opens a
distribution over stored contributors. Begin-block drain snapshots active ids,
advances each in a checkpoint and pays bounded chunks proportionally; the final
contributor receives division remainder so payouts equal delivery exactly.

## State, authority and invariants

Factory-owned state includes configuration, qualification/call indexes, authorized
settlers, settle counters and mining sequences. Required closure:

- each nonterminal candidate appears in exactly the correct lifecycle price/call
  index and no terminal/ineligible series remains;
- Rust and ERC-1155 series identity and lifecycle agree;
- issued quantities never exceed declared cap;
- settlement received asset delta is deposited and Issued burned equals Settled
  minted;
- mine sequence advances iff equal Settled units are burned and exact Promis minted;
- proceeds paid plus remaining progress equals received native amount;
- contributor payout is deterministic and no delivery pays twice.

## Atomicity, replay and failure

User issuance/settlement/mining and inbound proceeds are EVM transactions. Candidate
hook processing and each distribution chunk use explicit checkpoints. Bridge
delivery has a separate cross-chain finality/replay boundary governed by ADR-B-CRY-001.

Series id, index membership, settlement balances, mining sequence and distribution
progress form replay guards. Invalid user data reverts. Broken indexes, Rust/NFT
state divergence, payout underflow or impossible progress are invariant failures and
must not be logged-and-skipped indefinitely without escalation.

## Security, compatibility and evidence

Desis, OriginRouter, ERC-1155, VaultProvider and PromisFactory addresses/roles are
privileged deployment wiring. Asset/currency binding, Oracle snapshots, bridge
sender/domain and PoW format are security boundaries.

Config constants, fixed-point scales, price bins, call algorithm, token ids, PoW
preimage/difficulty, distribution chunking/dust and external ABIs require activation
and reference vectors.

Inspected issuance, settlement, mining, proceeds/distribution runtime and lifecycle
scan paths. Unit tests exist for major paths, but no production e2e closes PFS-004,
cross-chain replay, real token/vault behavior or index corruption.

## Consequences

IntexFactory is the single task-oriented interface while Intex retains state
authority. The broad cross-module story lives in PFS-004 rather than weakening the
module-level module audit profile.

## Rejected alternatives

- **Let callers choose derived prices or settlement asset:** pricing/currency can be
  manipulated.
- **Mint Promis directly:** Fidelity coupling in PromisFactory is bypassed.
- **Pay all contributors in inbound bridge transaction:** unbounded delivery gas can
  brick the bridge.
- **Swallow index divergence forever:** stuck obligations become invisible.

## Open questions and technical debt

1. Settlement asset is selected as `VaultProvider.assetAt(0)`. Bind each series to
   its actual issuance/reference settlement asset; enumeration order is not identity.
2. Prove local Rust series, local ERC-1155 and remote series creation are idempotent
   and recoverable across bridge failure/finality without duplicates.
3. Define exact authorization of internal issuance; add structural caller and
   deployment-role tests for Desis and router/NFT addresses.
4. Reconcile Rust and ERC-1155 lifecycle/expiry behavior with a cross-contract
   generated model.
5. Qualification/call scans need explicit maximum candidates/work per block,
   deterministic continuation cursor and fairness.
6. Candidate checkpoint errors are logged and skipped/retried. Classify transient
   versus invariant failures and add bounded alert/halt policy.
7. `assetAt(0)` and single-currency assumptions block safe multi-currency support.
8. Fee-on-transfer settlement measures received delta but economic policy for a
   payer receiving fewer effective units is not explicitly accepted; validate vault
   shares/balance delta too.
9. Approvals need strict ERC-20 safe-call, reset and nonstandard-token policy.
10. Distribution cursor uses saturation and final share subtracts `paid`; corrupted
    progress can mask/underflow. Use checked invariant validation.
11. A distribution with corrupt contributor state retries forever every begin block.
    Add durable diagnostics and approved repair/escrow policy without redirection.
12. Define duplicate/repeated proceeds semantics. Contributor cleanup after first
    payout currently prevents safe later deliveries.
13. Pin PoW difficulty/preimage with independent vectors and activation rules;
    prove sequence increment rolls back on NFT/Promis failures.
14. Add PFS-004 e2e covering real bridge, Oracle scan, dual-wallet settlement,
    VaultProvider, mining, replay, failures and restart.
15. Prove native IntexFactory balance always equals aggregate unfinished
    distributions and parked/unclaimed obligations.
