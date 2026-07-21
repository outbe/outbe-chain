# ADR-B-EVM-003: Stateful precompiles mutate EVM state through explicit journaled capabilities

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** EVM integration and stateful-module maintainers
- **Scope:** `outbe-primitives::storage` and production EVM provider seam
- **Depends on:** ADR-B-NOD-001, ADR-B-CNS-003, ADR-B-WIR-001
- **Related:** ADR-B-EVM-002, ADR-B-EVM-001

## Context

Rust stateful precompiles need EVM storage, balances, code, logs, gas, transient
state, block context and child calls while preserving normal CALL/STATICCALL journal
semantics. Test, consensus-read and direct-storage adapters must not accidentally
offer stronger authority or different observable results than production.

This ADR owns the low-level capability and typed slot/container contract. Generated
contract schemas are ADR-B-EVM-003; revm/Reth wiring and subcall execution are ADR-B-EVM-001.

## Decision

### Capability separation

Replace one broad optional provider with explicit capabilities:

- `ExecutionRead`: exact block context, account/storage/transient reads and gas;
- `ExecutionWrite`: journaled storage/balance/code/event changes, available only
  outside static context;
- `ChildCall`: CALL/STATICCALL with explicit gas/value/status semantics;
- `HistoricalRead`: storage and canonical hashes at one exact block, with no fake
  timestamp/beneficiary/account/event/gas behavior;
- test fixtures implementing a declared capability set.

Module constructors require the weakest capability they use. Unsupported operations
are absent from the type/interface, not successful no-ops or generic defaults.

### Storage handle and journal scopes

One non-`Send` `StorageHandle` owns an invariant mutable provider borrow for the
entire execution scope and may be cloned only as aliases to that same journal.
Runtime borrow conflicts return typed fatal/provider-borrowed errors rather than
panic.

Every public multi-write domain transition uses an RAII checkpoint. Success commits
exactly the top checkpoint; error, panic unwind or dropped guard reverts storage,
balances, code, transient state, logs, refunds and all provider-owned side effects to
the captured journal point. Checkpoints are opaque LIFO tokens bound to one provider
generation and cannot be committed/reverted out of order.

### Static and effect enforcement

All write effects—SSTORE, TSTORE, code, balance mint/burn/transfer, event and
value-bearing child call—check static mode in the provider, with handle-level early
checks only as defense in depth. A module cannot emit a log before discovering that
its later write is forbidden and preserve the log. Read-only adapters fail every
effect; they never silently discard events/gas/refunds.

Gas is charged before corresponding computation/read/write and is journal-consistent
where EVM semantics require. Arithmetic is checked. Child calls return typed
`Success(returndata)`, `Revert(returndata)` or `Halt(reason)` plus exact gas data;
callers cannot confuse revert with transport/provider failure.

### Typed storage schema

`Storable` has one canonical 256-bit word encoding per scalar. Dynamic/container
types define Solidity-compatible or explicitly Outbe-versioned slot derivation,
length/capacity invariants, dense/sparse semantics and complete deletion of stale
tails. Mapping keys use canonical typed bytes and frozen hash/packing rules.

`Slot`, `Mapping`, `StorageVec`, set, deque, circular buffer, binary heap, byte/string
and record DSL operations are bounded and structurally validate before/after
mutation. Multi-slot writes run under a checkpoint. Schema base slots/ranges are
generated and collision-checked by ADR-B-EVM-003; upgrades never reinterpret occupied
slots without migration.

### Adapter conformance

Production EVM provider is the normative behavior. HashMap/fault-injecting adapters
run a shared conformance suite covering reads, writes, static rejection, nested
checkpoints, gas/refunds, logs, balances, transient state, subcalls and every
container corruption/boundary. Historical/direct adapters implement only their
declared subset and return explicit unavailable/unsupported for missing context.

## Authoritative interfaces

| Responsibility                         | Owner/entrypoint       |
| -------------------------------------- | ---------------------- |
| Capability vocabulary and typed errors | storage boundary       |
| EVM journal/static/gas implementation  | ADR-B-EVM-001 provider |
| Typed word/container codecs            | storage types/DSL      |
| Contract slot layout generation        | ADR-B-EVM-003          |
| Domain mutation/state invariants       | each module ADR        |

## Invariants

- No code can obtain write/call authority from a read-only capability.
- Static execution cannot change any state, balance, code, log or transient value.
- A failed checkpointed transition leaves all provider effects unchanged.
- Nested checkpoints are provider-bound and strictly LIFO.
- Every storage type has canonical encoding and disjoint generated slots.
- Container indexes/length/membership/data are structurally closed after success.
- Test/read adapters never return a plausible fabricated execution context.
- Production and conformance adapters agree for their declared capability set.

## Atomicity, reentrancy and failure

Child CALL opens a real nested EVM frame/journal. Parent handle borrow is released or
re-entry is mediated by the EVM driver; an arbitrary `RefCell` borrow conflict is not
a protocol reentrancy policy. Module-specific checks-effects-interactions and
reentrancy guards remain in owner ADRs.

Out-of-gas, static violation, child revert/halt, storage corruption and provider
failure propagate without partial effects. Fatal invariant failures reject block
execution; user-invalid domain operations produce typed revert bytes where ABI
requires.

