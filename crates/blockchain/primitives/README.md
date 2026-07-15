# outbe-primitives

`outbe-primitives` contains shared runtime types used across the Outbe execution,
consensus, precompile, and block lifecycle code.

This crate should stay low-level. It must not contain business-module policy for
rewards, staking, metadosis, oracle, tribute, or other custom modules.

## Module Map

- `addresses.rs`
  Fixed system/precompile addresses.
- `block.rs`
  Block lifecycle context and `BlockLifecycle` trait.
- `chain.rs`
  Chain constants and chain identity helpers.
- `consensus.rs`
  In-process consensus/execution bridge types.
- `consensus_metadata.rs`
  Metadata transaction envelope shared by consensus and execution.
- `crypto.rs`
  Shared crypto helpers.
- `dispatch.rs`
  Precompile dispatch helper types.
- `error.rs`
  Runtime/precompile error type and `Result` alias.
- `participation.rs`
  Validator participation encoding/decoding.
- `reshare_artifact.rs`
  `OutbeBlockArtifacts` encoding for execution summaries and consensus header
  artifacts.
- `storage/`
  Explicit storage access layer for precompiles, block lifecycle, tests, and
  state readers.
- `time.rs`
  Time constants/helpers.

## Storage Model

Persistent runtime state must be accessed through an explicit `StorageHandle`.
There is no implicit thread-local storage path and no `Contract::default()` path
for persistent state.

`StorageHandle<'storage>` is not a thread-safe shared storage object. It is a
scoped, single-execution-context capability over the current storage provider.
Do not send it to other threads, store it in long-lived objects, or use it from
background/async tasks. It may be cheaply cloned into short-lived contract
facades only inside the current transaction, precompile call, block lifecycle,
or test storage scope.

`storage.clone()` is not test-only. Runtime code may use it when one scoped
execution context needs to build several contract/storage facades or pass the
same storage capability to several helpers. Cloning does not clone EVM state,
does not create another provider, and does not create an independent execution
view. It is an `Rc` handle clone pointing at the same provider, journal, and
checkpoint state.

The lifetime is part of the contract: a handle cannot outlive the provider scope
that created it, cannot be stored in `'static` facades, and cannot cross thread
boundaries. The implementation uses `Rc<RefCell<...>>`, so it is intentionally
`!Send`/`!Sync`; this matches the single-threaded execution model for runtime
storage access.

Storage APIs must keep `RefCell` borrows internal to one operation. Public APIs
must not return `Ref`, `RefMut`, or iterators that hold provider borrows across a
method boundary. Use index-based reads, materialized collections, or callbacks
that open and close storage access per item.

StorageHandle review guardrails:
before changing `StorageHandle`, generated contract facades, or storage
primitive lifetimes, run a read-only pre-survey for long-lived storage/facade
ownership. Look specifically for `StorageHandle<'static>`, `StorageHandle`
inside `Arc`/`Box`/`Mutex`/`OnceCell`/`static`, contract facades stored in
long-lived struct fields, and storage handles outside `BlockRuntimeContext`,
storage primitive wrappers, `CheckpointGuard`, or test fixtures. If any such
runtime owner is found, stop and update the refactor scope before editing code.

read_all() is a materialization API. It is acceptable for tests, bounded
arrays, admin/debug/read paths, or explicitly capped collections. Do not use it
in hot runtime paths over unbounded `StorageVec`/`StorageSet` data without a
cap, pagination/index iteration, or a written justification.

Storage lifetime safety must remain covered by compile-fail tests. Keep tests
for provider-scope escape, `!Send`/thread-spawn rejection, and `'static` facade
escape. The provider-scope test is a standard borrow-checker check; the `!Send`
and `'static` facade tests protect the architecture contract.

Use one of these forms:

```rust
let mut rewards = storage.contract::<Rewards>();
let validator_set = storage.contract::<ValidatorSet>();
let custom = storage.contract_at::<SomeContract>(custom_address);
```

or inside block lifecycle code:

```rust
let mut rewards = ctx.contract::<Rewards>();
```

The `#[contract(addr = ...)]` macro implements `StorageBacked` for fixed-address
contract facades, which is what enables `storage.contract::<T>()` and
`ctx.contract::<T>()`.

`Contract::new(storage)` and `Contract::at(storage, address)` are still valid
when that is clearer locally, but new code should prefer `storage.contract::<T>()`
or `ctx.contract::<T>()` when the default address is intended.

Do not store `StorageHandle` or contract facades in long-lived Rust objects,
global state, background services, caches, or async tasks. They are scoped
runtime capabilities for the current transaction, block lifecycle, or test
storage provider.

## Storage Providers

The storage provider trait is `PrecompileStorageProvider`.

Main implementations:

- `storage::evm::EvmStorageProvider`
  Production EVM/precompile storage provider.
- `storage::direct::DirectStorageProvider`
  Direct state provider used by block lifecycle and executor-owned state hooks.
- `storage::readonly::ReadOnlyStorageProvider`
  Read-only provider for consensus-side state reads.
- `storage::hashmap::HashMapStorageProvider`
  In-memory test provider.

Use `StorageHandle::new(&mut provider)` when an entrypoint owns a provider
directly:

```rust
let storage = StorageHandle::new(&mut provider);
let validator_set = storage.contract::<ValidatorSet>();
```

