# Stablecoin Factory V1 implementation plan

- **Status:** In implementation
- **Date:** 2026-07-28
- **Primary ADRs:** ADR-C-TOK-003, ADR-C-TOK-004, ADR-C-TOK-005
- **Imported seams:** ADR-B-EVM-001 through ADR-B-EVM-005,
  ADR-S-GOV-002, ADR-S-GOV-003

## Outcome

Implement a separately addressed Rust-native ERC-20 stablecoin standard as a dynamic
stateful precompile address class, plus a fixed Factory precompile and a fixed shared
Policy Registry precompile.

A prospective issuer submits a canonical bonded Vote proposal. Successful validator
quorum atomically initializes one zero-supply token, installs its marker code, commits
permanent Factory indexes, emits a receipt-visible event and refunds the bond. Expired
proposals release reservations and burn the bond. If approved target execution
returns a typed `Error` outcome, its partial effects roll back and Vote records
`Error`; the bond and reservation triple remain unchanged for a future
validator-approved governance decision outside Stablecoin Factory V1. Outer
storage/provider/unsupported/fatal errors propagate and are not proposal outcomes.

Factory approval means protocol admission only. It is not evidence of backing,
redeemability, price stability, issuer solvency, fee-asset eligibility or payment-lane
eligibility.

## Fixed product decisions

| Decision | V1 contract |
| --- | --- |
| Runtime shape | One compiled Rust stablecoin precompile implementation; callee address selects isolated token storage |
| Factory/Policy | Fixed stateful Rust precompiles |
| Token namespace | Genesis-reserved two-byte address class, leaving 144 token-id hash bits |
| Creation | Vote-only; no public Factory `create` selector |
| Ticker | Globally unique across pending and permanent Factory state |
| Initial supply | Zero |
| Initial policy | Explicit existing `policyId`; tooling may default to `ALLOW_ALL = 1` |
| Initial cap | Explicit nonzero `U256`; no zero-as-unlimited sentinel |
| Bond | `1,000,000 COEN = 10^24 = 1000000000000000000000000` base units |
| Token standards | ERC-20, EIP-2612, ERC-165, ERC-7943, fixed `bytes32` event-only memo variants |
| Upgrades | One global hard-fork runtime; V1 implements schema 1 only, and a future schema upgrade owns its concrete migration |
| Fee/bridge scope | Fee assets, payment lanes, reserve proofs, ERC-3009 and ERC-7802 are outside V1 |

## Frozen protocol values

No implementation task may change or duplicate these values without reopening G0,
the owning ADR and its vectors:

1. Factory `0x...EE0F`, Policy Registry `0x...EE10`, dynamic prefix `0x53c0` and
   marker `0xef`;
2. activation protocol version `0.2` (raw `2`), schema version `1`, destructive
   fresh-genesis devnet/testnet applicability and unsupported mainnet activation;
3. EIP-712 domain version `"1"` (role ids are frozen by SCF-001);
4. public bonded caps 16 globally and one per proposer inside Vote's 64 total slots;
5. policy membership batch cap 64; Factory and Policy list page cap 100 with no
   sorting or ordering guarantee;
6. the token/Policy gas contract in `outbe_primitives::stablecoin_fork`, mirrored by
   `fork-manifest.json`; Factory creation has no separate gas ceiling; and
7. planned CLI-only tooling ownership under `bin/outbe-cli`, with no maintained SDK
   promise.

SCF-001 froze policy add/remove as typed `MembershipUnchanged`, the complete ABI
error/event/indexing surface and all six role ids. SCF-002 froze the token-id widths as
`u64` big-endian chain id plus `u8` ticker length and pinned the SIX ISO 4217 List One
snapshot published 2026-01-01. SCF-003 froze the namespace/reset contract; SCF-004
froze version, bounds, token/Policy gas, EIP-712 and tooling ownership.

## Task execution contract

Taskplane is not required. Before implementation, the coordinator treats each task
below as a fixed execution scope containing:

- stable task id and dependencies;
- goal and explicit non-goals;
- production files/crates allowed to change;
- concrete deliverables;
- measurable acceptance criteria;
- unit, property/golden, integration and E2E obligations that apply;
- exact verification commands;
- rollback/migration implications;
- independent-review requirement;
- exit evidence: changed files, command results, remaining risks and commit id.

Only the coordinating agent updates this plan. Implementation/review agents report
evidence to the coordinator and never edit this file, avoiding a shared-file merge
conflict across isolated worktrees. Repository/build/genesis validation belongs in
`xtask`; end-to-end product flows belong in `crates/testing/e2e-harness`. Do not add a
parallel standalone `scripts/tests` harness when either owner can express the check.

Default completion rules for every implementation task:

1. production code and its focused tests land together;
2. no `unwrap`, `expect`, panic, float economics, unbounded hot-path `read_all`,
   implicit storage context or consensus `HashMap` is introduced;
3. failures are typed and preserve journal/state/log atomicity;
4. proposer and validator semantics are not allowed to diverge;
5. `cargo fmt --all --check` and focused `cargo nextest` pass;
6. LSP/lens diagnostics for changed files contain no blocking issue;
7. consensus/storage/crypto tasks receive an independent read-only review;
8. user-visible behavior updates README/audit evidence in the owning closure task.

## Test evidence levels

| Level | Evidence |
| --- | --- |
| T0 | Characterization test proving current behavior before refactor |
| T1 | Unit/state-machine/layout tests in the owning crate |
| T2 | Property, fuzz, ABI/golden-vector and failure-injection tests |
| T3 | Cross-crate integration through real precompile/provider interfaces |
| T4 | Full `OutbeBlockExecutor` proposer/validator/fork-boundary parity |
| T5 | Stablecoin-specific four-validator localnet E2E with restart and RPC checks |

A later T4/T5 task does not excuse missing T1-T3 evidence in the behavior task that
introduced the invariant. T5 localnet/product E2E is executed only in Phase 7 after
implementation and T4 parity are complete. On machines without SGX it runs with
`--tee none`; SGX-specific attestation evidence is environment-gated and collected at
the release gate rather than blocking earlier implementation tasks.

## Execution ledger

Tasks not listed here are `Pending`. The coordinator updates status only after
reviewing the task's scoped diff and verification evidence.

