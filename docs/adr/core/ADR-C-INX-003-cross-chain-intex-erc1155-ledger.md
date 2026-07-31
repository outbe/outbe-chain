# ADR-C-INX-003: Cross-chain Intex ERC-1155 preserves series and holder conservation

- **Status:** Proposed; current Solidity implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `contracts/intex/src/shared/IntexNFT1155.sol`
- **Depends on:** ADR-C-INX-001, ADR-C-INX-002, ADR-B-XCH-001
- **Related flow:** PFS-004

## Context

The Rust Intex precompile owns native protocol series. A separate upgradeable
ERC-1155 represents issued and settled Intex on Outbe and the target chain, tracks
series metadata, holders, supply and auction wins, and grants relayer, settlement
and Promis authorities. It is an independent ledger, not merely an ABI wrapper.

## Decision

Each nonzero `seriesId` has exactly two canonical token ids: issued
`uint256(seriesId)` and settled `keccak256("SETTLED", seriesId)`. `createSeries`
commits immutable identity/economic parameters and supply cap once. Relayer-authorized
mint/qualification/call/expiry/cross-chain operations, settlement-authorized
issued-to-settled conversion and Promis-authorized settled burn are explicit commands.
Enumerable owner/holder indexes are derived state updated atomically with ERC-1155
balances and supply.

The series FSM is `Absent -> Issued -> Qualified -> Called`; expiry is either made a
real persisted terminal state or is defined strictly as an event plus deterministic
deadline predicate. Issued transferability and settlement guards derive from this one
state. Upgrade and role changes are governed protocol activations, not ordinary admin
maintenance.

## Authoritative interfaces

- `createSeries`, `mint`, `markQualified`, `markCalled`, `expireSeries` own lifecycle.
- `settle` atomically burns issued from one holder and mints settled to another.
- `burnSettled` is the Promis consumption seam.
- `crosschainBurn/crosschainMint` are available only to the paired bridge profile.
- `readData`, balances, supply, owner-series and series-holder pagination are queries.

## Invariants

- Series identity is immutable and created once; token-id classification is total and
  collision-free for all admitted series ids.
- Issued plus bridged supply never exceeds series cap; settled supply arises only from
  equal issued burn and falls only through authorized burn or bridge transfer.
- Every nonzero balance appears exactly once in each required enumerable index and zero
  balances appear in none.
- Soulbound settled tokens cannot use ordinary transfer routes.
- Lifecycle transitions are monotonic and terminal behavior cannot be reopened by role
  rotation, bridge replay or upgrade.

## Atomicity, replay and failure

Every command updates primary balances, total supply, metadata and indexes in one EVM
transaction. Duplicate create/transition fails or returns an explicit idempotent result;
duplicate bridge mint is rejected by the bridge inbox before ledger mutation. Batched
updates plan and validate all items before the first write. Pagination is snapshot-local
and makes no stability promise across intervening transactions.

## Determinism and bounds

Supply, quantity and auction-win widths are checked before narrowing. Holder/series
enumeration and `expireSeries(limit)` require bounded cursors. No command copies an
unbounded holder set. Metadata strings have an operational cap or immutable content hash.

## Compatibility, trust and activation

Storage slot, role ids, token-id formulas, enum values, series schema and bridge ABI are
one versioned compatibility profile. UUPS upgrades preserve and validate storage, roles,
immutable dependencies and invariants through a migration/reinitializer manifest.

## Production-interface verification evidence

Inspected production entrypoints include initialization/upgrade, series creation and
transitions, mint, settle, settled burn, cross-chain burn/mint, `_update` index hooks and
all pagination reads. Foundry tests cover lifecycle and bridge integration, but no catalog
evidence yet proves upgrade storage compatibility and all randomized index invariants.

## Consequences

Remote representation has a clear ledger authority distinct from Rust Intex and bridge
transport. Reconciliation across those authorities belongs to PFS-004.

## Rejected alternatives

- Treating ERC-1155 events as the only series state is rejected.
- Letting routers write balances directly is rejected.
- Combining ledger and bridge into one upgrade authority is rejected.

## Open questions and technical debt

- **Critical:** prove total issued/settled supply and owner/holder indexes remain
  consistent under every single/batch transfer, burn, mint, settlement and failed hook.
- Expiry is documented as an event rather than an enum state; specify replay/query
  semantics and prevent repeated or partial mass expiry.
- Audit token-id collision/classification for arbitrary ERC-1155 ids and reject unknown
  ids in every mutation route.
- Bound `expireSeries`, holder snapshots, pagination and metadata; add randomized model
  tests for index/supply conservation.
- Replace immediate `DEFAULT_ADMIN_ROLE` upgrades/role grants with governed delay,
  storage-layout validation and incident recovery.
- Prove Rust Intex and both ERC-1155 deployments cannot independently create conflicting
  series metadata or supply.

