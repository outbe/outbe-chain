

<!-- Source: .ruler/AGENTS.md -->

# Outbe-Reth Agent Guide

This file defines repository-specific rules and working context for agents and contributors working on `outbe-chain`.

## 1. Project Identity

`outbe-chain` is a single-binary blockchain node built from:
- `reth` as the execution layer
- `Commonware Simplex` as the consensus layer

This repository is not a generic app. It is an example implementation of:
- `Reth + Simplex`
- single-binary EL+CL integration
- stateful Rust precompiles for validator lifecycle, staking, rewards, slashing, and custom business logic

The design intent is:
- no HTTP Engine API split between EL and CL
- in-process consensus/execution integration
- Reth SDK execution
- Commonware Simplex consensus
- hard-fork driven upgrades

## 1.1 Documentation Contract

`README.md` is the normative external specification for `outbe-chain`.

Use sources this way:

1. `README.md`
   - intended external behavior
   - intended operator flow
   - intended protocol and architecture contract
2. current code and tests
   - actual implementation state
   - enforcement and runtime behavior
4. whitepaper
   - historical design intent and ADR context

Rules:

1. Do not silently downgrade `README.md` to match accidental implementation behavior.
2. If code differs from `README.md`, either:
   - fix the code, or
   - record the deviation with label `bug` for confirmed bugs, `tech_debt` for module-level debt,  label `arch_debt` for architectural gaps
3. When product behavior changes intentionally, update `README.md` and the debt files together.
4. Do not copy whitepaper claims into `README.md`, tasks, or reviews unless they are verified against:
   - current `outbe-chain`
   - reference implementation usage in this repository
5. If `README.md` is ahead of implementation, prefer an explicit `Current implementation note(s)` block in the relevant section and point to the debt files.

## 2. Key Architectural Assumptions

These are design constraints, not bugs:

1. Consensus and execution run in one binary.
2. Upgrades are coordinated by binary rollout / hard fork, not on-chain governance.
3. Rust precompiles and hooks are first-class runtime logic.
4. Determinism across proposer and validator execution paths is mandatory.
5. Full-node mode is a distinct trust model from validator mode.

Do not introduce changes that implicitly move the design toward:
- EL/CL split over HTTP Engine API
- proxy-admin style governance
- non-deterministic execution between proposer and validator

## 3. Internal Repository Map

Primary areas:

- `bin/outbe-chain`
  - node binary
  - CLI wiring
  - full node / validator startup
- `bin/outbe-cli`
  - operator CLI
  - validator, staking, rewards, monitoring commands
- `crates/blockchain/consensus`
  - Commonware Simplex integration
  - DKG
  - certificate / scheme logic
  - application handler
- `crates/blockchain/evm`
  - Reth execution integration
  - pre/post-execution hooks
  - extra_data / participation encoding
- `crates/system`
  - validator set
  - staking
  - rewards
  - slash indicator
- `crates/core`
  - business precompiles and orchestrators
- `interfaces`
  - ABI and interface artifacts
- `scripts`
  - genesis seeding and support scripts

## 4. Reference Codebases

When you need precedent or implementation patterns, use these reference projects.
Do not hard-code developer-specific absolute paths or required environment variables in rules, skills, docs, or generated agent files.
If reference source inspection is required, first discover whether the checkout is available in the current workspace; otherwise ask for its location before making reference-repo-dependent claims.

| Repo | Path | What we use | How |
|---|---|---|---|
| `commonwarexyz/monorepo` | discover locally when needed | Simplex consensus, BLS crypto, DKG, certificate scheme | Core dependency |
| `paradigmxyz/reth` | discover locally when needed | Execution layer, chain spec, CLI framework | Core dependency |

Rules for using reference repos:

1. Treat them as reference implementations, not code to copy blindly.
2. Match the pattern, but adapt it to Outbe’s single-binary EL+CL design.
3. For consensus or BLS/DKG library semantics, verify against `monorepo`.
4. For Outbe-specific behavior, verify how `outbe-chain` integrates those primitives before documenting them.
5. Do not reintroduce layered EL/CL HTTP architecture from Malaketh-style designs.

## 4.1 Whitepaper Usage

Use the whitepaper for:
- terminology
- intended architecture
- ADR rationale
- high-level module relationships
- historical design decisions

Do not use the whitepaper as authoritative proof of:
- current CLI flags
- current RPC semantics
- current validator flow
- current startup/runtime behavior
- current precompile surface
- current economics implementation details

If you move material from the whitepaper into `README.md`:
1. verify it against the current repository state
2. verify library-level claims against `monorepo`
3. if implementation is incomplete, keep the intended behavior in `README.md` and record the deviation in debt files
4. avoid undocumented security claims, especially around:
   - DKG liveness
   - P2P authentication
   - block propagation
   - peer blocking / scoring

## 5. Numeric Rules

1. Do not use `f32` or `f64` in production code for:
   - on-chain amounts
   - rewards
   - emissions
   - pricing
   - VWAP
   - stake
   - slashing
   - balances
   - validator economics
2. Use fixed-point integer arithmetic with an explicit scale factor.
3. Default numeric type for token amounts, rates, and economic state is `U256`.
4. If a narrower integer type is used, document the bound and prove the conversion is safe.
5. Any conversion from `U256` to smaller numeric types must be justified in code comments and covered by tests.
6. `f32/f64` may exist in tests, research code, or temporary migration tooling only if:
   - they are not in production execution paths
   - they are clearly isolated
   - they are not used for final on-chain state transitions

## 6. Safety Rules

1. Do not use `unwrap()`, `expect()`, `assert!()`, `assert_eq!()`, or `panic!()` in:
   - consensus runtime
   - execution runtime
   - precompiles
   - hooks
   - RPC handlers
   - validator/node startup paths
2. Return structured errors instead of crashing the node.
3. Do not silently delete or overwrite user-owned state during partial processing.
4. Validate state-machine transitions explicitly.
5. If partial processing is possible, make completion atomic or recoverable.
6. If a failure is unrecoverable, make that explicit and deterministic in control flow and logs.

## 7. Consensus / Execution Invariants

When touching consensus, execution, or their boundary:

1. Preserve deterministic behavior across proposer and validator execution paths.
2. Any committee-dependent data must be decoded against the same committee it was encoded for.
3. Reshare / validator-set changes must not alter accounting semantics mid-block unless explicitly designed and tested.
4. Block availability failure paths must be explicit:
   - retry
   - fail-fast
   - or real recovery
   - but never silent stall
5. Do not rely on comments or README alone; verify actual runtime control flow.

## 8. Precompile and Hook Rules

1. New precompiles should use a single registry source of truth when possible.
2. If a precompile needs persistent state, ensure the account is preserved under EIP-161 semantics.
3. State-changing precompiles must have:
   - explicit validation
   - deterministic storage updates
   - tests for failure paths
4. If a precompile claims to transfer or mint value, verify the real balance movement path exists.
5. Hooks must be reviewed for:
   - determinism
   - unbounded work
   - hidden cost model
   - ordering relative to tx execution and post-exec accounting