| Task | Status | Branch/commit | Evidence / blocker |
| --- | --- | --- | --- |
| SCF-001 | Done | `c9c69720`, `d837e7f2` | Vote `Error`, Factory/Policy count+list ABI, typed enumeration error, page boundaries and Final ERC-7943 errors frozen; Forge 11/11, Alloy ABI vectors 4/4 and ABI export check pass |
| SCF-002 | Done | `c3106e5` | Primitives focused 10/10 and full 265/265; clippy/doc/fmt; independent codec review READY |
| SCF-003 | Done | `6e948d2`, `8e60aeb` | Rust 3/3; xtask 13/13; generated-genesis integration; clippy/release build; review READY |
| SCF-004 | Done | `c9c69720`, `d837e7f2` | Shared list page cap 100 and offset/final-page semantics frozen; Factory creation ceiling/gate removed; fork vectors 7/7 pass |
| SCF-081 | Done | `50ebcb2` | V1 ownership frozen as planned CLI-only; README/ADR disclaim maintained SDK support |
| SCF-G0 | Done | `c9c69720`, `d837e7f2`, `4c6b0c41`, `8ca8702b` | Four review blockers corrected; Forge 11/11, primitives 24/24, Vote+Update+Governance 138/138, CLI 206/206, EVM 247/247, clippy and ABI export check pass; repeat independent review READY |
| SCF-010 | Done | read-only | Exact AGENTS.md 8.1 surveys; trybuild 1/1; no long-lived runtime owner; storage review APPROVE |
| SCF-011 | Done | `713fdc15` | Exact 35-route/32-list drift, warm/contains, fallback, caller/callee and input-order baselines; EVM 240/240 pass (1 pre-existing skip); clippy/LSP clean |
| SCF-012 | Done | `a81c22e1` | Actual unsupported set_code, nested checkpoint/storage/balance/event rollback, account/change-set preservation, transfer underflow and current overflow wrap; primitives 279/279; clippy/LSP clean |
| SCF-013 | Done | `8c0eea17` | Baseline ACTIVE/PENDING behavior and zero-value paths characterized before SCF-026 changed ballots to ACTIVE-only; raw payload state/log, validator-change quorum, handler failure, replay and outer rollback snapshots covered |
| SCF-014 | Done | `67d6f2a2` | Behavioral H-1/H/H+1 snapshots: Update activates at begin-block H but history is exact-height sparse; canonical/build/validation stay chain-spec sourced; nested call currently accepts PUSH0 under London and Shanghai; RPC dispatch reflects caller-selected state; Update 56/56, EVM 244/244 (1 skipped), RPC 10/10; review APPROVE |
| SCF-020 | Review | `023a723d` | Compact 35-route declaration drives exact lookup/enumeration and reader adapter; existing empty shared-buffer behavior remains characterized until an explicit activation boundary; consolidated review deferred to SCF-G1 |
| SCF-026 | In progress | `4c6b0c41`, `8ca8702b` | Typed Applied/Error outcome, raw bytes and proposer/value/height/chain context, target-owned domain classification, outer technical Err propagation, nested rollback and ACTIVE-only ballots pass focused suites; exact PublicBonded admission remains inactive until Vote bond accounting |

Allowed statuses are `Pending`, `In progress`, `Blocked`, `Review` and `Done`. `Done`
requires the task's exit evidence and commit id; a gate becomes `Done` only after its
independent read-only review passes.

## Dependency overview

```text
SCF-G0 protocol lock
  ├── EVM/provider infrastructure ───────────────┐
  ├── raw Vote target infrastructure ───────────┤
  ├── Policy Registry ───────┐                  │
  └── Stablecoin schema ─────┴─> ledger runtime │
                                             ┌──┘
Policy + ledger + provider ──> Factory ──> Vote bond/finalization
                                             │
                                     production fork wiring
                                             │
                              full-block parity + localnet E2E
                                             │
                                    docs/release evidence
```

---

# Phase 0 — Protocol lock and golden artifacts

## SCF-001 — Freeze canonical Solidity interfaces

- **Goal:** Make public ABI bytes immutable before Rust implementation.
- **Scope/files:** create `contracts/precompiles/src/IStablecoin.sol`,
  `IStablecoinFactory.sol`, `IStablecoinPolicyRegistry.sol`, generated ABI exports and
  Solidity/Alloy vectors; update `IVote.sol` for Approved/Expired/Error proposal and
  bond views/events, plus the minimal Vote dispatch compatibility needed to keep those
  future views explicitly inactive before bond-accounting activation.
- **Depends on:** none.
- **Done when:** every function, custom error, event order/indexing, role id,
  Factory/Policy count+list selector, Final ERC-7943 error/id, EIP-2612 type hash and
  memo event has a checked-in vector.
- **Tests:** T1 Solidity compile; T2 Forge selector/topic/error/EIP-712 vectors and an
  independent Alloy encoder.
- **Verification:** scoped `forge fmt --check` for SCF-001 Solidity files; `forge
  test`; `cargo nextest run -p outbe-primitives --test stablecoin_abi_vectors`;
  `cargo nextest run -p outbe-vote`; `cargo check -p outbe-vote -p outbe-cli`.
- **Exit:** ABI review confirms that Factory exposes views only and token creation is
  not a public selector.

## SCF-002 — Freeze canonical proposal codec and token identity

- **Goal:** Define one byte-exact proposal and address derivation authority.
- **Scope/files:** new focused stablecoin primitives module and testdata under
  `crates/blockchain/primitives`; canonical JSON/ISO snapshot corpus and parameterized
  token-id/address vectors. SCF-003 supplies final network Factory/prefix constants.
- **Depends on:** SCF-001.
- **Done when:** widths for chain id/ticker length are explicit; Rust and an
  independent encoder agree on JSON bytes, full `tokenId`, prefix address and
  deliberate 144-bit-tail collision cases.
- **Tests:** T1 field/boundary validation; T2 whitespace/order/duplicate/escaping,
  ticker/name/ISO/decimals/cap/policy rejection corpus.
- **Verification:** `cargo nextest run -p outbe-primitives --test stablecoin_vectors`.
- **Exit:** no serde map ordering or platform integer width affects consensus bytes.

## SCF-003 — Select addresses, prefix, marker and reset policy

- **Goal:** Reserve the namespace before any user execution.
- **Scope/files:** `outbe-primitives` addresses, genesis fixtures,
  `scripts/seed_genesis.py`, a reproducible collision scanner and ADR evidence.