## Compatibility and migration

Slot derivation, scalar/container codecs, gas schedule, checkpoint semantics and
subcall status mapping are consensus-critical. Any change needs activation, storage
layout diff/migration and golden vectors against Solidity/revm behavior. Adapters may
add capabilities but cannot weaken existing failure semantics.

## Production-interface verification evidence

Inspected provider trait, handle/interior borrowing, RAII checkpoints, static/balance/
code/event/gas/subcall APIs, EVM/direct/read-only/HashMap adapters, DSL and storage
containers plus production callers/tests. The abstraction has useful explicit
handles/checkpoints, but its broad interface and several adapter no-ops allow
plausible misuse. Status remains Proposed.

## Consequences

module audits can inspect module logic against a precise effect capability instead
of assuming every `StorageHandle` operation is production-safe. Tests become evidence
only when their adapter declares and proves the required semantics.

## Rejected alternatives

- **Keep one trait with defaults/no-ops:** missing production authority looks like a
  valid zero/success result.
- **Let every module compute raw slots:** collision and migration proofs fragment.
- **Rely only on callers for STATICCALL checks:** one forgotten check mutates state.
- **Treat `RefCell` borrow failure as reentrancy protection:** it is an implementation
  accident, not a domain policy.

## Open questions and technical debt

1. `PrecompileStorageProvider` is one broad trait; read-only/direct/test providers
   must implement effects/context they cannot honestly supply. Split capability
   traits and make module requirements explicit.
2. `ReadOnlyStorageProvider` returns chain id, timestamp, block number, beneficiary,
   account info, transient reads and gas counters as zero/default. Context-dependent
   reads can therefore produce plausible wrong answers instead of unavailable.
3. The same read-only provider returns `Ok(())` for `emit_event` and `deduct_gas` and
   ignores refunds. Unsupported effects must fail or be absent from its capability.
4. Checkpoint methods expose raw revm `JournalCheckpoint` and public commit/revert
   without provider/generation/LIFO typing. Misordered or cross-scope use is not made
   unrepresentable.
5. `checkpoint`, commit, revert, refund and some handle methods use `borrow_mut`
   rather than `try_borrow_mut`, so nested misuse can panic instead of returning a
   structured fatal error.
6. Prove `CheckpointGuard` reverts on every unwind path and that provider checkpoint
   covers SSTORE, logs, balances, code, transient storage, refunds and every custom
   side channel.
7. Handle-level static gates exist for some effects, but the trait contract does not
   state/enforce them uniformly for SSTORE/TSTORE/event/balance/subcall. Audit every
   provider and add conformance tests.
8. `set_balance` reads then separately increases/decreases through multiple mutable
   borrows. Keep it checkpointed/atomic and define behavior under reentrant/custom
   providers.
9. Default `sub_call` returns `NotAvailable`, while stale handle comments still say
   the stub returns `Ok(empty)`. Remove contradictory documentation and audit callers
   for legacy assumptions.
10. `call`/`staticcall` default gas cap is `u64::MAX`; require an explicit bounded gas
    policy or correctly derive remaining EVM gas/EIP-150 semantics.
11. Production subcall borrow/re-entry behavior needs a formal call-frame proof. A
    hostile child callback currently may surface `ProviderBorrowed`; show whether
    that is deterministic and equivalent on all nodes.
12. `StorageHandle` is cloneable via `Rc<RefCell<&mut dyn ...>>`; runtime borrow checks
    replace compile-time exclusivity. Explore scoped capability borrowing/deep module
    APIs that eliminate aliasing failures.
13. `StorageRecord::{create,update,delete}` relies on each generated/manual record to
    enforce existence and complete clearing. Add generic structural contracts and
    generated state-machine tests.
14. `OptionalField::write(Some)` writes presence before value without an internal
    checkpoint; a second-write failure can leave a present default/stale value.
15. Audit every multi-slot container operation for the same partial-write risk.
    Require internal checkpoints or caller-proven enclosing atomic scope.
16. Dynamic bytes/vector shrink/delete must prove all stale tail slots are cleared
    and gas/refund behavior matches the active EVM schedule at maximum lengths.
17. Container lengths/capacities and loops need explicit protocol bounds; corrupted
    length words must fail before unbounded allocation/iteration.
18. Mapping/array/record slot derivation needs independent Solidity compatibility
    vectors and a whole-project collision/layout manifest.
19. `Storable::from_word` is generally infallible; invalid enum/bool/narrow integer
    encodings can be silently truncated/accepted unless each type is strict. Split
    canonical fallible decoding from total raw-word wrappers.
20. Test HashMap/direct adapters may not model cold/warm access, gas, refunds,
    transient storage, logs, static mode, nested frames or account lifecycle. Publish
    capability matrices and prevent weak fixtures from claiming production evidence.
21. Historical reads need an exact block identity capability rather than a storage
    reader plus zero context; connect this with ADR-B-TXP-001 consistency classes.
22. Add fault injection after every primitive/container write, nested checkpoint/
    child-call/reentrancy tests and differential vectors against production revm and
    equivalent Solidity contracts.
