# ADR-B-EVM-002: Outbe EVM extensions use one versioned registry and preserve call-frame semantics

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** EVM integration maintainers
- **Scope:** `crates/blockchain/evm` factory, precompile registry, dispatch and child-call driver
- **Depends on:** ADR-B-CNS-003, ADR-S-FEE-001, ADR-B-WIR-001, ADR-B-OCD-007, ADR-B-EVM-001
- **Related:** ADR-B-CNS-002, ADR-B-TXP-001, ADR-B-EVM-003, ADR-S-ZKP-001

## Context

Outbe exposes Rust modules as stateful EVM precompiles and lets those modules call
ordinary bytecode and other Outbe precompiles. This integration is consensus
critical: every validator must construct the same EVM, recognize the same addresses,
inject the same capabilities, charge the same gas and map every terminal result to
the same receipt and journal outcome.

This ADR owns only the revm/Reth integration and call-frame contract. System
transaction ordering is ADR-B-EVM-001, zero-fee admission is ADR-S-FEE-001, compressed-entity
lifecycle is ADR-B-OCD-007, provider/storage semantics are ADR-B-EVM-003 and individual module
state machines belong to their module ADRs.

## Decision

### One protocol-versioned extension manifest

Maintain one compile-time manifest whose entries contain:

- canonical address and stable symbolic name;
- activation/deactivation protocol version;
- dispatch function and ABI/schema identity;
- base/dynamic gas policy;
- required capabilities, including body reads and compressed-entity execution;
- stateful/stateless and reentrancy policy; and
- implementation crate/owner ADR.

The manifest generates address lookup, enumeration, warm-address handling,
diagnostics and conformance tests. Duplicate addresses or names, overlap with
Ethereum precompiles/reserved system addresses, missing capability declarations and
unmapped entries are compile-time failures. An address cannot silently change
meaning; registry changes require protocol activation under ADR-S-GOV-003 and ADR-B-WIR-001.

### EVM construction is fail-closed

`OutbeEvmConfig`/`OutbeEvmFactory` construct an EVM for an explicit chain spec,
hardfork/spec id, block environment and execution mode. Production modes are typed:
canonical block execution, payload construction, pending simulation and exact-block
read-only RPC. Each mode declares mandatory bridge, signer, body-reader, authenticated
tree and historical-state capabilities.

Missing or inconsistent mandatory capabilities fail construction before any
transaction executes. Test/offline constructors are visibly non-production and
cannot be wired into a live node accidentally. An EVM instance receives an immutable
execution context; nested frames inherit the same chain/spec/block identity and
capability generation.

### Context dispatch and effect ownership

The registry dispatch adapter obtains one exclusive, lifetime-bound EVM context and
constructs the least-authority provider described by ADR-B-EVM-002. The adapter must not
fabricate context, bypass the revm journal or retain a context pointer beyond the
synchronous call.

Eliminate the untyped `*mut c_void` seam where upstream interfaces permit. Until
then, isolate it in one audited module with a documented provenance/lifetime proof,
compile-time concrete-context assertion, Miri/sanitizer coverage and no safe API that
can supply an arbitrary pointer.

Every top-level and nested dispatch uses the same registry, capability bundle,
calldata materialization and result mapping. Special body-aware dispatch is selected
by manifest capability, not a second address match.

### Gas and terminal result contract

Gas is derived from the active protocol schedule and charged once. The registry
charges declared input/computation base cost; provider operations and child frames
charge their actual EVM costs without double counting. Cold/warm access, memory,
refund caps, EIP-150 forwarding and exceptional halts follow the active revm spec.
All arithmetic is checked and bounded.

Module results map exhaustively to EVM outcomes:

- domain rejection becomes ABI-defined `REVERT` with charged gas and journal rollback;
- write protection and child-frame exceptional conditions retain their canonical
  halt/revert class;
- out-of-gas consumes gas according to revm semantics;
- provider corruption, missing consensus authority or impossible internal state is
  a typed fatal block-execution error, never a user-controlled revert; and
- unknown future error variants cannot be flattened by a wildcard mapping without a
  deliberate protocol decision.

Receipt status, returndata, logs, gas used/refunded and committed state must describe
the same terminal outcome.

### Child CALL and STATICCALL

A child call is a genuine revm frame over the existing journal, not an independent
mini-executor. It preserves caller as the invoking precompile address, target/value,
calldata, remaining gas, active spec, depth, warm state, transient state and static
flag. Outer static mode is monotonic: no descendant may regain write/value authority.

Frame creation and return use upstream handler semantics. Success commits the frame;
revert and halt roll it back exactly once and return typed status/returndata/gas to
the caller. Ordinary bytecode, Ethereum precompiles and Outbe precompiles remain
reachable through the same resolution order at every depth.

Reentrancy is declared per module/entrypoint. The EVM frame stack and module state
guards enforce it; a thread-local same-address ban is not the protocol definition.
Cross-address cycles, callbacks and read-only re-entry must have explicit behavior.