- **Depends on:** SCF-002.
- **Done when:** Factory/Policy addresses, prefix and exact marker are collision-free
  against Ethereum/Outbe/predeploy/genesis/planned ranges; each target network is
  explicitly classified as new genesis/reset-compatible or unsupported.
- **Tests:** T1 prefix/overlap and fixed-vector checks; T3 complete seeder output is
  rescanned and contains exact marker accounts. Fresh-localnet/reset evidence is
  deferred to SCF-083 under the Phase 7 T5 policy.
- **Verification:** `cargo run -p xtask -- stablecoin namespace-check`, Rust namespace
  vectors, `cargo nextest run -p xtask` and generated-genesis scan.
- **Exit gate:** **reset gate** — V1 cannot activate on state that executed before the
  address-class creation guard.

## SCF-004 — Freeze caps, gas, version and tooling ownership

- **Goal:** Remove the remaining runtime-configurable or implementation-chosen values.
- **Scope/files:** fork manifest/testdata and ADR/plan constants only.
- **Depends on:** SCF-001 through SCF-003.
- **Done when:** activation version, public pending cap, policy member-batch cap,
  Factory/Policy list page cap, warm/cold class semantics, token/Policy gas schedule,
  EIP-712 version and SDK owner are recorded; the SCF-002 ISO snapshot is verified,
  not replaced. Bond vector is exactly `10^24`.
- **Tests:** T2 constant/boundary vectors and manifest placeholder scan. SCF-034 and
  SCF-047 validate the frozen Policy/token caps and gas; Factory creation has no
  separate gas ceiling or benchmark reopen rule.
- **Exit gate G0:** SCF-001..004 approved; implementation may not silently change a
  frozen artifact without reopening ADR and vectors.

---

# Phase 1 — Characterization and generic infrastructure

## SCF-010 — StorageHandle and materialization survey

- **Goal:** Confirm no long-lived storage owner blocks the dynamic-precompile work.
- **Scope:** run the exact AGENTS.md 8.1 surveys; classify every hit; no production
  edits.
- **Depends on:** `SCF-G0`.
- **Done when:** report covers `StorageHandle<'static>`, long-lived wrappers, facade
  fields, callbacks and `read_all`; any unsafe owner expands scope before edits.
- **Tests/verification:** prescribed `rg` commands and existing trybuild suite.
- **Exit:** storage reviewer approves the survey.

## SCF-011 — Characterize current EVM routing

- **Goal:** Pin all existing exact-address behavior before replacing duplicate tables.
- **Scope/files:** EVM tests only.
- **Depends on:** `SCF-G0`.
- **Done when:** tests enumerate every dispatch-recognized fixed address, current
  lookup/list drift, warm/contains behavior, Ethereum fallback, actual caller/callee,
  top-level/nested parity and `Bytes`/`SharedBuffer` gas inputs.
- **Tests:** T0 production `OutbeEvmFactory` characterization; no count-only assertion.
- **Verification:** focused `outbe-evm` registration/subcall suites.

## SCF-012 — Characterize DirectStorageProvider hook behavior

- **Goal:** Pin current unsupported code mutation, checkpoint, event, change-set and
  balance behavior before changing it.
- **Scope/files:** primitives/EVM tests only.
- **Depends on:** SCF-010, SCF-011; serialize after the EVM routing baseline because
  both tasks modify `outbe-evm` tests.
- **Tests:** T0 `set_code`, nested checkpoint, account preservation, state-root hook,
  transfer overflow/underflow and event rollback.
- **Exit:** baseline passes on pre-refactor implementation.

## SCF-013 — Characterize Vote before public bonded admission

- **Goal:** Preserve Update/Governance behavior exactly.
- **Scope/files:** Vote tests only.
- **Depends on:** `SCF-G0`.
- **Tests:** T0 ACTIVE-only creation, zero-value rejection, raw payload retention,
  quorum under validator changes, handler failure, terminal replay and hook rollback.
- **Exit:** pre-feature ABI/state/log snapshots are pinned.

## SCF-014 — Characterize protocol version across execution modes

- **Goal:** Establish current H-1/H/H+1 behavior before introducing one resolver.
- **Scope/files:** Update/EVM/RPC tests only.
- **Depends on:** SCF-012; serialize in the EVM characterization lane.
- **Tests:** T0 canonical, payload build, validation, nested call and exact-block RPC.
- **Exit:** every current source/fallback is documented.

## SCF-020 — Consolidate exact precompile routing behind one compact declaration

- **Goal:** Remove the characterized 35-route/32-enumeration drift and the second
  body-reader address match without changing existing route behavior.
- **Scope/files:** add focused `outbe-evm/src/precompile_routes.rs`; refactor
  `precompiles.rs` and registration/preservation tests. One declaration generates
  exact lookup and complete exact enumeration and carries only the dispatch adapter
  (`Basic`, `BodyReadersRequired`, `BodyReadersOptional`) plus base-gas function.
- **Non-goals:** no stablecoin class route; no protocol activation, persistence,
  warming, reentrancy, ABI/schema, owner or sponsorship metadata; no change to
  `PrecompilesMap`/nested `warm_addresses` or `contains`; no gas schedule or current
  `Bytes`/`SharedBuffer` pricing-order change; no debug-route reachability change.
- **Depends on:** SCF-011, SCF-014.
- **Done when:** all 35 characterized exact routes come from one declaration; lookup
  and enumeration cannot drift; resolved route adapters replace the second reader
  match; duplicate exact declarations and Ethereum overlap fail compilation or
  mandatory construction validation; top-level and nested calls still share
  `outbe_ctx_dispatch`; byte/state/log/gas behavior of old exact routes is unchanged.
- **Tests:** T1 exact-set/duplicate/Ethereum-overlap and reader-mode validation; T3
  top-level/nested equivalence, Ethereum/ordinary fallback, missing-reader behavior
  and the characterized calldata pricing order. Tests exercise behavior and typed
  interfaces; they do not read or match Rust source text.
- **Verification:** full EVM registration, genesis/preservation and subcall suites;
  `cargo fmt --all --check`; `cargo clippy -p outbe-evm --all-targets -- -D warnings`.
- **Exit:** routing review confirms no class, activation, warm/cold, persistence or
  gas-policy change.

## SCF-021 — Enforce genesis-active CREATE/CREATE2 reservation

