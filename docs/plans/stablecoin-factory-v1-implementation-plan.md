# Stablecoin Factory V1 implementation plan

- **Status:** Proposed
- **Date:** 2026-07-27
- **Primary ADRs:** ADR-C-TOK-003, ADR-C-TOK-004, ADR-C-TOK-005
- **Imported seams:** ADR-B-EVM-002 through ADR-B-EVM-005,
  ADR-S-GOV-002, ADR-S-GOV-003

## Outcome

Implement a separately addressed Rust-native ERC-20 stablecoin standard, a governed
Factory and a shared bounded Policy Registry. Prospective issuers submit a canonical
bonded Vote proposal. Successful validator quorum atomically initializes one token,
its marker code and permanent Factory indexes; every other terminal outcome releases
the reservation and burns the escrowed native COEN bond.

Factory approval means protocol admission only. V1 does not implement reserve
proofs, price stability, issuer endorsement, fee-asset eligibility, payment lanes,
ERC-3009, ERC-7802, cross-chain mint/burn, logo/metadata URI or initial minting.

## Preconditions and unresolved constants

Implementation must not name an activation fork until all four values below are
reviewed together:

1. `STABLECOIN_FACTORY_ADDRESS`;
2. `STABLECOIN_POLICY_REGISTRY_ADDRESS`;
3. the two-byte dynamic token prefix and exact marker bytecode; and
4. the fixed native COEN proposal bond.

Addresses/prefix require a collision scan across genesis allocation, Ethereum
precompiles, every Outbe fixed/reserved address and planned dynamic namespaces. The
selected class must be reserved from genesis (including a contract-creation guard),
not activated later over potentially occupied state. Bond size requires an economics
review; zero and runtime-configurable values are not allowed. Public-pending,
membership-batch/page caps and gas schedules require benchmarks.

## Dependency graph

```text
canonical ABI + address derivation vectors
        |
        +--> EVM reserved-address-class dispatch
        |
        +--> Policy Registry ----+
        |                         |
        +--> Stablecoin ledger <--+
        |                         |
        +--> Factory registry/initializer
                                  |
Vote target admission + bond -----+
                                  |
executor/manifest/fork wiring + production-interface tests
```

The EVM callee-address seam and Policy Registry can proceed in parallel after the
ABI/address vectors are frozen. Factory execution depends on both Policy and ledger.
Vote integration depends on the Factory proposal/reservation API.

## Phase 0 — freeze protocol artifacts

### 0.1 Canonical Solidity interfaces

Create:

- `contracts/precompiles/src/IStablecoin.sol`;
- `contracts/precompiles/src/IStablecoinFactory.sol`; and
- `contracts/precompiles/src/IStablecoinPolicyRegistry.sol`.

Freeze function selectors, typed errors, event signatures/order, role ids,
ERC-165/7943 id `0x3edbb4c4`, EIP-2612 type hashes and Tempo-compatible memo events.
Generate ABI selector tests before Rust dispatch exists.

**Acceptance:** independent Alloy/Solidity encoding produces the same selectors,
event topics and EIP-712 digests as checked-in golden vectors.

### 0.2 Canonical proposal and address vectors

Add golden vectors for:

- exact V1 JSON bytes and every rejected non-canonical form;
- ISO/name/ticker/decimals/cap/policy bounds;
- `tokenId` and two-byte-prefix address derivation for multiple chain ids, issuers
  and tickers; and
- deliberate 144-bit address-tail collision injection.

Keep derivation in one primitives module used by Factory, CLI and tests. Do not copy
the formula into EVM routing. Extend the generic Vote target contract to pass original
payload bytes, proposer and attached-value context; canonical validation must not
round-trip through `serde_json::Value`.

**Acceptance:** Rust and a small independent test-vector implementation agree
byte-for-byte; changing field order or domain separator fails tests.

## Phase 1 — extend EVM routing safely

### 1.1 Pre-survey and characterization

Before touching dispatch or `StorageHandle::set_code`, run the StorageHandle
ownership survey required by `AGENTS.md` section 8.1. Add characterization tests for
all current exact-address lookups, enumeration, warm addresses, marker preservation,
top-level/nested calls and Ethereum fallback.

