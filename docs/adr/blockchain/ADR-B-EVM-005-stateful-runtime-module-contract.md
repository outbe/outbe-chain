# ADR-B-EVM-005: Stateful Rust precompiles mutate through scoped storage, generated dispatch, and explicit checkpoints

- **Status:** Proposed (documents the observed current implementation)
- **Date:** 2026-07-17
- **Scope:** `crates/blockchain/primitives`, `crates/blockchain/macros`, EVM precompile wiring, every `crates/core/*` and `crates/system/*` stateful module
- **Depends on:** ADR governance, ADR-B-CNS-003
- **Related:** all System/Core module-owner ADRs and ADR-B-OCD-006 and ADR-B-OCD-007 body lifecycle

## Context

Most Outbe protocol and business logic is implemented as native Rust precompiles,
not Solidity contracts. Their storage nevertheless participates in the EVM journal,
receipts and state root. A module can only pass an architecture review if its effective
interface includes ABI-generated dispatch, raw storage facades, lifecycle hooks,
cross-precompile calls, events and test adapters—not only its public Rust methods.

## Decision

### External mutation seam

Externally callable methods are declared with `#[contract_dispatch]` and
`#[contract_public]`. Macro generation:

- builds the Solidity ABI interface and decodes one selector/call;
- classifies each method as view, mutating or payable;
- passes EVM caller to mutating methods and caller/value to payable methods;
- rejects value globally when no payable method exists;
- routes the result through shared view/mutate encoders
  (`macros/src/dispatch_codegen.rs:42-230`).

The generated `dispatch(storage, data, caller, value)` plus the registered
precompile address is the production transaction interface. ABI methods must not
accept redundant authority fields when caller identity is authoritative; any
delegated/owner path is an explicit guard in the module command.

### Storage authority

`StorageHandle<'storage>` is the only general runtime storage capability. It wraps
one mutable `PrecompileStorageProvider`, is lifetime-invariant, cloneable only
within that scope, and fails on overlapping mutable borrow
(`primitives/src/storage/handle.rs:22-61`). `StorageBacked` facades make the
dependency explicit and support the fixed default address or deliberate
`contract_at` address (`storage/mod.rs:168-179`).

Persistent reads/writes, balances, logs, gas and subcalls go through the same
provider. STATICCALL write protection is enforced by provider/handle operations;
it is not left to each business module. Raw `Map`, `Slot`, `StorageVec`, record
entries and `contract_at` remain part of the effective internal interface and must
not let callers reproduce another module's invariants.

### Atomicity and errors

`StorageHandle::with_checkpoint` opens an RAII journal checkpoint, commits only
after closure success and reverts on `Err`/early return
(`storage/handle.rs:208-227`). Ordinary EVM/precompile dispatch inherits the
transaction journal. Multi-module lifecycle orchestration opens an additional
checkpoint when it promises all-or-nothing behavior.

`PrecompileError` classifies:

- user/domain rejection (`Revert`/`RevertBytes`);
- gas and STATICCALL failures;
- node-local body/tree unavailability or request deadline;
- deterministic body corruption;
- CE transaction/block capacity;
- subcall status;
- unsupported provider operation;
- fatal invariant failure (`primitives/src/error.rs:9-74`).

Owning adapters must map these classes consistently: user rejection becomes a
transaction receipt/revert, local inability must not become a false consensus vote,
and fatal/corrupt finalized execution fails the block/node. Catching an error after
partial writes is forbidden unless the same checkpoint restores semantic pre-state.

### Cross-module calls and reentrancy

`StorageHandle::call/staticcall` enters a child EVM frame and preserves raw revert
bytes or typed halt status. A mutable provider borrow remains held during the
subcall; hostile re-entry that tries to borrow the same provider is rejected as
`ProviderBorrowed` (`storage/handle.rs:268-380`).

This borrow gate is a structural reentrancy defense, but it is not permission to
leave an aggregate temporarily inconsistent before a subcall. A module command
must preserve invariants at every externally observable call boundary or keep the
entire effect plan behind a checkpoint with no callback path.

### Lifecycle seam

Block-boundary mutations use `BlockLifecycle` and a typed context as defined by
ADR-B-CNS-003. A module-specific context may add least-authority capabilities such as
parent-body readers or CE execution scope. It may not expose the raw provider or
invent an alternate unscoped hook path.

## Generic command FSM

Every mutating module command is specified as:

```text
Decoded(caller, value, canonical arguments)
  -> authority/provenance guard
  -> load and strictly decode current aggregate
  -> validate current-state/event/time/index guards
  -> compute deterministic effect plan
  -> open/own required checkpoint
  -> apply record + index + balance + subcall + event effects
  -> consume typed receipts / propagate error
  -> commit and return canonical semantic result
```

Outcomes are exhaustive: committed domain outcome, retryable/local execution error,
or fatal invariant/corruption error. A `bool`, zero amount, ignored `Result`, hidden
create-if-missing behavior or best-effort effect is not an acceptable receipt unless
the owning ADR types and tests its exact semantics.

## Generic persisted invariants

Every module ADR must instantiate these requirements:

- persisted enum/status tags decode through a closed set and malformed values fail
  closed;