- **Goal:** Prevent any contract from occupying the selected two-byte class.
- **Scope/files:** focused EVM creation-guard module and the supported revm handler
  seam; dependency pin change only if unavoidable and separately reviewed.
- **Depends on:** SCF-003, SCF-020.
- **Done when:** top-level/nested CREATE and CREATE2 into the class fail with canonical
  gas/journal behavior; adjacent addresses and native balance transfers remain valid.
- **Tests:** T1 derivation predicate; T3 CREATE/CREATE2/nested/revert/OOG rollback;
  T5 localnet deployment attempts.
- **Exit:** EVM consensus review approves all creation paths.

## SCF-022 — Add authenticated actual-callee class routing

- **Goal:** Route many token addresses to one Rust handler without shadowing ordinary
  EVM accounts.
- **Scope/files:** add a thin exact/class resolver over `precompile_routes`, dispatch,
  subcall and storage-context seams; use a test registry/handler until Factory lands.
- **Depends on:** SCF-020, SCF-021.
- **Done when:** exact routes win; the entire reserved prefix is claimed even before
  runtime activation; inactive, unknown or unauthenticated members fail closed with
  no bytecode fallback; the class route receives actual callee; registration, reverse
  full id, schema and exact marker are all required; exact/class and class/class
  overlap fail compilation or mandatory construction validation.
- **Tests:** T1 route/auth outcomes; T3 two-instance isolation, forged marker,
  unregistered member, top-level/nested/static/reentrancy parity.
- **Exit:** security review proves prefix alone never authenticates a token.

## SCF-023 — Journal code mutation and checked balance credit

- **Goal:** Allow Factory execution inside the atomic pre-execution hook batch.
- **Scope/files:** `outbe-primitives` direct provider and focused executor tests.
- **Depends on:** SCF-022; this continues the single `outbe-evm` writer lane after
  dynamic routing instead of branching from characterization.
- **Done when:** `set_code` updates code/hash/change set; nested and outer rollback
  cover code/storage/balance/events; increase/transfer destination arithmetic is
  checked and fatal on overflow.
- **Tests:** T1 commit/revert/nested/overflow; T3 hook state-root notification includes
  code; trybuild lifetime suite remains green.
- **Exit:** storage and EVM reviewers approve one rollback domain.

## SCF-024 — Publish Factory logs through HookEvents

- **Goal:** Make pre-exec creation observable in the mandatory receipt.
- **Scope/files:** hook-event whitelist and executor receipt tests.
- **Depends on:** SCF-003, SCF-023; this is the next `outbe-evm` writer after
  provider code journaling.
- **Done when:** successful Factory log appears exactly once; rolled-back creation
  publishes none; non-whitelisted logs remain tracing-only.
- **Tests:** T1 partition; T3 full receipt construction and ordering.

## SCF-025 — Add one exact-state Outbe protocol-version resolver

- **Goal:** Remove `SpecId`/latest-state/token-schema ambiguity.
- **Scope/files:** primitives/Update re-export, EVM config/dispatch/subcalls and RPC
  exact-state construction; do not alter Update FSM.
- **Depends on:** SCF-014, SCF-004, SCF-024; serialize after HookEvents because these
  tasks share `outbe-primitives`/`outbe-evm` surfaces.
- **Done when:** proposer, validator, nested call and historical RPC resolve the same
  version from exact state and pass it explicitly into class routing; static exact
  route metadata and Ethereum `SpecId` are not activation authorities;
  missing/corrupt authority is fatal.
- **Tests:** T1 codec/failure; T3 H-1/H/H+1 and same-block Update visibility.
- **Exit:** Update/EVM/RPC owners approve the authority contract.

## SCF-026 — Introduce inert raw-byte Vote target APIs

- **Goal:** Give Factory admission/reservation/execution/cleanup a typed compile-time
  contract before changing Vote state.
- **Scope/files:** Vote target registry plus adapters for existing Update/Governance.
- **Depends on:** SCF-013, SCF-001.
- **Done when:** target receives original bytes, proposer/value/height/chain context;
  admission distinguishes validator-only and exact public bond; executable target
  outcome distinguishes Applied from Error; outer infrastructure/provider errors
  remain `Err` and abort the enclosing execution.
- **Tests:** T1 registry uniqueness/raw bytes/outcome classification; T3 existing
  targets remain behaviorally identical.

## SCF-027 — Make Vote operator notifications rollback-safe

- **Goal:** Prevent process-local JSONL/operator records from claiming a terminal
  result that canonical hook state later rolls back.
- **Scope/files:** Vote notification/lifecycle seam only; canonical HookEvents remain
  the authoritative receipt evidence.
- **Depends on:** SCF-013, SCF-026.
- **Done when:** notification publication is post-commit or idempotently reconciled by
  proposal/status identity; replay cannot leave a phantom Approved/Error/Bond event.
- **Tests:** T1 notification idempotency; T3 injected failure after notification
  preparation and restart/replay reconciliation.
- **Exit gate G1:** SCF-010..027 focused suites pass; generic infrastructure contains
  no Factory/Policy/token business state.

---

# Phase 2 — Shared Policy Registry precompile

## SCF-030 — Add Policy schema, state and typed API

- **Goal:** Establish the fixed state authority and cross-module query surface.
- **Scope/files:** new `crates/core/stablecoinpolicy` with
  `schema.rs`, `state.rs`, `api.rs`, `errors.rs`, tests and narrow re-exports.
- **Depends on:** `SCF-G0`. Fixed-address EVM integration is owned later by SCF-033;
  the Policy schema task has no conditional provider dependency.
- **Done when:** descriptors 0..4, built-in ids 0/1, checked next id, immutable type/
  children, current/pending admin, direct membership and dense member indexes are
  schema-stable.
- **Tests:** T1 raw slot/layout, missing/unknown ids, checked exhaustion; T2 random
  mapping/index/count invariants; no unbounded production `read_all`.

## SCF-031 — Implement bounded authorization evaluation

- **Goal:** Provide O(1), non-recursive, non-mutating authorization.
- **Scope/files:** Policy runtime/API/tests.
- **Depends on:** SCF-030.
- **Done when:** DenyAll/AllowAll/Whitelist/Blacklist truth tables and Directional
  send/receive/mint lanes are exhaustive; Directional children cannot be Directional.