**Acceptance:** tests expose the existing duplicate lookup/enumeration tables and
preserve current behavior before refactoring.

### 1.2 One manifest with exact and class routes

Refactor `crates/blockchain/evm/src/precompiles.rs` into one versioned manifest whose
route is either an exact address or a protocol-reserved address class. Dynamic class
dispatch receives the actual callee address; fixed handlers use an adapter and do not
all change signatures. Generate lookup, enumeration and conformance from the same
manifest.

Resolution order is exact Ethereum/Outbe address first, then non-overlapping reserved
class. Reserve the class from genesis: reject CREATE/CREATE2 results in it and make a
matching but unregistered address fail closed instead of executing ordinary bytecode.
Prefix/class overlap is a compile-time or startup-fatal error.

**Acceptance:** every old fixed entry remains identical; two registered dynamic
addresses use isolated storage; an unregistered prefix address reverts; nested and
top-level calls observe the same callee.

### 1.3 Hook-provider code mutation, marker and receipts

Vote finalization currently runs in the atomic pre-execution hook batch through
`DirectStorageProvider`, which rejects `set_code`. Add journaled code mutation,
include code changes in the hook change set sent to the parallel state-root task and
make balance credit checked. Add Factory to the `HookEvents` receipt whitelist.

A successfully installed nonempty marker already prevents EIP-161 pruning. Do not
scan Factory at block end or treat an allowlist as dynamic-address discovery. Verify
exact marker code during dispatch and prove `set_code` rolls back with failed
creation.

**Acceptance:** created token code survives state clearing and is included in state
root; failed creation leaves no code; arbitrary prefix code cannot be created;
`StablecoinCreated` appears in the mandatory `HookEvents` receipt; forced native
surplus does not corrupt bond or token accounting.

## Phase 2 — implement shared Policy Registry

Create workspace crate `outbe-stablecoinpolicy` under
`crates/core/stablecoinpolicy/` with:

```text
src/schema.rs      immutable descriptors and storage layout
src/state.rs       member/admin/index mutation helpers
src/runtime.rs     create/update/query use cases
src/precompile.rs  ABI dispatch only
src/errors.rs      typed module errors
src/tests.rs or tests/{state,e2e}.rs
```

Implement stable policy descriptors 0 through 4, built-in ids 0/1, checked monotonic
ids, Whitelist, Blacklist and non-recursive Directional policies; permissionless
creation; bounded duplicate-free membership batches; two-step admin transfer; exact
`isMember` semantics; O(1) non-reverting authorization views; and typed internal query
API. Keep state access explicit through scoped `StorageHandle`.

**Acceptance:** tests cover built-ins, unknown ids, every policy type/direction,
unauthorized edits, duplicate/oversized batches, rollback, admin transfer, shared
policy use and fixed read-count bounds. No runtime `read_all()` is used.

## Phase 3 — implement the stablecoin ledger

Create workspace crate `outbe-stablecoin` under `crates/core/stablecoin/` with:

```text
src/schema.rs      metadata, balances, allowances, roles, freeze and versions
src/state.rs       local ledger/index helpers
src/runtime.rs     transfer/mint/burn/admin/compliance orchestration
src/precompile.rs  canonical ABI routing only
src/errors.rs      typed errors
src/tests/{common,erc20,permit,roles,compliance,migration}.rs
```

### 3.1 ERC-20 and supply

Implement immutable metadata, zero initial supply, checked balances/supply,
allowances including infinite allowance, cap management and standard events.
Reject unexpected native value. Cover `decimals = 0` and `18`, `U256::MAX`, cap
reduction to current supply, zero public transfers and rejected zero privileged
operations.

### 3.2 Roles, pause and admin transfer

Implement fixed role ids, operational memberships, initial issuer grants, two-step
non-renounceable admin transfer and asymmetric pause (`GUARDIAN` pauses, `ADMIN`
unpauses). Keep cap/policy/freeze/role/admin recovery paths available while paused.

### 3.3 Policy and ERC-7943