- record existence and every secondary index/counter equivalence have one owner;
- create/update/delete maintain record and index membership in one checkpoint;
- raw/default zero is distinguished from a valid domain value where ambiguity is
  possible;
- timestamps/heights are canonical block context, not wall-clock time;
- balance changes use checked arithmetic and share rollback with the command;
- events describe committed outcomes and are removed on revert;
- storage-schema/version activation is fork/genesis-governed, never inferred from
  whatever slots happen to exist.

## Side-effect ledger

| Effect | Owner | Atomicity domain | Receipt/error | Replay |
|---|---|---|---|---|
| Slot/map/record mutation | owning module facade | EVM journal/checkpoint | propagated `Result` | transaction replay |
| Native balance transfer | storage provider/module command | same EVM journal | success/error; checked amount | nonce/command semantics |
| Canonical event | owning command/lifecycle | same journal + receipt | encoded log or error | removed on revert |
| Precompile subcall | caller module + child EVM frame | nested journal | returndata/revert/halt | owning command policy |
| Off-chain body mutation intent | compressed-body lifecycle | tx CE checkpoint + canonical event | commitment/typed lifecycle result | intent-bound ADR-B-OCD-006 and ADR-B-OCD-007 rules |
| External asynchronous effect | owning specialized ADR | outside EVM unless proven otherwise | outbox/receipt required | explicit idempotency required |
| Metric/trace/sidecar journal | diagnostics | non-transactional | best effort only when stated | may repeat/disappear |

## Determinism and bounded execution

All consensus-visible selection uses canonical ordering. `HashMap`/filesystem/
Mongo natural order, wall-clock time and process-local cache history cannot choose
state. Scans and cleanup require a cap plus deterministic cursor/progress policy;
`read_all()` is not acceptable on an unbounded collection. Gas is necessary but
does not by itself define starvation or partial-progress semantics.

## Production and test interfaces

The production seam is ABI dispatch under the real EVM provider and block/tx
journal. `HashMapStorageProvider` and direct facades are useful adapters but do not
prove production rollback, STATICCALL, nested-call, gas, receipt or state-root
behavior. Each stateful module needs:

- legal/illegal transition tables through generated dispatch or an equivalent
  production command interface;
- corrupt tag/index tests;
- failure injection after each distinct write/subcall boundary with semantic
  pre-state comparison;
- duplicate/retry/terminal replay and same-key/different-intent cases;
- cap/cursor/order/starvation evidence;
- integration tests for cross-module effects;
- proposer/validator parity when consensus-visible;
- an independent stateful model for non-trivial FSMs.

## Consequences

- Modules share one transaction/storage vocabulary and can compose atomically.
- Raw facades remain powerful and must be kept private/narrow to preserve architectural guarantees
  locality.
- Generated ABI reduces handwritten dispatch drift but expands the effective
  interface auditors must inventory.
- Test adapters cannot substitute for production transaction evidence.

## Rejected alternatives

### Process-global implicit storage context

Rejected because authority, lifetime and nested transaction ownership become
invisible to callers and tests.

### Public repository trait for every module

Rejected without two real adapters; it widens interfaces and encourages tests past
the production seam.

### Treat gas exhaustion as the only execution bound

Rejected because gas does not specify deterministic ordering, durable progress or
starvation among multiple eligible records.

## Open questions and technical debt

- Many raw contract methods/facades are `pub` across crates. A complete reachability
  inventory must determine which mutation bypasses can skip ABI authority or
  orchestration; this ADR does not assume crate boundaries are closed seams.
- `StorageHandle::checkpoint`, `checkpoint_commit` and `checkpoint_revert` are
  public low-level operations. Prefer RAII capabilities and audit callers for
  mismatched/nested checkpoint ownership.
- `StorageKey::mapping_slot` subtracts `key.len()` from 32 without a local checked
  bound. Prove all implementations are <=32 bytes or make invalid keys unconstructible.
- `set_block_timestamp` is available on the general handle and ignored by production
  providers. Restrict it to test adapters so tests cannot accidentally exercise a
  non-production time mutation interface.
- `call` and `staticcall` default to `u64::MAX` gas. Each module ADR must prove a
  deterministic enclosing bound or use explicit caps.
- Borrow-based reentrancy rejection returns a runtime error but has no global
  lock-order/recursive-call model across different providers/frames.
- `PrecompileError` is non-exhaustive, but mappings to receipt, soft failure,
  abstention and fatal block error are distributed across adapters. Create one
  checked classification table and exhaustiveness tests.
- Macro generation proves ABI shape at compile time but not selector collisions
  across registered precompile modules or parity with checked-in Solidity
  interfaces. ADR-B-CRY-001 must own an ABI manifest/golden check.
- Direct `contract_at` can instantiate a facade at arbitrary storage addresses.
  Audit whether any production caller can create aliasing/cross-module state.
- Diagnostic journals contain wall-clock timestamps; they must never be imported
  into consensus logic or treated as authoritative event ordering.
- Module-level stateful property tests and production fault matrices are uneven;
  every System/Core module-owner ADR must identify its concrete G9 gaps.
- This ADR requires human acceptance before its `Proposed` status changes.