### Conformance and production evidence

A registry-generated suite runs every active entry through the production factory
and context adapter. It proves address/name/ABI/gas/capability agreement, top-level
versus nested equivalence, CALL/STATICCALL rollback, value transfer, reentrancy,
depth, OOG, Ethereum fallback and exact receipt/log/state effects. Differential tests
compare the custom frame driver and gas accounting with the pinned upstream revm
handler for equivalent bytecode calls.

## Authoritative interfaces

| Responsibility | Authority |
|---|---|
| Active Outbe address/dispatch/capability table | versioned EVM extension manifest |
| Chain/spec/mode and capability assembly | `OutbeEvmConfig` / `OutbeEvmFactory` |
| Context-to-provider adapter | EVM dispatch seam governed by ADR-B-EVM-002 |
| Top-level and nested precompile resolution | one generated registry dispatcher |
| Child frame creation/return | revm-compatible child-call driver |
| Module ABI and state transition | owning module ADR / generated ABI contract |

## Invariants

- Every active Outbe address resolves to exactly one manifest entry at a protocol
  version, and every manifest entry is registered and enumerable.
- All validators derive identical factory configuration from canonical inputs.
- A production EVM cannot execute with a missing mandatory capability.
- Top-level and nested calls observe the same registry, block identity and protocol
  schedule.
- Static authority can only stay static or become more restrictive down the stack.
- A child revert/halt leaves no child state, balance, transient value or log effect.
- Gas and refunds are charged once and match the receipt and committed execution.
- Unsafe context access cannot escape its synchronous typed factory invocation.
- Reentrancy policy is deterministic and module-declared, not thread scheduling.

## Atomicity, concurrency and replay

revm's transaction and frame journals are the sole atomicity authority for EVM
effects. Off-chain or compressed-entity effects participate through ADR-B-OCD-001 and ADR-B-EVM-002 and
must seal or roll back with the same execution outcome. Dispatch must not publish
side effects before the enclosing transaction commits.

EVM instances do not share mutable execution scope across blocks. Factory-level
service handles may be shared, but each invocation snapshots an exact immutable
generation. Concurrent RPC simulations cannot activate, overwrite or deactivate a
canonical block executor's scope. Re-executing the same block against the same parent
and external authenticated inputs produces byte-identical results.

## Compatibility and migration

Address assignment, activation version, dispatch ABI, result classification, gas
schedule, capability requirements and call-frame semantics are consensus protocol.
Changes require an activation plan, golden vectors before/after the boundary and a
state migration when address meaning or layout changes. The pinned alloy/revm fork
and its context-hook ABI are part of the compatibility surface until the unsafe seam
is removed.

## Production-interface verification evidence

Inspected `OutbeEvmConfig`, `OutbeEvmFactory`, `OutbeEvm`, precompile lookup and
enumeration, raw context hook, `CtxStorageProvider`, error/gas translation,
`OutbeSubCallPrecompiles`, manual frame loop and production builder wiring. Existing
tests cover selected registration, reader propagation, system-call gas and CE-scope
wiring, but do not yet prove the whole manifest/call-frame contract. Status remains
Proposed.

## Consequences

The EVM crate becomes a narrow integration module: module implementations cannot
alter global registration or frame semantics, and audits can distinguish a module
bug from an adapter/gas/journal bug. Adding a precompile requires one manifest entry
and generated evidence rather than several synchronized match arms and lists.

## Rejected alternatives

- **One ADR for the entire block executor:** mixes system lifecycle, fee policy,
  module state and revm integration under no single authority.
- **Hand-maintained lookup plus address list:** permits silent omissions and drift.
- **Treat missing services as empty scopes/readers:** creates mode-dependent plausible
  results rather than failing construction.
- **Thread-local same-address guard as reentrancy semantics:** does not describe
  cross-address cycles, callbacks, async/thread migration or module-specific policy.
- **Custom child execution without differential proof:** can drift from revm journal,
  gas, depth and hardfork behavior.

## Open questions and technical debt

1. `outbe_dispatch_fn` and `outbe_precompile_addresses` duplicate the registry. The
   latter already omits at least `VAULT_PROVIDER_ADDRESS`, `GOVERNANCE_ADDRESS` and
   `DEBUG_SUBCALL_PRECOMPILE_ADDRESS` present in lookup; replace both with one
   generated manifest and make current registration tests exhaustive.
2. `DEBUG_SUBCALL_PRECOMPILE_ADDRESS` is registered in the normal table. Prove it is
   impossible on production networks or gate it by a non-consensus test build; a
   debug capability must not silently become protocol ABI.
3. Most entries use one flat `PRECOMPILE_BASE_GAS`; only slash indicator and ZK paths
   declare input-aware costs. Audit each module for CPU, allocation, decoding and
   iteration bounds so cheap gas cannot buy unbounded native work.
