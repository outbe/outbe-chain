
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
- `contracts`
    - Solidity smart contracts
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

## 5. Numeric Rules

1. Do not use `f32` or `f64` in production code
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

## 10. Testing Expectations

When fixing a bug, prefer:
- one regression test that proves the bug is closed
- one happy-path test if behavior changed
- one edge-case test if the bug was boundary-condition dependent

When touching cross-module flows, add more than unit tests:
- integration test
- execution-level test
- or end-to-end flow test

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
- Do not write code comments describing old state and what was changed. Just write meaningful short comment with no old references. 

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