Bind only existing policies and implement ERC-7943 exactly, including non-reverting
views, absolute frozen amount above balance, recipient check on forced transfer and
`Frozen -> Transfer -> ForcedTransfer` ordering. Pin vectors for
`U = B - min(B,F)`, `C = max(0,A-U)`, `F' = F-C`, including `F < B`, `F == B`,
`F > B`, mixed consumption, full movement and issuer burn. Ordinary self-transfer
changes no freeze; forced self-transfer is rejected. Apply the selected burn and
frozen-allowance semantics.

### 3.4 EIP-2612 and memo variants

Implement domain separator/nonce/deadline/signature checks and all six `bytes32` memo
variants. Memo is event-only. Add replay, malleability, wrong-chain, wrong-token,
expired-deadline and failed-call nonce tests.

### 3.5 Global hard-fork behavior and lazy migration

Add one canonical Outbe protocol-version resolver and propagate its exact-block value
through proposer, validator, nested-call and RPC paths; Ethereum `SpecId` and token
schema are not substitutes. Dispatch by that value, not per-token implementation.
Static views decode supported old schemas without mutation. The first mutating call
may perform an O(1) atomic lazy migration. Add a test-only next version proving
read compatibility, migration rollback and no registry scan. Retired selectors must
revert.

**Acceptance:** property tests preserve `sum(distributed balances) == totalSupply`
within generated participants, cap, allowance and frozen invariants across arbitrary
successful/failing sequences.

## Phase 4 — implement Factory identity and registry

Create complex workspace crate `outbe-stablecoinfactory` under
`crates/core/stablecoinfactory/`:

```text
README.md           tier, Vote hook and cross-module dependencies
src/schema.rs       permanent indexes and pending reservations
src/state.rs        index/reservation consistency helpers
src/runtime.rs      validation, prediction and atomic initialization
src/precompile.rs   view ABI only
src/api.rs          typed Vote target interface
src/vote_target.rs  admission/reserve/execute/terminal adapter
src/errors.rs       typed errors
src/tests/{common,state,creation,vote_e2e}.rs
```

Implement canonical parsing from original payload bytes by typed decode plus
byte-identical re-encode; current ISO numeric table; full token id/address prediction;
pending and permanent indexes; marker installation; ledger initialization; policy
existence check; permanent views and `StablecoinCreated`.

Do not expose public creation or use Factory `read_all()`. A pre-existing native
balance at a predicted address must neither block creation nor count as backing.

**Acceptance:** all forward/reverse indexes agree after creation; collision,
duplicate and malformed proposals leave no reservation/token/code; terminal release
permits a corrected resubmission; successful registration cannot be deleted.

## Phase 5 — extend Vote with target admission and bond escrow

Update `crates/system/vote` without importing Factory storage types into Vote core.
Introduce a compile-time target contract with distinct methods for pure validation,
reservation, approved execution and terminal cleanup. Its admission enum includes
`ActiveValidatorOnly` and exact `PublicBonded { amount }`.

Extend proposal state with bond amount and explicit unsettled/settled state. Make the
proposal creation ABI payable while requiring zero value for every non-bonded target.
The attached value remains native balance at `VOTE_ADDRESS`.

During tally:

1. compute the existing current-ACTIVE-set quorum;
2. run approved handler in a nested storage checkpoint;
3. on success consume reservation, mark Approved, refund bond and emit events;
4. on quorum failure release reservation, mark Expired and burn bond;
5. on typed recoverable domain rejection roll back handler writes, release
   reservation, mark Rejected and burn bond; or
6. on OOG, unsupported provider capability, corruption, impossible schema/invariant
   or settlement/cleanup failure, propagate fatal and roll the complete hook batch
   back to Pending without settling the liability.

A terminal proposal is replay-inert. Add global and per-identity public pending caps
so one address cannot consume the global 64-slot pending index.

**Acceptance:** native-balance accounting proves
`VOTE_ADDRESS balance >= sum(unsettled bond liabilities)` after every transition;
forced surplus is never refunded. Injected failure at each step leaves no partial
token, index, marker, status or double settlement.
Existing Update/Governance proposal authority and zero-value behavior remain
unchanged.

## Phase 6 — wire the activation fork

Add fixed Factory/Policy addresses to primitives, reserve the two-byte class and its
CREATE/CREATE2 guard in genesis execution rules, allocate fixed markers and update the
versioned EVM/Vote manifests. Register the Factory Vote target in
`crates/blockchain/evm/src/handlers.rs`. Bind Factory/token activation through normal
hard-fork configuration while keeping the namespace reserved from genesis; no
environment variable or runtime plugin may enable it.