- **Tests:** T1 truth tables; T2 fixed read-count, missing/cycle/self-reference and
  unknown descriptor properties.

## SCF-032 — Implement creation, membership and two-step admin

- **Goal:** Complete mutable Policy state with bounded atomic operations.
- **Scope/files:** Policy state/runtime/errors/tests.
- **Depends on:** SCF-031, SCF-004 caps.
- **Done when:** permissionless policy creation requires nonzero admin; batches reject
  duplicates/zero/oversize before writes; policy id/type/children are permanent;
  admin nominate/cancel/accept is two-step; membership mapping and dense index update
  atomically.
- **Tests:** T1 authorization and state machine; T2 cap-1/cap/cap+1, random sequence,
  rollback, stale nominee and count parity.

## SCF-033 — Wire canonical Policy precompile

- **Goal:** Expose ABI at the fixed address with no hidden business logic in dispatch.
- **Scope/files:** Policy `precompile.rs`, workspace/EVM dependency, fixed marker and
  ABI vectors.
- **Depends on:** SCF-001, SCF-032, SCF-025; fixed Policy registration is the next
  production `outbe-evm` writer after the generic EVM/version lane.
- **Done when:** unexpected value is rejected before reads; malformed/trailing ABI
  fails; views are static-safe; nested revert removes writes/logs.
- **Tests:** T1 every selector; T2 arbitrary calldata no-panic/no-mutation; T3
  registration, marker, top-level/nested parity.

## SCF-034 — Policy property and bounded-work gate

- **Goal:** Prove Policy state/index consistency and bounded mutation/authorization.
- **Depends on:** SCF-033.
- **Tests/evidence:** worst-case 64-member batch, directional evaluation, random
  mapping/index histories and `listPolicyMembers` boundaries at limit 1 and 100.
  View pagination has no gas-budget or benchmark gate.
- **Exit gate G2:** Policy crate, ABI, properties and EVM integration pass; typed
  `api.rs` is frozen for token use.

---

# Phase 3 — Dynamic Stablecoin token precompile

## SCF-040 — Add callee-scoped schema and Factory-only initialization API

- **Goal:** Define isolated per-token state and a narrow creation seam.
- **Scope/files:** new `crates/core/stablecoin` with schema/state/api/errors and tests;
  actual address is supplied by dynamic dispatch. This task records the V1 schema
  version but does not add `migration.rs` or a second schema.
- **Depends on:** SCF-001, SCF-002, SCF-022.
- **Done when:** immutable identity, supply/cap/policy/pause, balances, allowances,
  permit nonces, roles, admin transfer and frozen mappings have pinned layout;
  initialization is callable only through typed Factory API and starts at zero supply.
- **Tests:** T1 raw layout/initialization/address isolation; T2 invalid/corrupt schema.

## SCF-041 — Implement ERC-20 accounting and cap core

- **Goal:** Supply-conserving checked token accounting.
- **Depends on:** SCF-040.
- **Done when:** metadata, totalSupply, balance, allowance, approve, transfer,
  transferFrom, mint/burn internals, infinite allowance and cap checks match ABI.
- **Tests:** T1 full ERC-20 edge table; T2 reference-model sequences proving
  `sum(balances) == totalSupply`, `totalSupply <= cap`, exact allowance and rollback.

## SCF-042 — Implement roles, pause and two-step token admin

- **Goal:** Enforce the fixed authority topology.
- **Depends on:** SCF-041.
- **Done when:** ADMIN/ISSUER/CAP_MANAGER/GUARDIAN/COMPLIANCE/ENFORCER behavior,
  initial grants, admin nominate/cancel/accept, guardian pause/admin unpause and paused
  recovery matrix match ADR; repeated operational `grantRole`/`revokeRole` calls are
  successful no-ops with no state or event change.
- **Tests:** T1 every role/function positive/negative path; T2 random unauthorized
  sequences never mutate state; adminless state is unreachable.

## SCF-043 — Integrate Policy and ERC-7943

- **Goal:** Apply shared policy, freeze and enforcement semantics exactly.
- **Depends on:** SCF-042, `SCF-G2`.
- **Done when:** ordinary transfer/mint and allowance-backed `burnFrom` use the
  required Policy/freeze lanes; `can*` views do not revert/mutate; forced transfer
  rejects self, checks recipient and uses `U=B-min(B,F)`, `C=max(0,A-U)`, `F'=F-C`
  with event order `Frozen` (when `C>0`) → `Transfer` → `ForcedTransfer`.
  Privileged `ISSUER.burn()` of the issuer/redemption account bypasses sender policy
  and frozen spendability, remains blocked by global pause, and when it consumes
  frozen units applies the same formula with `Frozen` before the burn `Transfer`;
  public denial paths use the Final ERC-7943 standard errors rather than exposing
  the Policy Registry's internal error.
- **Tests:** T1 all descriptor/directional paths and `F<B`, `F=B`, `F>B`; authorized
  issuer self-burn versus unauthorized caller and allowance-backed `burnFrom`;
  T2 formula, conservation, frozen allowance reduction, issuer-burn event ordering,
  memo/non-memo parity and failure-log properties.

## SCF-044 — Implement EIP-2612

- **Goal:** Add replay-safe approvals bound to chain and dynamic token address.
- **Depends on:** SCF-041, SCF-001.
- **Done when:** domain separator, nonce, deadline, low-s recovery and exact v/r/s
  semantics match independent vectors; nonce advances only on success.
- **Tests:** T1 golden signatures; T2 replay, malleability, wrong chain/token/domain,
  expired/boundary deadline, invalid signer and failed-call nonce preservation.
- **Exit:** crypto review required.

## SCF-045 — Implement fixed `bytes32` memo variants

- **Goal:** Add event-only payment references without parallel accounting logic.
- **Depends on:** SCF-042, SCF-043 for enforcement variant.
- **Done when:** all six approved memo methods call the same underlying operations;
  no schema field stores memo; canonical Transfer/ERC-7943 events plus memo event are
  emitted in frozen order.
- **Tests:** T1 zero/mixed/all-ones memo log vectors; T2 arbitrary `B256` leaves state
  identical to non-memo operation and failed base operation emits no memo log.

## SCF-047 — Token property and gas gate

- **Goal:** Close ledger invariants and freeze gas schedule.
- **Depends on:** SCF-043 through SCF-045.
- **Evidence:** combined random operation model, malformed ABI corpus, static/nested/
  OOG rollback, worst policy/permit/mutation benchmarks.