6. Begin-block and end-block runtime modules must use `outbe_primitives::block::{BlockContext, BlockRuntimeContext, BlockLifecycle}`:
   - the executor builds one canonical `BlockContext` from block/header, chain, proposer, and validator-set state
   - the executor wraps it in `BlockRuntimeContext` with the current scoped `StorageHandle`
   - module lifecycle entrypoints implement `BlockLifecycle` on a zero-sized marker type, for example `XxxLifecycle`
   - the executor calls lifecycle modules through `<XxxLifecycle as BlockLifecycle>::begin_block(&runtime_ctx)` or `end_block(&runtime_ctx)`
   - do not add or keep block-boundary APIs that pass ad hoc positional arguments such as `(timestamp, block_number)`
   - lifecycle ordering must stay explicit in the executor and hard-fork governed, not runtime plugin registration
7. Do not add production runtime code that hides persistent-state access behind implicit context or `Contract::default()` construction. Macro-generated contracts and module storage facades must receive explicit `StorageHandle` via `storage.contract::<T>()`, `ctx.contract::<T>()`, `Contract::new(storage)`, `Contract::at(storage, address)`, or typed `storage.rs` accessors.
8. Per-block execution summary data that affects validator reward settlement belongs in `OutbeBlockArtifacts` in `header.extra_data`, not in consensus metadata transactions and not in per-block Rewards storage. Phase 1 finalized-parent system transaction input may identify finalized block number/hash and voters; money fields must be loaded from the finalized block artifact and recomputed/validated by execution.

## 8.1 StorageHandle review guardrails

Before changing `StorageHandle`, generated contract facades, or storage primitive lifetimes, run a read-only pre-survey for long-lived storage/facade ownership. Look specifically for `StorageHandle<'static>`, `StorageHandle` inside `Arc`/`Box`/`Mutex`/`OnceCell`/`static`, contract facades stored in long-lived struct fields, and storage handles outside `BlockRuntimeContext`, storage primitive wrappers, `CheckpointGuard`, or test fixtures. If any such runtime owner is found, stop and update the refactor scope before editing code.

Recommended pre-survey commands:

```sh
rg -n "StorageHandle<'static>|Box<[^\n>]*StorageHandle|Arc<[^\n>]*StorageHandle|Mutex<[^\n>]*StorageHandle|OnceCell<[^\n>]*StorageHandle|static .*StorageHandle" crates bin --type rust
rg -nU "struct\s+\w+[^\{]*\{[^\}]*StorageHandle" crates bin --type rust
rg -n "read_all\(\)|\.read_all\(" crates bin --type rust
rg -n "FnOnce|FnMut|impl Fn|Ref<|RefMut<|with_account_info|account_info" crates/blockchain/primitives/src/storage crates/blockchain/macros/src --type rust
```

read_all() is a materialization API. It is acceptable for tests, bounded arrays, admin/debug/read paths, or explicitly capped collections. Do not use it in hot runtime paths over unbounded `StorageVec`/`StorageSet` data without a cap, pagination/index iteration, or a written justification.

Storage lifetime safety must remain covered by compile-fail tests. Keep tests for provider-scope escape, `!Send`/thread-spawn rejection, and `'static` facade escape. The provider-scope test is a standard borrow-checker check; the `!Send` and `'static` facade tests protect the architecture contract.

## 9. Documentation Consistency Rules

Keep these in sync:
- README
- CLI behavior
- RPC behavior
- runtime behavior

`README.md` is the spec. The debt files describe where implementation still deviates.

If code changes user-visible behavior, update README unless the task explicitly says no update is needed.

If implementation still does not meet the README contract:
- keep README as the intended contract
- add or update the deviation :
  - with label `bug` (confirmed bug)
  - with label `tech_debt` (module-level debt)
  - with label `arch_debt` (architectural gaps)
- do not hide the gap by weakening the README unintentionally

Required examples of README-sensitive changes:
- CLI flags
- validator onboarding flow
- staking flow
- RPC response semantics
- full-node / validator mode behavior
- key storage backend behavior
- any statement imported from the whitepaper

## 10. Testing Expectations

When fixing a bug, prefer:
- one regression test that proves the bug is closed
- one happy-path test if behavior changed
- one edge-case test if the bug was boundary-condition dependent

When touching cross-module flows, add more than unit tests:
- integration test
- execution-level test
- or end-to-end flow test

Especially important for:
- validator lifecycle
- staking / unbonding / claim
- reshare / DKG
- rewards / emission
- custom orchestrators

## 11. Review Checklist for Agents

Before proposing or implementing a fix, check:

1. Does the proposed fix actually close the runtime/control-flow bug, or only part of it?
2. Does it require README changes?
3. Does it require new tests?
4. Does it preserve proposer/validator determinism?
5. Does it create a new source of truth instead of reusing an existing one?

## 13. Preferred Working Style for Agents

1. Start by reading task;
2. When changing behavior, update the associated task/debt/audit documents if needed.
3. When adding a task, use `.agents/task-template.md`.
4. When uncertain about architecture, inspect the reference codebases before inventing a new pattern.
5. Do not close a task logically if:
   - the fix is partial
   - tests are missing
   - README still lies
   - a funding / transfer / state transition path is still incomplete



<!-- Source: .ruler/architecture.md -->

# Architecture Rules

- Treat `outbe-chain` as a single-binary EL+CL node, not as a generic service.
- Keep validator mode and full-node mode as distinct trust models; full nodes sync and serve RPC without voting, proposing, or requiring consensus key material.
- Keep consensus, execution, runtime hooks, and stateful precompiles deterministic across proposer and validator paths.
- Prefer explicit wiring over plugin-style discovery for consensus-critical lifecycle ordering.
- Do not introduce an HTTP Engine API split or proxy-admin governance model.
- Process-local consensus state must not be the source of truth for execution, accounting, or runtime state transitions.
- Lifecycle ordering uses `BlockLifecycle` on zero-sized marker types and `BlockRuntimeContext` from `outbe-primitives::block`; modules do not expose ad-hoc positional block-boundary APIs (see README "Architecture" and README "Stateful Runtime Module Contract").
- Reuse existing module boundaries: `bin/` for binaries, `crates/blockchain/` for Reth/Commonware integration, `crates/system/` for protocol modules, `crates/core/` for business modules.
- `ChainSpec` genesis hash is immutable at runtime; any change is hard-fork coordinated and updates README plus debt records.
- Genesis V2 (`scripts/seed_genesis.py`) seeds only public state: validator addresses + BLS MinPk public keys, oracle pairs, pre-deployed external contracts, and the `0xEE04` accounting-progress marker with `slot 0 = 0`. VRF group public key, DKG share material, polynomial scalars, and dealer secrets are produced by the runtime DKG and never appear in `genesis.json` — committing them would diverge the trust model. Block 1's mandatory `BoundaryOutcome` system tx is the single writer of the epoch-0 `CommitteeSnapshotStore` (`ValidatorSet` slots 31..40); see `consensus_execution.md`.
- Before adding a new abstraction, verify it removes real duplication or matches an existing local pattern.
- For the canonical storage-backed determinism rule, see `storage_handle.md`.

See also: `.ruler/skills/lifecycle-autodoc/`



<!-- Source: .ruler/consensus_execution.md -->

# Consensus And Execution Boundary