For tests, prefer `HashMapStorageProvider::enter`:

```rust
let mut provider = HashMapStorageProvider::new(chain_id);
provider.enter(|storage| {
    let mut rewards = storage.contract::<Rewards>();
    let _genesis_day = rewards.genesis_utc_day.read()?;
    Ok::<_, PrecompileError>(())
})?;
```

## Block Artifacts

`header.extra_data` carries `OutbeBlockArtifacts`:

```rust
pub struct OutbeBlockArtifacts {
    pub execution_summary: Option<ExecutionSummaryArtifact>,
    pub consensus_header_artifact: Option<ConsensusHeaderArtifact>,
}

pub struct ExecutionSummaryArtifact {
    pub total_emission_limit: U256,
    pub validator_reward_cap: U256,
    pub validator_fee_sum: U256,
}
```

Rules:

- non-genesis execution blocks must commit an execution summary artifact;
- validators recompute the current block summary and reject mismatches;
- finalized validator settlement loads the finalized block header by
  `(number, hash)` and uses the summary committed there;
- finalized-parent attestations must not carry money fields;
- DKG / reshare artifacts share the same `OutbeBlockArtifacts` container.

## Checkpoints

Use `StorageHandle::with_checkpoint` when a group of writes must be atomic:

```rust
storage.with_checkpoint(|| {
    sink.apply(amount)?;
    Ok(())
})?;
```

If the closure returns `Err`, the checkpoint guard is dropped without commit and
writes in that checkpoint are reverted. The error is still returned to the
caller unless the caller intentionally handles it.

EmissionLimit uses this behavior for sink policy: non-terminal sink failures are
rolled back and converted into terminal sink fallback; terminal sink failure is
fatal to the block lifecycle.

## Block Lifecycle

Block lifecycle modules must implement `BlockLifecycle` and receive
`BlockRuntimeContext`.

```rust
pub struct MyLifecycle;

impl BlockLifecycle for MyLifecycle {
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        let mut module = ctx.contract::<MyContract>();
        module.process(ctx.block.timestamp)?;
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}
```

Lifecycles without end-block output use `EndBlockResult = ()`. Lifecycles that
seal or otherwise derive block-associated data return a typed value from
`end_block`.

`BlockContext` contains block metadata:

- `block_number`
- `timestamp`
- `chain_id`
- `proposer`
- `validators`

`BlockRuntimeContext` wraps `BlockContext` with the current scoped
`StorageHandle`.

Do not add block lifecycle APIs that take ad hoc positional arguments such as
`(timestamp, block_number)` when a `BlockRuntimeContext` is available.

## Precompile Dispatch

State-changing precompile dispatch functions should accept `StorageHandle`
explicitly:

```rust
pub fn dispatch(
    storage: StorageHandle,
    input: &[u8],
    caller: Address,
    value: U256,
) -> Result<Vec<u8>> {
    let mut contract = storage.contract::<MyContract>();
    // decode, validate, mutate
}
```

View-only precompiles still receive storage explicitly so the call surface stays
uniform and can read state deterministically.

## Numeric Rules

Do not use `f32` or `f64` for runtime economics, balances, emissions, rates,
stake, rewards, pricing, VWAP, or slashing.

Use integer/fixed-point arithmetic, normally `U256`, with explicit scale factors.

## Anti-Patterns

Do not:

- create implicit storage context APIs
- reintroduce `Contract::default()` for persistent state
- keep consensus-relevant mutable state in Rust singletons or caches
- hide block lifecycle inputs behind positional argument lists
- add business-module allocation policy to this crate
- perform non-deterministic work in storage/precompile/block lifecycle helpers

## Good Default Pattern

For new stateful runtime code:

```rust
pub fn apply(ctx: &BlockRuntimeContext, amount: U256) -> Result<()> {
    let mut module = ctx.contract::<MyContract>();
    ctx.with_checkpoint(|| {
        module.apply(amount)?;
        Ok(())
    })
}
```

This keeps storage explicit, preserves rollback semantics, and avoids
process-local state.

## Storage DSL Layer

On top of `Slot`, `Mapping`, `StorageVec`, and `StorageSet`, the primitives crate
now exposes an entity-oriented storage DSL in `outbe_primitives::storage::dsl`.

### Public types

- `Value<T>`
- `Map<K, V>`
- `List<T>`
- `Set<T>`
- `Optional<T>`
- `Deprecated<T>`
- `RecordEntry<'storage, K, V>`
- `StorageRecord`

### `Map<K, V>` behavior

`Map<K, V>` intentionally supports two modes:

1. `Map<K, scalar>`
   - behaves like a typed keyed scalar mapping;
   - exposes direct slot operations such as `read` / `write`.

2. `Map<K, Record>`
   - requires `Record: StorageRecord`;
   - exposes entity-oriented operations such as `exists`, `get`, `create`, `update`, `delete`, and `entry`.

### Nullability and evolution

- use `Optional<T>` when a field may be truly absent and zero is not enough;
- use `Deprecated<T>` together with `deprecated = true` metadata to reserve old
  layout positions during schema evolution.

### Design rule

The DSL is additive, not exclusive. When a structure is best represented by
nested mappings, composite keccak keys, or specialized indexing structures,
keep using the low-level storage primitives directly.