4. `extend_outbe_precompiles` relies on an Outbe alloy-evm fork and casts a raw
   context pointer with `unsafe`. Upstream a typed hook, pin/verify the fork commit,
   document its ABI and add Miri/sanitizer regression coverage meanwhile.
5. The hook's safety proof assumes its generic `DB` exactly matches the erased
   pointer. There is no runtime tag or compiler-visible lifetime/type witness at the
   cast. Make this relationship structurally enforced.
6. `OutbeEvmFactory::new()` creates a scope without a tree service and no body
   readers; comments call it transitional, but the type is indistinguishable from a
   production factory. Introduce typed construction modes and live-node validation.
7. RPC scope creation reads a finalized marker under a shared service and falls back
   to `ExecutionScope::new()` on missing service/marker error. ADR-B-OCD-001 requires
   fail-closed exact-block scopes; distinguish unavailable/corrupt from legitimate
   non-CE mode.
8. `compressed_tree_service` is an `RwLock<Option<_>>` mutable after factory cloning.
   Prove no EVM construction can race installation/replacement and observe a
   different authority generation; prefer immutable assembly.
9. Runtime body readers are optional, then selected through a second hard-coded
   address match. Move capability requirements into the manifest and reject missing
   mandatory readers at EVM construction, not midway through dispatch.
10. `OUTBE_SYSTEM_TX_ADDRESS` uses body-aware dispatch only when readers exist but
    otherwise falls through to the generic function, unlike Tribute/Nod addresses.
    Prove this asymmetry is intentional and cannot cause validator-mode divergence.
11. Pre-decoding first maps `SharedBuffer` to empty bytes for base-gas calculation,
    then materializes real bytes for dispatch. Input-dependent gas can therefore be
    undercharged for contract-originated calls. Materialize once before pricing.
12. `actual_gas = base_gas + storage_gas` and several meters use saturating
    arithmetic. Replace masking with checked invariants and differential tests for
    OOG/refunds/double charging.
13. `CtxStorageProvider::sload` always charges warm-read cost and `sstore` always
    charges `SSTORE_RESET`; it does not visibly price cold access, original/current
    value, refunds or active-spec transitions. Reconcile provider charges with revm
    to prevent material gas under/overcharging.
14. `map_outbe_precompile_result` turns `SubCall` and `Unsupported` into fatal errors
    and has a wildcard fatal mapping for a non-exhaustive enum. Specify every status
    at the ABI/block-validity boundary and version new variants deliberately.
15. Write-protection is mapped to a generic custom precompile halt string. Prove its
    receipt/gas behavior equals canonical STATICCALL semantics at all depths.
16. Reentrancy uses a process thread-local vector and denies only re-entry to the
    same address. It is not carried in the EVM context, does not express per-method
    policy and permits cross-address cycles. Replace it with execution-scoped,
    module-declared policy and tests.
17. `ReentrancyStack` uses `RefCell::borrow_mut`; corruption/nested guard misuse can
    panic. The guard removes the last matching entry rather than asserting strict
    LIFO, hiding stack-order bugs.
18. The child driver manually mirrors a pinned revm `run_exec_loop`. Upstream changes
    to frame initialization/return, memory, EOF, authorization or hardfork behavior
    can silently diverge. Prefer an upstream API or maintain differential/version
    gates for every dependency upgrade.
19. Child frame input uses `depth: 0` while separately checking the journal depth.
    Prove nested interpreter depth and the 1024 limit are preserved; add recursive
    ordinary/Outbe mixed-call tests.
20. Child gas forwarding accepts the supplied `input.gas_limit` directly. Prove the
    caller wrapper applies remaining-gas and EIP-150 caps exactly and cannot mint gas
    with a default `u64::MAX` request.
21. Child CALL outcome classification covers selected `InstructionResult` variants
    and flattens all others to fatal text. Build an exhaustive, version-pinned map
    including success-return variants, invalid opcodes and future EOF outcomes.
22. The child uses `self_address` as caller. Verify CALL/STATICCALL/DELEGATECALL are
    the only intended schemes and forbid unsupported identity/value semantics at the
    type boundary.
23. Add rollback tests asserting storage, balance, code, transient state, logs,
    refunds, CE overlay and body-reader failure observations after nested revert,
    halt, OOG and panic/fatal paths.
24. Add production-factory tests for every construction mode and every manifest
    entry, including top-level/nested byte equality, Ethereum fallback, hardfork
    transitions and absent capabilities.
25. `set_spec` updates the nested Ethereum provider and local `spec`; prove already
    captured manifest gas/capability behavior cannot retain a stale spec.
26. Handler registries for Vote and Update are additional compile-time tables wired
    from this crate. Their ownership remains ADR-S-GOV-002 and ADR-S-GOV-003, but EVM conformance must
    prove their exact active version is bound to the same protocol schedule.