- Verify proposer and validator paths use the same parent block, header inputs, transaction list, `extra_data`, `prev_randao`, epoch-scoped committee, chain spec, and starting EVM state.
- Decode committee-dependent data against the same ordered committee for which it was encoded.
- `header.extra_data` carries `OutbeBlockArtifacts` with execution data that affects the block hash: `execution_summary` (tag `0x01`), `consensus_header_artifact` (tag `0x02`/`0x03` for `BoundaryOutcome`/`DealerLog`), `timestamp_millis_part` (tag `0x05`), and `late_finalize_credits` (tag `0x06`, the vote.md `LateFinalizeCreditsArtifact`). The active codec is `VERSION 0x08`. Finalized-parent certificate metadata does **not** ride in `header.extra_data`; legacy tag `0x04` is rejected by the active codec. The header `late_finalize_credits` artifact is BLS-verified pre-exec and bound to the body `LateFinalizeCredits` system-tx calldata by the stateless validator (header↔body parity). Encoded header artifacts stay within `OUTBE_MAX_EXTRA_DATA_SIZE = 64 KiB` (see README "Consensus Artifact Transport").
- Finalized-parent certificate metadata is an exact-parent Phase 1 begin-zone system-transaction input. `FinalizationActor` is the single writer to the consensus-owned `FinalizedParentCertStore`; proposers wait only for the certificate record whose hash equals the Simplex context parent. They do not scan a 256-block backlog, pick oldest-first records, or skip to unrelated parents.
- V2 Phase 1 replaces the exact-parent finalization wait with **direct-parent certified accounting**: block N ≥ 2 begin-zone starts with `SystemTxKind::CertifiedParentAccounting` whose input is the direct-parent's certified-accounting metadata. The progress marker is `ACCOUNTING_PROGRESS_ADDRESS` (`0xEE04`) slot 0 = `last_accounted_block_number: u64`; the account has no precompile dispatch and is preserved across EIP-161 by `0xef` marker bytecode.
- Genesis V2 bootstrap: `genesis.json` MUST NOT contain DKG share, polynomial scalar, dealer secret, or VRF group key material. Block 1 mandatorily carries `ConsensusHeaderArtifact::BoundaryOutcome` from `DkgManager`; that system tx is the first writer to the V2 `CommitteeSnapshotStore` (`ValidatorSet` slots 31..40). A block-1 proposal without a `BoundaryOutcome` artifact deterministically forfeits its slot (`genesis_dkg_boundary_not_ready`). Block N ≥ 2 reads the snapshot via Phase 1.
- Vote/slashing facts ride in the Phase 1 finalized-parent metadata; settlement money fields stay in `ExecutionSummaryArtifact` (in `extra_data`), not in consensus memory or generic metadata.
- Settlement money fields are loaded from the finalized block's committed artifact via the historical provider and recomputed or validated by execution.
- Begin-zone system transaction order is Phase 1 finalization/slashing (`CertifiedParentAccounting`, begin_order 0), then the mandatory `LateFinalizeCredits` phase (begin_order 1, every block ≥ 2; records in-window late finalize credits and settles the matured `N+K` fee escrow — vote.md §4.4–4.5), then CycleTick, optional BoundaryOutcome, then OracleSlashWindow. All phases execute before user transactions and emit normal receipts; Oracle slash-window penalties run after any same-block BoundaryOutcome activation and stay bounded by the protocol validator cap.
- Non-terminal emission sinks run under local storage checkpoints.
- Non-terminal sink failure or unused return falls back deterministically to terminal Metadosis.
- Terminal Metadosis failure is fatal to the block hook.
- Block availability failures choose an explicit path: retry, fail-fast, or recovery; silent stalls are not acceptable.
- Finalization is monotonic; finalized blocks are not reorged; full-node sync resumes from finalized state.
- View timeout deterministically nullifies the view and advances leader selection via VRF from the prior finalized certificate; genesis view 1 is the one-time round-robin exception (see README "Consensus Deep Dive").
- DKG completes on threshold participation, not `n`-of-`n`; unreachable validators must not block ceremony completion.
- `EXITING` validators remain accountable in the current consensus set until `activateResharedSet()` completes; only after activation do they transition to `UNBONDING` (see README "Becoming a Validator").
- Consensus and execution encoding (`OutbeBlockArtifacts`, `BoundaryOutcome`, `DealerLog`, and Phase 1 `FinalizedParentAttestation` input) is byte-for-byte deterministic across validators.

See also: `.ruler/skills/consensus-determinism-review/`



<!-- Source: .ruler/dependency_upstream_policy.md -->

# Dependency And Upstream Policy

- Treat upstream projects as reference implementations, not copy-paste sources.
- `Cargo.toml` pins Reth by git revision and Commonware by tag; dependency updates are reviewed as protocol-relevant until the changed boundary is identified and tested.
- For Reth-specific API and SDK behavior, follow `reth_sdk_integration.md`.
- Re-check Alloy, Revm, Reth, and Commonware updates for transaction encoding, block/header semantics, EVM execution, provider behavior, consensus scheme semantics, and serialization formats.
- Dependency updates include targeted tests for every behavior boundary they affect.
- If an upstream update changes user-visible or protocol behavior, update `README.md` and the corresponding `audit_*.md` entry in the same PR.
- `Cargo.lock` is committed to the repository.
- `cargo update` is a reviewed action, not a routine run.
- Supply-chain review uses the repository's configured tooling, currently `cargo-vet` under `supply-chain/`.

See also: `.ruler/skills/reth-sdk-change-review/`



<!-- Source: .ruler/docs_contract.md -->

# Documentation Contract Rules

- `README.md` is the normative external specification for operator flow, protocol surface, CLI, RPC, addresses, and architecture.
- If implementation differs from `README.md`, fix code or record the deviation in an `audit_*.md` file at repo root with label `bug`, `tech_debt`, or `arch_debt`.
- Do not weaken `README.md` to match accidental behavior unless product intent changed.
- If user-visible behavior changes, update `README.md` and any relevant debt/audit files in the same change.
- Treat `whitepaper.md` as historical design context, not proof of current CLI, RPC, validator flow, precompile surface, startup behavior, or economics implementation.
- Verify whitepaper-imported claims against code and reference repos before restating them.
- README `Current implementation note(s):` blocks track README-ahead-of-implementation deviations and reference the corresponding `audit_*.md` entry when debt is outstanding (see README "Stateful Runtime Module Contract").
- Edits to `.ruler/*.md` require `ruler apply --agents codex,claude --config .ruler/ruler.toml --mcp --no-gitignore --skills --nested --no-backup` in the same commit.
- Generated files (`AGENTS.md`, `CLAUDE.md`, `.mcp.json`, `.codex/`, `.claude/`) must not be edited directly.
- Source skills live only under `.ruler/skills/`.
- Protocol-critical addresses and constants are part of the external surface; changes update README and a corresponding `audit_*.md` entry.
- Validator status names are part of the external surface; use exact casing from README consistently in code, docs, and errors.
- When an `audit_*.md` finding is resolved, update the entry in the same PR with closure reference; do not silently delete resolved items.
- `.ruler/AGENTS.md` documents workflow and process; modular `.ruler/*.md` files document technical rules per domain.
- User-visible breaking changes are documented with old behavior, new behavior, and operator migration guidance.

## Module README by tier