- **Exit gate G3:** token ABI, properties, crypto, Final ERC-7943 and gas review pass.

---

# Phase 4 — Governed Stablecoin Factory

## SCF-050 — Add Factory schema, state, views and typed API

- **Goal:** Own permanent identity and pending reservations.
- **Scope/files:** new complex `crates/core/stablecoinfactory` with README,
  schema/state/api/errors/tests.
- **Depends on:** SCF-002.
- **Done when:** tokenCount/listTokens, tokenById, global tokenByTicker, tokenIdOf,
  address/full-id collision protection, pendingTokenId, pendingTicker,
  pendingAddress and proposal→reservation have explicit invariants and no permanent
  delete path; `listTokens` accepts `1 <= limit <= 100` without ordering semantics.
- **Tests:** T1 raw layout and every inverse; T2 random reserve/release/consume and
  global cross-issuer ticker uniqueness; list boundary tests at limit 1 and 100.

## SCF-051 — Implement canonical parsing and prediction

- **Goal:** Validate original bytes and deterministic identity before any write.
- **Depends on:** SCF-050, `SCF-G2`.
- **Done when:** typed decode + byte-identical re-encode validates proposer==issuer,
  metadata/ISO/decimals/cap/policy and all collision indexes; forced native balance at
  predicted token address is not treated as backing or collision.
- **Tests:** T1 canonical/rejected corpus; T2 independent derivation vectors and
  policy-existence failure with zero writes.

## SCF-052 — Implement atomic reservation lifecycle

- **Goal:** Reserve global ticker, token id and predicted address exactly once.
- **Depends on:** SCF-051, SCF-026.
- **Done when:** reserve writes all three indexes + proposal owner atomically;
  Expired releases all three; success consumes all three; Error retains all three.
- **Tests:** T1 same/different issuer duplicates and replay; T2 injected failure at
  each index and corrected resubmission after release.

## SCF-053 — Implement atomic token initializer

- **Goal:** Convert one approved reservation into one permanent token.
- **Depends on:** SCF-023, `SCF-G3`, SCF-052.
- **Done when:** Factory revalidates reservation/policy/address, initializes ledger,
  installs exact marker, commits all permanent indexes and emits
  `StablecoinCreated`; any target execution error is returned to Vote.
- **Tests:** T1 every validation/failure class; T2 failure after every mutation proves
  no partial code/storage/index/log; T3 real hook provider changes state root and
  preserves forced token-address COEN.

## SCF-054 — Wire Factory view precompile and Vote adapter

- **Goal:** Expose views publicly and mutation hooks only to compile-time Vote target.
- **Depends on:** SCF-053, SCF-026.
- **Done when:** public ABI has no create/reserve/release selector; Vote adapter owns
  validate/reserve/execute/release; Vote core imports no Factory storage type.
- **Tests:** T1 views and malformed/value rejection; T3 adapter contract and nested
  checkpoint behavior.

## SCF-055 — Factory property and atomicity gate

- **Goal:** Prove index consistency, reservation lifecycle and atomic creation.
- **Depends on:** SCF-054.
- **Evidence:** random pending/permanent histories, collision injection, max input,
  reservation triple and failure after every initializer mutation.
- **Exit gate G4:** Factory registry/initializer/adapter evidence passes independent
  storage and atomicity review.

---

# Phase 5 — Vote public bond and terminal settlement

## SCF-060 — Append Vote bond/liability schema compatibly

- **Goal:** Represent exactly-once bond settlement without reinterpreting legacy state.
- **Scope/files:** Vote schema/state/errors/API/ABI; append-only migration because
  existing Vote slot 0 already owns `proposal_count`.
- **Depends on:** SCF-026, SCF-004.
- **Done when:** proposal stores amount and NoBond/Unsettled/Refunded/Burned state;
  aggregate liabilities are checked; legacy zero-filled proposals are NoBond;
  counter exhaustion is typed.
- **Tests:** T1 raw V0→V1 slot fixtures, invalid enum, overflow/underflow; T3 fork read/
  tally of legacy proposal.
- **Exit:** storage-layout review required.

## SCF-061 — Implement payable target admission and caps

- **Goal:** Admit only Factory outsiders with exactly `10^24` value.
- **Depends on:** SCF-052, SCF-060.
- **Done when:** every non-Factory target/call rejects value before state; Factory
  requires caller==payload issuer and exact bond; global/per-identity caps apply;
  validate→allocate→reserve→record liability is one checkpoint.
- **Tests:** T1 admission/value/cap boundaries; T2 reservation failure and forced Vote
  surplus proving `balance >= liabilities`, never equality.

## SCF-062 — Implement typed nested finalization

- **Goal:** Record target execution failure without committing partial target state.
- **Depends on:** SCF-053, SCF-061.
- **Done when:** approved target work runs in a nested checkpoint; success commits
  target state and records Approved; a typed target Error outcome rolls target work
  back then records Error while retaining the reservation triple and liability;
  outer infrastructure/provider errors propagate; Expired releases the reservation
  triple.
- **Tests:** T1 transition table and validator-set changes; T2 injected failure before/
  after each Factory/status/index step. Only ACTIVE validators may cast; tally uses
  ballots from the current ACTIVE set and that set's count as its denominator.

## SCF-063 — Implement refund/burn settlement

- **Goal:** Settle one recorded liability exactly once.
- **Depends on:** SCF-062, SCF-023.
- **Done when:** Approved refunds exactly `10^24`; Expired burns exactly `10^24`
  with `decrease_balance`; Error performs no settlement; liabilities and settlement
  state update atomically; Metadosis and surplus sweep are absent.
- **Tests:** T1 arithmetic/events/replay/insufficiency; T2 forced surplus remains,
  double finalization cannot move value, and Error retains its unsettled liability.
- **Exit:** independent accounting review.

## SCF-064 — Vote/Factory adversarial integration gate

- **Goal:** Prove token-or-no-token and one-settlement behavior through real APIs.
- **Depends on:** SCF-054, SCF-063.
- **Tests:** T3 approve/refund, expire/burn, execution Error with retained
  bond/reservations, spam caps, validator-set change, global ticker/address
  collisions, terminal replay and every initializer mutation boundary.
- **Exit gate G5:** Vote, Factory, provider and accounting suites pass together; no
  critical test is ignored.