Update CLI/SDK proposal tooling to:

- create policies before proposals;
- default policy id to 1 but serialize it explicitly;
- default decimals to 6 but serialize it explicitly;
- show raw and decimals-adjusted cap;
- predict and display token id/address;
- attach the exact bond; and
- warn that approval is not a reserve, price or fee-asset endorsement.

**Acceptance:** the namespace is reserved/fail-closed from genesis while token and
Factory selectors remain inactive before their fork; all validators derive identical
routes and constants at activation; RPC simulation and canonical execution produce
identical results.

## Phase 7 — verification and documentation closure

### Targeted commands

Run diagnostics before builds, then at minimum:

```text
cargo nextest run -p outbe-stablecoinpolicy
cargo nextest run -p outbe-stablecoin
cargo nextest run -p outbe-stablecoinfactory
cargo nextest run -p outbe-vote
cargo nextest run -p outbe-evm
cargo nextest run -p outbe-primitives --test trybuild
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
```

Run execution-level parity tests with identical parent/header/state on proposer and
validator paths. Run localnet smoke only after the activation/genesis patch is
complete.

### Required adversarial matrix

Cover malformed/duplicate raw JSON, invalid ISO/ticker/name/decimals/cap, nonexistent
policy, reserved-class CREATE/CREATE2, address collision, duplicate reservation,
proposal spam caps, validator-set change at tally, recoverable versus fatal handler
failure, forced Vote surplus, refund/burn insufficiency, terminal replay, marker/code
state-root rollback, policy update affecting multiple tokens, every frozen formula
boundary, forced transfer event order, pause recovery, admin loss assumptions, permit
replay, historical static views, reentrancy, nested calls and OOG at each mutation
boundary.

### Contract documentation

Before activation, update:

- root `README.md` operator/protocol contract;
- ADR-S-GOV-002 and ADR-B-EVM-002 with implemented evidence;
- the ADR index/status and coverage ledger;
- module README for the complex Factory;
- any relevant `audit_*.md` implementation deviations; and
- CLI help and canonical ABI artifacts.

Do not mark any ADR Implemented until production-path evidence, exact fork constants
and verification results are recorded.

## Suggested reviewable commit sequence

1. ABI, raw canonical payload and address vectors only.
2. Inert generic Vote target admission/raw-payload/reservation/outcome types.
3. Address derivation primitives and collision tests.
4. Genesis-reserved EVM exact/class manifest and CREATE/CREATE2 guard.
5. Direct hook-provider code journaling, checked balance credit and HookEvents tests.
6. Policy Registry schema/state, then precompile.
7. Canonical protocol-version resolver and propagation.
8. Stablecoin ERC-20 core.
9. Stablecoin roles/pause/cap.
10. Stablecoin Policy/ERC-7943.
11. Stablecoin EIP-2612/memos/read-compatible migrations.
12. Factory registry and views.
13. Factory initializer and adapter against the already-landed Vote target API.
14. Vote bond escrow/terminal settlement and typed failure classification.
15. Fork/handler activation wiring.
16. CLI/SDK tooling.
17. Integration, parity, adversarial and localnet tests.
18. README/audit/ADR evidence closure.

Each commit must keep all pre-existing targets compiling and must not combine a
schema change with unrelated refactoring.

## Definition of done

- Each admitted proposal has exactly one terminal token-or-no-token outcome and one
  terminal bond settlement.
- Every registered token is discoverable through all consistent Factory indexes,
  has exact marker code and isolated address-scoped state.
- ERC-20, EIP-2612 and ERC-7943 production interfaces pass golden and adversarial
  tests.
- Policy evaluation is single-source, non-recursive and bounded.
- Proposer and validator execution agree on state root, logs, receipt, gas and native
  balance deltas.
- Pre-fork behavior is unchanged and post-fork activation is hard-fork deterministic.
- README states clearly that admission is not backing, stability, redeemability or
  fee eligibility.
- No unresolved address, prefix, bond, gas or batch-cap placeholder remains in code.