- Simple modules: module README is not required.
- Medium modules: module README is optional.
- Complex modules: module README is recommended.
- If the module uses a non-obvious hook/sink, document that in the module README.
- The root README should carry only a short summary of the tier model and a link to `.ruler/module_structure.md`; detailed structure rules live there, not in the root README.
- The module README must be updated in the same change when the module tier changes, when entrypoint kinds change (new or removed `precompile` / `rpc` / `lifecycle` / `hook` / `sink`), or when the cross-module API surface (`api.rs`) changes materially.
- A complex module’s README should name the tier and why it applies, list the entrypoint kinds and where they route, list cross-module dependencies and whether they go through `api.rs` or public runtime calls, and call out any non-obvious hook/sink or structural edge case.
- The module-structure rule must stay discoverable from the root README, from `.ruler/*`, and from any module README that references tier or structure.

See also: `.ruler/skills/readme-contract-audit/`, `.ruler/module_structure.md`



<!-- Source: .ruler/economics_numeric_rules.md -->

# Economics And Numeric Rules

- Do not use `f32` or `f64` in production execution paths for token amounts, rates, emissions, pricing, VWAP, stake, rewards, slashing, balances, or validator economics.
- Research code, tests, and temporary migration tooling may use floats only when isolated from on-chain state transitions.
- Use fixed-point integer arithmetic with an explicit scale factor documented at the declaration site (see README "Emission Model").
- Default economic amount/rate state to `U256`; conversion to a narrower bit width such as `u64` or `u128` must document the maximum value and target type in a code comment and be covered by tests.
- Verify claimed transfers, mints, burns, escrows, fee payouts, dust routing, and claim flows against real balance movement, not only returned amounts.
- Any allocation that funds a module address up-front must burn the pre-funded balance when the full amount is returned as excess to a terminal sink; failure to do so is the lockup pattern.
- Block 0 does not produce validator rewards; settlement skips block 0 explicitly rather than relying on an empty voter set.
- Dust from fee and emission splits (`fee_dust`, `emission_dust`, cap remainders) routes deterministically to terminal Metadosis (see README "Emission Model"); Rewards also burns `emission_dust` from its own backing balance so pending rewards cannot double-count returned terminal dust.
- Arithmetic in reward, fee, slashing, and emission paths uses checked or saturating operations, or documents why overflow is impossible under declared supply and participant bounds.
- For AgentReward cap tests, `input` is the pool amount before per-address cap; tests assert `sum(distributed) + returned_excess == input` for each pool (AgentReward 32%; see README "Emission Model" and `audit_agentreward.md`).
- Every mint has a matching burn path on revert or unused return; on-chain balance and in-storage counter parity is a tested invariant.
- Non-terminal sink handlers are module-owned static functions taking `BlockRuntimeContext` and amount.
- Non-terminal sink handlers must not hold long-lived state, spawn background tasks, read wall-clock time, or use random data.
- Emission constants live in a single module (`emission.rs`); precompile view methods read exported constants rather than duplicating literals.
- Oracle fallback paths (`unwrap_or_default`, missing-price, stale-price, zero-VWAP) are explicitly tested.
- `claimReward` zero-amount semantics must match README and the module README (currently: claim-all).

See also: `.ruler/skills/economics-integer-audit/`



<!-- Source: .ruler/module_structure.md -->

# Runtime Module Structure Standard

Permanent structure rules for runtime modules with persistent EVM state.

## Quick reference

- `schema.rs`: storage schema and record/types only.
- `state.rs`: local storage mutation helpers, CRUD, indexes, local transitions.
- `runtime.rs`: main business logic and orchestration.
- `precompile.rs`: ABI decode/dispatch/encode only (inbound ABI for this module).
- `sol_ext.rs`: `sol!` interface declarations for *external/outbound* contracts the module calls via `StorageHandle::call`.
- `lifecycle.rs`: thin block hook entrypoints delegating into `runtime.rs`.
- specialized hooks/sinks stay in dedicated files (`emission_sink.rs`, `<name>_hook.rs`, `<name>_sink.rs`).
- `errors.rs` is a baseline module file. Solidity events live inside the precompile's own canonical interface in `contracts/precompiles/src/I<Module>.sol` and reach Rust through the `sol!("…")` import in `precompile.rs`; modules do not keep a separate `events.rs` shim.
- tests start in `tests.rs` and move to `tests/` once multiple files improve navigation.

Adoption policy: do not introduce new runtime modules with `contract.rs`, `logic.rs`, `storage.rs`, or `orchestrator.rs` filenames. Migrate existing ones opportunistically when touched.

## 1. File dictionary: responsibilities and prohibitions

### `schema.rs`

Lives here:
- storage schema;
- records/entities;
- status/type enums that describe record or state-machine states;
- state layout.

Must not live here:
- orchestration/use-case logic;
- lifecycle hooks;
- ABI dispatch.

Meaning: answers **what does this module store?**

Example boundary: a state enum like `Status` belongs here; a period constant like `WAITING_PERIOD_HOURS` belongs in `constants.rs`.

### `state.rs`

Lives here:
- local storage/state operations;
- CRUD;
- field updates;
- index helpers;
- getters/setters;
- local state transitions.

Must not live here:
- top-level orchestration across several modules;
- lifecycle entrypoint wiring;
- ABI code.

Meaning: answers **how is this module’s storage read and mutated locally?**

Cross-module boundary: local state helpers are internal building blocks. Neighboring modules must not depend directly on another module’s `state.rs`.

### `migration.rs`

Lives here:
- schema/storage migration logic when schema evolution becomes a real subsystem;
- repair or transitional logic that should not pollute `schema.rs` or `state.rs`.

Must not live here:
- baseline storage schema declarations;
- normal runtime use-case logic.

When to add: optional; add it when there are 2 or more schema/layout versions that require explicit transformation logic.

Meaning: answers **where does schema evolution logic live once it is substantial?**

### `runtime.rs`

Lives here:
- main business logic;
- use-cases;
- orchestration within the module;
- coordination between `state.rs` and neighboring modules;
- helper functions used only by this module’s runtime flow.

Must not live here:
- ABI dispatch;
- block hook entrypoints;
- storage layout declarations.

Meaning: answers **what does this module do as a runtime/use-case layer?**

### `constants.rs`

Lives here:
- module-global constants used by schema/state/runtime;
- protocol/business constants such as periods, limits, or fixed labels that belong specifically to the module.

Must not live here:
- generic shared constants that belong in a shared crate;
- mutable runtime logic.

When to add: optional; add it when a module has a non-trivial set of module-local constants (for example `FORMING_PERIOD_HOURS`, `WAITING_PERIOD_HOURS`).

Meaning: answers **where do module-local constants live?**

Example boundary: `WAITING_PERIOD_HOURS` or `MAX_DAY_LIMITS_KEPT` belongs here; a record-state enum like `Status` does not.

### `precompile.rs`

Lives here:
- ABI decode;
- dispatch;
- ABI encode;
- the module's own **inbound** `sol! { interface ... }` ABI (e.g. `ICredisFactory`).

Must not live here:
- substantial business logic;
- long state mutation flows;
- orchestration;
- ABI of *external* contracts this module calls via `StorageHandle::call` (those live in `sol_ext.rs`).

Meaning: answers **how is this module called through the precompile ABI?**

Two equivalent styles for the inbound ABI surface are allowed:

1. **Manual** (legacy): an inline `sol! { interface I... { function ...; } }` block plus a hand-written `pub fn dispatch(...) -> Result<Bytes>` whose body is `dispatch_call(...)` + `match call { ... }` calling into `runtime.rs`. Existing modules use this form.
2. **Macro-driven** (preferred for new modules): one `#[contract_dispatch] impl ContractName<'_> { ... }` block whose methods are annotated with `#[contract_public("solidity signature")]`. The macro emits the private `sol!` interface and a drop-in `pub fn dispatch(...)`. Companion markers: `#[contract_view]` (read-only, no caller/value), no marker (default mutating: first param after `&mut self` is `caller: Address`), `#[contract_payable]` (caller + value before ABI args). Pilot lives in `crates/core/agentreward/src/precompile.rs`.

Both styles preserve the boundary above — `precompile.rs` still routes only; business logic stays in `runtime.rs`.

### `sol_ext.rs`

Lives here:
- `sol! { interface ... }` declarations for **external/outbound** contracts the module calls via `StorageHandle::call`;
- pure ABI type declarations with no orchestration.

Must not live here:
- the module's own inbound precompile ABI (that lives in `precompile.rs`);
- business logic, storage mutation, or dispatch.

When to add: optional; add it when the module makes sub-calls to one or more external contracts and declares their ABI via `sol!`. Inline `sol!` blocks in `runtime.rs` should move here once they exist.

Visibility default: `mod sol_ext;` (private). The generated types are consumed internally by `runtime.rs`; only promote to `pub mod` if another crate genuinely needs the same generated ABI types.

Meaning: answers **which external contract ABIs does this module call out to?**

### `rpc.rs`

Lives here:
- RPC-facing adapter/routing for a module-specific `outbe_*` namespace;
- parameter/response conversion for RPC;
- thin delegation into `runtime.rs` or query helpers.

Must not live here:
- core business logic;
- storage schema;
- lifecycle hooks.

When to add: optional; add it only when the module really exposes a separate `outbe_*` RPC surface.

Meaning: answers **how is this module called through its RPC surface?**

### `lifecycle.rs`

Lives here:
- `begin_block`, `end_block`, init hooks;
- thin lifecycle entrypoints into runtime logic.

Must not live here:
- the whole module’s business logic;
- storage helpers;
- ABI code.

Meaning: answers **when does this module run from block lifecycle?**

### `<name>_hook.rs` / `<name>_sink.rs`

Lives here:
- one or more specialized hook/sink entrypoints beyond the main ABI/lifecycle entrypoints;
- each hook/sink should get its own file when it has separate meaning.

Must not live here:
- general runtime flow;
- unrelated helpers.

When to add: optional; add one file per distinct hook/sink entrypoint.

Meaning: answers **where does a separate specialized hook or sink live?**

### `api.rs`

Lives here:
- traits and module-facing API contracts for cross-module calls;
- a stabilized public surface that is distinct from ABI/precompile routing.

Must not live here:
- storage schema;
- runtime implementation.

When to add: optional; add it when the module exposes a reusable cross-module API surface.

Meaning: answers **what is the intended cross-module/public API of this module?**

### `events.rs` (removed)

This file is no longer part of the module structure.

- Solidity events for the module's own precompile live inside its interface in `contracts/precompiles/src/I<Module>.sol`, alongside its functions and structs.
- Rust dispatch picks them up through the `sol!("…")` macro in `precompile.rs`, which exposes the interface as a module (`crate::precompile::IXxx`).
- Emit sites in `runtime.rs` (or sinks/hooks) `use crate::precompile::IXxx;` and write `self.emit(IXxx::EventName { … })`. Do not re-export events through a `crate::events` path.
- Outbound (cross-contract sub-call) event types that are *not* part of this module's precompile surface stay with their `sol_ext.rs` companion interface — they do not warrant a separate file either.

Meaning: there is no `events.rs`; events flow from the canonical `.sol` interface directly to the Rust emit site.

### `errors.rs`

Lives here:
- module-specific error enums/types beyond shared/common errors.

Must not live here:
- generic project-wide errors already owned by shared crates.

When to add: part of the baseline structure for all tiers.

Meaning: answers **where do module-local error types live?**

### `genesis.rs`

Lives here:
- genesis/import/export/init parameter shapes;
- genesis-only setup logic that is more than trivial default storage initialization.

Must not live here:
- ordinary runtime logic;
- ABI routing.

When to add: optional; add it when the module has non-trivial genesis inputs, seed data, bootstrap data, or import/export logic.

Meaning: answers **where does module genesis/init modeling live?**

### `lib.rs` / `mod.rs`

Lives here:
- module wiring;
- `pub mod ...` declarations;
- minimal public re-exports.

Must not live here:
- substantive business logic;
- large helper implementations.

Meaning: answers **how is the module assembled and what does it re-export?**

### `contract.rs` (legacy filename only)

- this is not a target filename in the standard;
- if a current `contract.rs` defines storage/state layout, migrate it to `schema.rs`;
- if a current `contract.rs` defines contract/API/trait surface for cross-module calls, migrate it to `api.rs`.

Must not live here: new code.

Meaning: legacy naming that should be removed over time.

### `tests.rs`

Lives here:
- all tests in one file while that remains readable.

Must not live here:
- large multi-file test organization once separation is already needed.

When to use: use it as the initial test file while one file is still readable; the moment you want 2 or more separate test files, switch to `tests/`.

Meaning: answers **where do tests live before a test directory is necessary?**

### `tests/mod.rs`

Lives here:
- test submodule wiring;
- very small shared test utilities.

Must not live here: large shared harness code.

When to add: add it once the module switches from `tests.rs` to `tests/`.

Meaning: answers **how are test submodules assembled?**

### `tests/common.rs`

Lives here:
- shared test fixtures;
- mock helpers;
- reusable builders/harness helpers for multiple test files.

Must not live here: actual test cases.

When to add: optional; add it when shared test setup is large enough that `tests/mod.rs` becomes noisy.

Meaning: answers **where does shared test harness code live?**

### `tests/state.rs`

Lives here:
- CRUD;
- layout compatibility;
- local state transitions;
- index behavior.

Must not live here: broad end-to-end scenarios.

Meaning: answers **where do state-level tests live?**

### `tests/lifecycle.rs`

Lives here:
- begin/end block behavior;
- bootstrap/init flow;
- cleanup;
- lifecycle transitions.

Must not live here: unrelated state-only tests.

Meaning: answers **where do lifecycle-hook tests live?**

### `tests/e2e.rs`

Lives here:
- full end-to-end scenarios;
- cross-module integration;
- user-visible flows.

Must not live here: low-level state-only assertions unless they directly support the e2e scenario.

Meaning: answers **where do integration/end-to-end tests live?**

## 2. Formal tier triggers

### Dominance rule
- If a **complex trigger** matches, the module is **complex**.
- Otherwise, if a **medium trigger** matches, the module is **medium**.
- Otherwise, the module stays **simple**.

### Entrypoint kinds
Recognized entrypoint kinds:
- `precompile`
- `rpc`
- `lifecycle`
- `hook/sink`

### Simple
A module stays **simple** only if all of these are true:
- exactly **1 entrypoint kind** exists;
- there are **0–1 record types** in schema;
- there is no top-level cross-module orchestration;
- state operations and use-cases still fit comfortably in one runtime file.