---

# Phase 6 — Production activation and consensus evidence

## SCF-070 — Wire production routes and compile-time targets

- **Goal:** Connect completed modules without runtime plugins.
- **Scope/files:** workspace/EVM dependencies, exact Factory/Policy routes, token
  class route, Factory Vote target and global version context.
- **Depends on:** `SCF-G1`, `SCF-G2`, `SCF-G3`, `SCF-G4`, `SCF-G5`.
- **Tests:** T1 route/target uniqueness and activation; T3 existing routes/targets
  remain unchanged and stablecoin instances authenticate full id/schema/marker.

## SCF-071 — Land genesis/reset activation artifact

- **Goal:** Ship namespace reservation from block 0 and selectors at the named fork.
- **Depends on:** SCF-003, SCF-004, SCF-021, SCF-070.
- **Done when:** fixed markers/allocations, genesis hash/reset instructions and
  pre-fork fail-closed/post-fork active vectors are final.
- **Tests:** T3 genesis/fork matrix; T5 fresh pre-activation localnet.
- **Exit:** explicit operator/network reset approval.

## SCF-072 — Prove full-executor HookEvents receipt

- **Goal:** Establish canonical receipt visibility, not only event partitioning.
- **Depends on:** SCF-024, SCF-064, SCF-070.
- **Tests:** T4 execute full block and assert exactly one ordered
  `StablecoinCreated`, receipts root/log bloom/cumulative gas, marker code/hash and no
  target log after an execution Error rollback.

## SCF-073 — Proposer/validator full-block parity

- **Goal:** Prove deterministic consensus/execution output.
- **Depends on:** SCF-072.
- **Fixture equality:** parent/header, certified-parent/finalization inputs,
  `extra_data`, committee snapshot, transaction list, chain spec and starting state.
- **Output equality:** state root, receipts root, receipt status/log order/bloom,
  gas/refunds/cumulative gas, Factory indexes, token marker/code hash, token balances/
  supply and Vote/native bond deltas.
- **Tests:** T4 Approved, Expired and Error boundaries; no scaffold/ignored
  parity test is accepted.

## SCF-074 — Fork-boundary, RPC and deterministic replay

- **Goal:** Verify H-1/H/H+1 behavior across restart and exact-block reads.
- **Depends on:** SCF-025, SCF-073.
- **Tests:** T4 repeated proposer/validator replay from identical snapshot; historical
  static views, nested calls, restart and RPC at each boundary.
- **Exit gate G6:** SCF-070..074 pass on required consensus CI architectures; no
  activation constant remains provisional.

---

# Phase 7 — Operator tooling, feature E2E and release

## SCF-080 — Implement outbe-cli policy and proposal flows

- **Goal:** Produce canonical bytes/value without exposing key material remotely.
- **Depends on:** SCF-001, SCF-002, SCF-051, SCF-071.
- **Done when:** CLI creates policies, predicts global ticker/token id/address,
  defaults policy=1 and decimals=6 but serializes both, shows raw/human cap, attaches
  exact `10^24` bond and prints the non-endorsement warning.
- **Tests:** T1 parsing/boundaries; T2 CLI bytes equal goldens; mocked invalid input
  performs no RPC call; signed transaction value is exact.

## SCF-081 — Resolve and implement SDK scope

- **Goal:** Remove ambiguous “CLI/SDK” promises.
- **Depends on:** SCF-004, SCF-001.
- **Done when:** maintained SDK package/location is identified and consumes the same
  vectors, or V1 is explicitly declared CLI-only and SDK claims are removed.
- **Tests:** vector parity with primitives/CLI if SDK exists.

## SCF-082 — Add stablecoin-specific localnet E2E

- **Goal:** Exercise the product flow, not merely node height.
- **Depends on:** SCF-064, SCF-072, SCF-080.
- **Scope/files:** `outbe-e2e-harness` feature and native Alloy steps.
- **Scenario:** create shared policy, submit exact bonded proposal, vote/tally, inspect
  HookEvents receipt, assert refund, mint, transfer, permit, memo, pause/recovery,
  freeze/forced transfer, second issuer global-ticker rejection, restart all nodes and
  compare historical/current reads.
- **Tests:** T5 four validators; every node agrees on height, proposal, indexes,
  token/code, balances, receipts and bond deltas.

## SCF-083 — Run localnet reset/restart and mixed-version gates

- **Goal:** Validate operator activation behavior.
- **Depends on:** SCF-071, SCF-082.
- **Tests:** T5 fresh genesis, pre-fork rejection, activation, restart, incompatible
  old binary/mixed manifest failure and ordinary `mise run localnet-smoke`.

## SCF-084 — Close README, ADR and audit contracts

- **Goal:** Make external and architectural contracts match verified behavior.
- **Depends on:** SCF-073, SCF-074, SCF-080, SCF-081, SCF-083.
- **Scope:** root README, CLI help, module READMEs, ABI exports, ADR-S-GOV-002,
  ADR-B-EVM-002, TOK ADR statuses/index/coverage and relevant `audit_*.md` entries.
- **Done when:** addresses, prefix, marker, bond, caps, flow, roles, policy,
  non-endorsement, reset and schema-1 behavior are exact and evidence-linked.

## SCF-085 — Release verification and GO/NO-GO

- **Goal:** Produce a reviewable release evidence bundle.
- **Depends on:** SCF-084.
- **Commands/evidence:** Forge build/test/ABI export; all affected crate, CLI,
  primitives trybuild, EVM, Vote, Update, node, RPC and e2e suites; doctests where
  public examples changed; workspace nextest; fmt; clippy; supply-chain checks;
  benchmarks; localnet outputs.
- **Pass:** no failed or ignored required test, dirty generated ABI, provisional
  constant, README/debt contradiction or unreviewed consensus finding.
- **Exit gate G7:** release is GO only after independent consensus, storage,
  cryptography, security and documentation reviews accept the evidence. Otherwise
  the decision is NO-GO.

---

## Milestone gate packets

Gate names used in dependencies are executable read-only review packets, not prose
aliases. When Taskplane packets are generated, use these exact gate dependencies:

| Gate packet | Depends on | Pass evidence |
| --- | --- | --- |
| `SCF-G0` | SCF-001, SCF-002, SCF-003, SCF-004 | protocol artifacts approved; no implementation-chosen constant |
| `SCF-G1` | SCF-022, SCF-024, SCF-025, SCF-027 | EVM/provider/version/Vote infrastructure and old-behavior regressions pass |
| `SCF-G2` | SCF-034 | Policy ABI/state/index/properties pass |
| `SCF-G3` | SCF-047 | token ABI/model/crypto/Final ERC-7943/gas pass |
| `SCF-G4` | SCF-055 | Factory index/initializer/adapter evidence passes |
| `SCF-G5` | SCF-064 | Vote/Factory accounting and failure matrix passes |
| `SCF-G6` | SCF-074 | full-block/fork/RPC parity passes |
| `SCF-G7` | SCF-085 | operator E2E/docs/release reviews produce GO |

A gate packet makes no production edit. It reads the dependency evidence, runs or
checks the declared gate commands, records pass/fail and blocks downstream tasks on
fail.

## Safe parallel execution lanes

After `SCF-G0`:

- SCF-010 (read-only survey) and SCF-013 (Vote tests) may run in parallel.
- Every pre-Factory `outbe-evm` writer is one executable dependency chain:
  SCF-011 → SCF-012 → SCF-014 → SCF-020 → SCF-021 → SCF-022 → SCF-023 →
  SCF-024 → SCF-025. SCF-033 joins that same lane after both SCF-025 and the Policy
  runtime dependency SCF-032; it cannot be scheduled as a sibling EVM writer.
- SCF-026 → SCF-027 is a separate sequential Vote lane and may run in parallel with
  the EVM/provider lane.
- Policy SCF-030..032 is one sequential crate-local lane; SCF-033 waits for the EVM
  lane, and SCF-034 follows SCF-033.
- Token schema SCF-040 may start after ABI freeze, but policy integration SCF-043
  waits for `SCF-G2` and dynamic production routing waits for Factory authentication.
- Factory SCF-050..055 is sequential; initializer waits for provider, Policy and token
  APIs.
- Vote SCF-060..064 is sequential because every task changes the same proposal FSM.
- Production activation, parity, localnet and documentation are never parallelized
  ahead of their gates.

No two agents write the same crate/worktree concurrently. Parallel work uses isolated
worktrees and merges only after each task's focused gate passes.

## Verification command matrix

These are minimum phase commands; each generated Taskplane packet narrows them with
its focused test filter and records exact output.

| Gate | Minimum commands |
| --- | --- |
| `SCF-G0` | `cd contracts/precompiles && forge fmt --check src/IStablecoin.sol src/IStablecoinFactory.sol src/IStablecoinPolicyRegistry.sol src/IVote.sol test/StablecoinInterfaces.t.sol && forge test --match-contract StablecoinInterfacesTest`; `cargo nextest run -p outbe-primitives --test stablecoin_abi_vectors --test stablecoin_vectors --test stablecoin_namespace --test stablecoin_fork_vectors`; `cargo nextest run -p outbe-vote`; `cargo nextest run -p xtask --test stablecoin_namespace`; `cargo run -p xtask -- stablecoin abi-check`; `cargo run -p xtask -- stablecoin namespace-check` |
| `SCF-G1` | prescribed StorageHandle `rg` survey; `cargo nextest run -p outbe-primitives --test trybuild`; `cargo nextest run -p outbe-evm`; `cargo nextest run -p outbe-vote -p outbe-update -p outbe-governance` |
| `SCF-G2` | `cargo nextest run -p outbe-stablecoinpolicy`; focused `cargo nextest run -p outbe-evm` registration/subcall tests |
| `SCF-G3` | `cargo nextest run -p outbe-stablecoin`; EIP-2612/Final ERC-7943/property filters; focused dynamic-route EVM tests |
| `SCF-G4` | `cargo nextest run -p outbe-stablecoinfactory`; focused provider/hook rollback and state-root tests |
| `SCF-G5` | `cargo nextest run -p outbe-vote -p outbe-stablecoinfactory -p outbe-primitives -p outbe-evm` |
| `SCF-G6` | affected Update/RPC/node/EVM suites plus dedicated full-block parity and fork-boundary targets |
| `SCF-G7` | `cargo run -p outbe-e2e-harness --bin outbe-e2e -- --tee none --validators 4 --all --input crates/testing/e2e-harness/features/stablecoin.feature`; `mise run localnet-smoke`; `cargo nextest run --workspace`; `cargo test --doc --workspace`; `cargo fmt --all --check`; `cargo clippy --all-targets -- -D warnings` |

## Recommended PR boundaries

1. **PR-A:** SCF-001..004 — decisions, interfaces and vectors only.
2. **PR-B:** SCF-010..014 — characterization only.
3. **PR-C1:** SCF-020..024 — EVM/provider infrastructure.
4. **PR-C2:** SCF-025..027 — version, inert Vote target and rollback-safe notification infrastructure.
5. **PR-D:** SCF-030..034 — Policy Registry, split by state/runtime/ABI/property commits.
6. **PR-E:** SCF-040..045 and SCF-047 — token slices, each behavior with its tests.
7. **PR-F:** SCF-050..055 — Factory.
8. **PR-G:** SCF-060..064 — Vote bond and Factory finalization.
9. **PR-H:** SCF-070..074 — activation and consensus evidence.
10. **PR-I:** SCF-080..085 — tooling, E2E, docs and release dossier.

## Definition of done

Stablecoin Factory V1 is complete only when:

- each proposal becomes Approved, Expired or Error; Approved refunds, Expired burns,
  and Error retains its bond liability and reservation triple for a future
  validator-approved governance transition outside V1;
- every token is authenticated by Factory full id/schema/marker and uses isolated
  callee-scoped storage through one Rust precompile implementation;
- global ticker uniqueness holds across pending and permanent state;
- Policy evaluation is single-source, non-recursive and bounded;
- ERC-20, EIP-2612 and ERC-7943 ABI/property/adversarial suites pass;
- schema version 1 is pinned and unknown schemas fail closed; speculative migration
  infrastructure is absent from V1;
- HookEvents receipt and proposer/validator outputs are byte/state equivalent;
- the stablecoin-specific four-validator E2E passes before and after restart;
- README states that admission is not backing, stability, redeemability or fee
  eligibility;
- no unresolved address, prefix, marker, version or cap placeholder remains;
- bond vectors pin exactly `1,000,000 COEN = 10^24` base units.