### Medium trigger
A module becomes at least **medium** if at least one of these is true:
- local storage helpers are reused by multiple use-cases;
- there are non-trivial indexes/helpers beyond basic CRUD;
- there is cross-module interaction with 1–2 neighboring modules through `api.rs` or public runtime calls, but the module is not the main coordinator.

### Complex trigger
A module becomes **complex** if at least one of these is true:
- there are **multiple entrypoint kinds** (2 or more from the list above);
- cross-module orchestration is central to the main use-case (one use-case coordinates 3 or more neighboring modules).

### Note on qualitative triggers
Some medium triggers are intentionally qualitative readability signals rather than hard metrics, especially:
- local storage helpers reused by multiple use-cases;
- non-trivial indexes/helpers beyond basic CRUD.

### LOC is a secondary signal only
If one file keeps growing while mixing entrypoint code, state ops, and orchestration, re-check the tier.

## 3. Recommended baseline structures

Baselines correspond to the tiers in §2. The tree blocks below omit `lib.rs` / `mod.rs`, because the module assembly file is always present.

### Simple module
```text
src/
  schema.rs
  runtime.rs
  precompile.rs
  errors.rs
  tests.rs
```

### Medium module
```text
src/
  schema.rs
  state.rs
  runtime.rs
  precompile.rs
  errors.rs
  tests.rs
```
or
```text
src/
  schema.rs
  state.rs
  runtime.rs
  precompile.rs
  errors.rs
  tests/
    mod.rs
    state.rs
    e2e.rs
```

For when to switch from `tests.rs` to `tests/`, see the `tests.rs` rule in §1.

### Complex module
```text
src/
  schema.rs
  state.rs
  runtime.rs
  precompile.rs
  lifecycle.rs
  <name>_hook.rs   # one file per distinct hook/sink; see §1
  errors.rs
  tests/
    mod.rs
    state.rs
    lifecycle.rs
    e2e.rs
```

### Optional files for any tier
```text
api.rs        # public surface: add when the module exposes a cross-module/public API surface
rpc.rs        # entrypoint: add when the module serves an `outbe_*` RPC namespace
migration.rs  # storage evolution: add when schema/storage migration logic becomes substantial
constants.rs  # module helpers: add when the module has module-local constants
genesis.rs    # setup: add when genesis/import/export/init shape is substantial
```

Optional files are added only when needed. They are not part of the baseline structure by default.

## 4. Anti-patterns

### Bad: business logic inside `precompile.rs`
```rust
fn dispatch(...) {
    // decode
    // business logic
    // storage writes
    // cross-module calls
    // encode
}
```

### Good
```rust
fn dispatch(...) {
    // decode
    runtime::process_call(...)?;
    // encode
}
```

### Bad: `runtime.rs` performs ABI decode directly
```rust
fn process_call(raw: &[u8]) {
    // selector parsing / abi decode here
}
```

### Good
```rust
// precompile.rs
fn dispatch(raw: &[u8]) -> Result<Bytes> {
    let call = decode_call(raw)?;
    runtime::process_call(call)
}
```

### Bad: one `runtime.rs` mixes state ops and use-cases
```rust
fn issue(...) {
    self.field_a.write(...)?;
    self.field_b.write(...)?;
}

fn run_begin_block(...) {
    // orchestration
}
```

### Good
```rust
// state.rs
fn issue_fields(...) -> Result<()> {
    self.field_a.write(...)?;
    self.field_b.write(...)?;
    Ok(())
}

// runtime.rs
fn issue(...) -> Result<()> {
    state::issue_fields(...)
}
```

### Bad: reaching into a neighbor through its `state.rs`
```rust
fn do_work(...) -> Result<()> {
    let value = other_module::state::read_value(...)?;
    self.field.write(value)
}
```

### Good
```rust
// runtime.rs
fn do_work(...) -> Result<()> {
    let value = other_module::api::read_value(...)?;
    state::write_local_value(value)
}
```

### Bad: `state.rs` calls neighboring modules
```rust
fn update_local_state(...) {
    other_module::runtime::do_work(...)?;
    self.field.write(...)?;
}
```

### Good
```rust
// state.rs
fn update_local_state(...) -> Result<()> {
    self.field.write(...)
}

// runtime.rs
fn orchestrate(...) -> Result<()> {
    other_module::api::do_work(...)?;
    state::update_local_state(...)
}
```

### Bad: `lifecycle.rs` becomes a second `runtime.rs`
```rust
fn begin_block(...) {
    // hundreds of lines of orchestration and cross-module calls
}
```

### Good
```rust
fn begin_block(...) {
    runtime::run_begin_block(...)
}
```

## 5. Structure self-check and tier-up signals

### 5.1 Tier-up signals
Raise a module to the next tier when any trigger from §2 now matches where it did not before. Typical codebase symptoms:
- `runtime.rs` now contains several clearly different responsibilities;
- a new entrypoint kind is added (for example, a hook appears alongside a precompile entrypoint);
- the module starts coordinating 3 or more neighboring modules in one central use-case.

### 5.2 Structure self-check
A new reader should be able to answer quickly:
- where is the schema?
- where is storage mutated?
- where is the runtime/use-case flow?
- which entrypoint kinds exist (`precompile`, `rpc`, `hook`, `lifecycle`) and where do they live?
- where is the cross-module API surface, if the module has `api.rs`?

If not, either refactor within the current tier (split responsibilities more clearly) or raise the tier if a §2 trigger now matches.

## 6. Visibility and re-export rules

- `lib.rs` / `mod.rs` assembles the module and re-exports only the intended public surface. The public surface of a module is what is intentionally re-exported from `lib.rs`; everything else should be restricted as much as possible.
- Default rule: items that are not part of the external or cross-module API should be `pub(crate)` or private.
- Default public surface goes through re-exports from `lib.rs`. If the module has `api.rs`, the cross-module API trait surface goes through `api.rs`; `lib.rs` re-exports `api.rs` as part of the public surface.
- `state.rs`, `runtime.rs`, `migration.rs`, `constants.rs`, and `sol_ext.rs` default to `pub(crate)` (declared in `lib.rs` as `mod ...;` without `pub`). Specific items are promoted to `pub` only through explicit re-exports from `lib.rs` when needed.
- `schema.rs` and entrypoint files (`precompile.rs`, `rpc.rs`, `lifecycle.rs`, `<name>_hook.rs` / `<name>_sink.rs`) typically expose specific public items (for example: types used in API signatures, entry functions, the `sol!`-generated interface module that carries the event types). Only those items should be `pub`; the rest should stay `pub(crate)`.
- In a multi-crate workspace, `pub(crate)` stops at the crate boundary. Items needed by other crates must be `pub` and should go through the module’s re-exports or `api.rs`.

See also: `.ruler/docs_contract.md` (module README by tier), `.ruler/storage_dsl.md` (schema evolution and pre/post-genesis compatibility).



<!-- Source: .ruler/reth_sdk_integration.md -->

# Reth SDK Integration

- Outbe uses Reth SDK components inside a single binary with in-process consensus/execution integration.
- Engine calls (`new_payload`, `forkchoice_updated`, payload builder) go through in-process Reth engine handles, never HTTP serialization.
- Do not copy Reth's default Ethereum CL/EL deployment assumptions into Outbe.
- Use Reth as a reference for SDK patterns, Node Builder components, EVM configuration, payload building, txpool behavior, provider APIs, and RPC extension points.
- When a Reth API shape or behavior is unclear, verify against the `Cargo.toml` pinned `paradigmxyz/reth` revision; inspect a local Reth checkout only when semantics cannot be confirmed from the pinned revision metadata or docs.
- Prefer Reth extension traits and builder patterns over forking or reimplementing Reth internals.
- Custom `outbe_*` RPC methods register through Reth's extension surface, not a parallel router.
- If a change affects CLI behavior, payload construction, Engine handle calls, txpool admission, provider state reads, or RPC output, add targeted verification and update README when user-visible.
- The payload builder commits `OutbeBlockArtifacts` in `header.extra_data` before sealing; any `extra_data` change changes the block hash, so validators recompute and reject on mismatch (see README "Architecture").
- `mixHash` / `prev_randao` is sourced from Outbe's VRF seed or genesis round-robin exception, not Reth's default Ethereum randomness (see README "Architecture").
- The Outbe txpool uses ZeroFee admission and deterministic priority classes; admission or priority changes require proposer/validator parity verification.
- Pre-execution block hooks and begin-zone system transactions run under deterministic executor ordering before user transactions and before Reth's parallel state-root task observes those writes.
- Finalized-metadata settlement loads historical blocks via the Reth provider by `(number, hash)`.
- Provider borrows are short-lived and not held across consensus or async boundaries.
- Outbe precompile addresses are marked touched before state-root computation to preserve them under EIP-161 semantics (see README "EVM").
- Reth ExEx is observability/indexing only; consensus-critical or validator-settlement logic must not run inside an ExEx.

See also: `.ruler/skills/reth-sdk-change-review/`



<!-- Source: .ruler/rust_quality.md -->

# Rust Quality Rules

- Keep Rust changes focused and crate-local unless cross-crate behavior genuinely changes.
- Prefer explicit public crate APIs with private internal modules over broad re-exports.
- Avoid ambiguous bool or `Option` positional arguments in new APIs when enums, builders, newtypes, or named methods make call sites clearer.
- Avoid growing high-touch orchestration files when a cohesive new module would keep invariants closer to tests.
- Make `match` statements exhaustive when feasible and avoid wildcard arms for consensus/runtime state machines.
- Use structured errors in runtime, consensus, RPC, precompile, hook, and node startup paths.
- Prefer `thiserror` derivations with `#[non_exhaustive]` for error types whose variants can evolve.
- Do not add panics, `unwrap()`, `expect()`, `assert!`, `debug_assert!`, `todo!`, `unimplemented!`, or `unreachable!` in consensus, runtime, execution, precompile, hook, RPC, or node-startup paths.
- `HashMap` and `HashSet` are forbidden on consensus-visible paths; use `BTreeMap` / `BTreeSet` when iteration order or byte-for-byte encoding matters.
- Narrowing integer casts via `as` are not used in consensus, runtime, precompile, or hook paths; use `try_into()` or document that the value cannot exceed the target type range and add a boundary test.
- `unsafe` is not used in consensus, runtime, execution, precompile, hook, or RPC paths except for documented FFI.
- Wall-clock time (`SystemTime::now()`) is not used on consensus-visible paths; consensus logic takes timestamps from `BlockContext`.
- Non-deterministic randomness (`thread_rng`, `OsRng`, `fastrand`) is not used as consensus randomness; consensus randomness comes from VRF-derived seeds. `OsRng` is allowed only for protocol-required cryptographic secret material generation (for example key/DKG dealer randomness) or verifier-side Commonware BLS batch verification where the upstream API requires unpredictable scalar weights. Such RNG use must be wrapped or commented with its cryptographic purpose and must never feed VRF seed derivation, leader election, `prev_randao`, metadata encoding, or deterministic state transitions.
- `block_on()` inside an async context is not used.
- Protocol constants are `const` or `const fn`; runtime lazy init is not used for consensus state.
- New stateful runtime contracts are generated via the `#[contract]` macro from `outbe-macros`; hand-rolled facades are justified in a comment.

See also: `.ruler/skills/rust-targeted-verification/`



<!-- Source: .ruler/security_adversarial_review.md -->

# Security And Adversarial Review

- Review consensus, p2p, cryptography, precompile, RPC, and economics changes as adversarial surfaces.
- Validate authorization, caller identity, value handling, replay resistance, malformed input, duplicate input, and missing-state paths explicitly.
- Do not rely on comments, README text, or happy-path tests as proof of safety.
- For distributed or consensus behavior, test degraded networks, missing data, stale committees, byzantine evidence, and restart/recovery where practical.
- For persistent state, prefer atomic or recoverable processing.
- Never silently delete or overwrite user-owned state during partial processing.
- Log unrecoverable failures with block number/hash, epoch/view, tx hash, validator address or caller, module name, and error class when available.
- Logs, error messages, panics, and RPC responses must not leak BLS private keys, threshold shares, DKG polynomials, TEE keys, salts, or session secrets.
- Consensus signatures bind `chain_id`, epoch, and ordered validator-set commitment.
- Slashable byzantine evidence is accepted only through documented chain-artifact channels and must be reproducible from chain state, not node-local memory.
- TEE-decrypted values using `TEE_PRIVATE_KEY` and `TEE_SALT` must produce byte-identical output across proposer and every validator (see `crates/core/tributefactory/src/crypto.rs`).
- Precompiles that do not accept native token value must reject `msg.value != 0` before any state read or write.
- `admin_*` and `debug_*` RPC methods require authentication or a local-only socket.
- Production RPC must not be exposed unauthenticated on public interfaces.
- `outbe-cli` must not transmit key material to a remote RPC.
- Initial DKG ceremony failure is fail-fast through `run_initial_dkg(...).await.wrap_err("DKG ceremony failed")?` and validator consensus-thread shutdown; update this rule if startup retry or operator-restart behavior is introduced (see `crates/blockchain/consensus/src/stack.rs` and `bin/outbe-chain/src/main.rs`).

See also: `.ruler/skills/precompile-runtime-audit/`, `.ruler/skills/consensus-determinism-review/`



<!-- Source: .ruler/storage_dsl.md -->

# Storage DSL Rules

Use the storage DSL when a contract has a clear entity model with one primary
key and multiple fields stored in parallel mappings.

## Preferred shape

- `#[storage_schema]` for contract storage facades.
- `#[storage_record(exists_field = ...)]` for entity records.
- exactly one `#[key]` per record.
- `#[attribute(order = N, ...)]` for every schema field that participates in
  stable layout.

## Layout rules

- Never reorder live storage by changing physical slot meaning.
- `order` is a logical ordering hint; generated global slots must remain stable.
- When removing a field from a live layout, reserve its position with
  `deprecated = true`.
- Use explicit nullable types (`Optional<T>`) instead of implicit zero-as-none
  when absence must be distinguished from zero.

## Existence rules

- `exists` must be explicit and field-based via `exists_field`.
- Do not hide existence in macro-generated sentinels.
- If no natural field exists, add one to the record schema yourself.

## Mixed-mode rules

- DSL records may coexist with low-level `Mapping`, nested mappings,
  keccak-composite keys, sparse indexes, and circular buffers.
- Do not force specialized indexing structures into entity-record form.

## API rules

- Prefer `create(record)` and `update(record)` over ambiguous upsert semantics.
- Preserve field-level accessors on entries for gas-efficient partial updates.
- Keep the record key materialized in the in-memory struct via `#[key]`.

## Pre/post-genesis compatibility

- Before genesis/mainnet: schema and storage layout may change freely (add/remove/rename/retype) unless another project-local compatibility constraint already applies.
- After genesis/mainnet: layout changes must preserve backward compatibility for any live storage/state. New fields must be optional or have safe defaults; removed fields remain represented through deprecation or reserved positions where the chosen storage format requires it; type changes require explicit migration logic.
- Evolution rules specific to the chosen storage/serialization format (optional fields, deprecated markers, reserved positions, format-specific compatibility tools) are documented next to the schema.
- Migration/repair logic does not live in `schema.rs`. Small cases live in `state.rs`; once substantial, they move to `migration.rs` (see `.ruler/module_structure.md` §1).
- Cross-version schema-evolution testing is not fully specified by this document. When backward compatibility becomes mandatory after genesis, migration and schema changes ship with compatibility tests for the old format/state.

See also: `.ruler/module_structure.md`



<!-- Source: .ruler/storage_handle.md -->

# StorageHandle Rules

- Persistent runtime state access must receive an explicit scoped `StorageHandle`.
- Do not hide state behind implicit context, `Contract::default()`, process globals, long-lived service objects, or background/async tasks.
- Contract facades must be short-lived and constructed via `storage.contract::<T>()`, `ctx.contract::<T>()`, `Contract::new(storage)`, `Contract::at(storage, address)`, or typed `storage.rs` accessors.
- `Contract::default()` must not be used for persistent runtime state.
- Facades are scoped to a single precompile call, hook invocation, or block-boundary action.
- Facades must not outlive the handle's execution scope or escape into long-lived struct fields.
- Before changing `StorageHandle`, generated facades, storage primitive lifetimes, or storage wrapper ownership, run the pre-survey in `AGENTS.md` section 8.1.
- The pre-survey specifically hunts `StorageHandle<'static>`, `Arc`/`Box`/`Mutex`/`OnceCell`/`static` wrapping `StorageHandle`, and storage handles outside `BlockRuntimeContext`, storage primitives, `CheckpointGuard`, or test fixtures.
- Treat `read_all()` as materialization; it is acceptable for tests, admin/debug paths, and explicitly bounded or capped collections.
- Hot runtime paths over unbounded `StorageVec`/`StorageSet` must use pagination, index iteration, or an inline justification with a documented size bound.
- Preserve compile-fail coverage for provider-scope escape, `!Send` thread-spawn rejection, and `'static` facade escape.
- `StorageHandle` must never be captured by `std::thread::spawn`, `tokio::task::spawn`, or `spawn_blocking`.
- `storage.clone()` clones the `Rc` handle, not EVM state or provider; all clones share the same journal/checkpoint, lifetime, and non-thread-safe boundary.
- Consensus-relevant state lives only in EVM storage through `StorageHandle`.
- Slot 0 is reserved for storage schema version; migrations increment this version rather than re-using retired slots (see README "Upgrades").
- Within a single block execution, writes through a `StorageHandle` are visible to later reads from the same handle.

See also: `.ruler/skills/storage-handle-survey/`



<!-- Source: .ruler/testing_harnesses.md -->

# Testing And Harness Map

- Use targeted tests before workspace-wide tests.
- Default test runner is `cargo nextest run`. Install once with `cargo install --locked cargo-nextest` (or `mise run nextest-install`); CI images must provide it before running test targets.
- `cargo nextest run` does not execute doctests. Run `cargo test --doc` (workspace: `mise run test-doc`) whenever the touched crate's public API has executable doc-examples.
- Consensus changes start with `cargo nextest run -p outbe-consensus`.
- Storage lifetime or facade changes run `cargo nextest run -p outbe-primitives --test trybuild`.
- The storage trybuild harness keeps compile-fail coverage for provider-scope escape, `!Send` thread-spawn rejection, and `'static` facade escape.
- Node startup, restart, or localnet flow changes run the localnet harness: `mise run localnet-bootstrap`, `mise run localnet-start`, `mise run localnet-status`, and `./scripts/run-testnet.sh stop`. A 4-validator localnet that reaches a non-zero block height on every node is the restart / startup smoke signal.
- Runtime economics, precompile, and hook changes need crate tests plus integration or execution-level tests when behavior crosses module boundaries.
- Fuzz or conformance-style tests are appropriate for encoding, decoding, artifact formats, consensus metadata, and storage/wire compatibility.
- Before committing, run `cargo fmt --all --check`.
- Before opening or updating a PR, run `cargo clippy --all-targets -- -D warnings` while respecting existing workspace lint configuration.
- State migration and hard-fork activation changes require migration tests plus deterministic replay across the fork boundary.
- Consensus/execution-boundary parity tests use the same parent block/header, finalized-parent tx, `extra_data`, validator-set snapshot, transaction list, chain spec, and starting storage state on both paths; assert equal post-block state root, event log, and account balance deltas.
- Tests must be isolated: no shared `/tmp` paths, no global mutable singletons left mutated, and no reliance on previous-test side effects.
- Assertion text must not depend on wall-clock timestamps, PIDs, or random values.
- Flaky tests are quarantined with `#[ignore]` plus a tracking issue within one PR cycle of discovery rather than retried until green.
- Deterministic encoding, header hash, state-root computation, and fee calculation are good candidates for property tests (`proptest`/`quickcheck`).

## Audit toolchain (entry points)

Use `mise run audit-*` as the primary entry point — these wrap the same commands the audit skill prescribes:

- `mise run audit-tools-install` — install the full toolchain (cargo-nextest, machete, deny, audit, llvm-cov, llvm-lines, bloat, udeps, geiger; nightly + miri/rust-src; llvm-tools-preview, clippy, rustfmt).
- `mise run audit-quick` — fast cycle: `cargo clippy --all-targets --all-features -- -D warnings` + `cargo nextest run --workspace`.
- `mise run audit-full` — slow cycle: `cargo machete` + `cargo deny check` + `cargo audit` + `cargo +nightly udeps`.
- Per-tool: `mise run audit-deny`, `mise run audit-rustsec`, `mise run audit-machete`, `mise run audit-udeps`, `mise run audit-miri`, `mise run audit-geiger`, `mise run audit-bloat`, `mise run audit-llvm-lines`, `mise run audit-coverage`, `mise run audit-bench`.

Tool tiers (must-have / nice-to-have / unsafe-only):

- Must-have for skill phases 3.1, 3.4, 3.5: `clippy`, `cargo-machete`, `cargo-deny`, `cargo-audit`, `proptest`, `criterion`.
- Nice-to-have: `cargo-udeps` (precise but nightly), `cargo-geiger`, `cargo-llvm-lines`, `cargo-bloat`, `cargo-mutants`, `cargo-nextest`.
- Required when `unsafe` blocks appear: nightly toolchain + `cargo +nightly miri test --lib`.
- Required on consensus-critical paths: `proptest` + property-based invariant tests + cross-platform CI (x86 + ARM).

Supply-chain review uses `cargo-vet` under `supply-chain/` (already configured); `cargo-deny` complements it for license / advisory / source bans.

See also: `.ruler/skills/rust-targeted-verification/`, `.ruler/skills/localnet-smoke-harness/`, `.ruler/skills/rust-module-audit/`
