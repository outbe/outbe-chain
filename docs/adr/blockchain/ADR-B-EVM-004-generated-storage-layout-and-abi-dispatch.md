# ADR-B-EVM-004: Generated storage layouts and ABI dispatch are compile-time protocol contracts

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Stateful-module framework and EVM maintainers
- **Scope:** `crates/blockchain/macros` plus generated dispatch/layout manifests
- **Depends on:** ADR-B-WIR-001, ADR-B-EVM-002
- **Related:** ADR-B-EVM-001 and every stateful module ADR

## Context

Procedural macros generate contract facades, base-slot assignment, mapped record
CRUD and ABI dispatch for most stateful modules. Generated code controls consensus
storage and public call authority while being largely invisible in ordinary source
review. A macro defect is multiplied across all modules.

This ADR owns compile-time generation guarantees. Container mechanics are ADR-B-EVM-002;
module invariants and authorization remain in each owner ADR.

## Decision

### Layout manifest

`#[contract]` accepts only a closed vocabulary of storage field kinds. Expansion
computes and emits a machine-readable layout containing contract/address, field
name/type, explicit order, base slot, reserved span, nested record schema/version
and dynamic-slot derivation. Compilation rejects duplicate order, overlapping or
backward explicit slots, arithmetic overflow, unsupported/ambiguous aliases and any
field whose span is not statically known.

Declaration order is not an implicit upgrade mechanism. Every persisted field has
an explicit stable id/order/slot; removed fields remain reserved as typed deprecated
gaps. CI diffs manifests against the accepted previous layout and requires an
activation/migration ADR for reinterpretation or relocation.

`#[storage_schema]` validates and emits schema version/fingerprint rather than being
a no-op marker. Genesis/migration/runtime code can assert the expected fingerprint.

### Record generation

`#[storage_record]` requires exactly one canonical key and an explicit existence
strategy that cannot collide with a valid zero/default value. It generates bounded,
checkpointed create/update/delete transitions:

- create rejects existing and writes all fields atomically;
- update rejects missing, validates immutable/key fields and atomically replaces the
  complete record;
- delete rejects or idempotently handles absence per declared policy and clears all
  scalar, presence and dynamic tail slots atomically;
- load validates structural closure and canonical narrow/enum/bool encodings.

Optional/dynamic/deprecated fields have explicit slot spans and migration semantics.
Generated accessors do not expose mutation authority to declared read-only views.

### ABI dispatch generation

`#[contract_dispatch]` parses one canonical Solidity signature per method and emits
selector/interface metadata. Compilation rejects duplicate selectors/signatures,
unsupported ABI types, mismatched Rust/ABI types or return shapes, incorrect receiver
and special-argument types, and unbounded dynamic input without a declared cap.

Every arm independently enforces:

- view: read-only storage capability, no value, bounded gas/input;
- mutating: write capability, exact caller, zero value, one journal checkpoint;
- payable: write capability, exact caller/value and declared value policy, one
  journal checkpoint.

The presence of one payable method cannot weaken other arms. ABI decode rejects
trailing/noncanonical data and converts errors to stable revert payloads. Generated
selectors and ABI are compared with checked-in Solidity interfaces/artifacts.

### Evidence and expansion review

Macros emit deterministic formatted expansion snapshots/layout/ABI manifests for
every production contract. Review and architecture tooling inspect those artifacts, not
infer generated code. Trybuild covers every forbidden schema/dispatch shape; runtime
differential tests compare generated dispatch with Solidity ABI and raw-call/static/
value/revert behavior.

## Authoritative interfaces

| Responsibility | Owner/entrypoint |
|---|---|
| Field/span/slot calculation | `#[contract]` layout generator |
| Record CRUD generation | `#[storage_record]` |
| ABI selector/arm generation | `#[contract_dispatch]` |
| Storage effect semantics | ADR-B-EVM-002 capabilities/containers |
| EVM frame/static/value integration | ADR-B-EVM-001 |
| Domain authorization/invariants | module ADR and handwritten method |

## Invariants

- Generated slot ranges within one address never overlap or change silently.
- Layout/ABI/schema fingerprints are deterministic and versioned.
- Record CRUD is structurally closed and atomic on every error point.
- Each ABI selector maps to exactly one method and canonical signature.
- Nonpayable/view calls reject nonzero value regardless of other methods.
- View dispatch cannot acquire or exercise mutation/event/subcall authority.
- Generated and checked-in Solidity interfaces agree byte-for-byte on selectors and
  encoding/returns.
- Unsupported macro input fails compilation rather than falling back to one slot.

## Security and trust assumptions

The compiler/proc-macro and reviewed expansion artifacts are trusted build inputs.
String/last-identifier type matching is insufficient authority validation because
aliases or same-named foreign types can pass. Macro output must use trait bounds and
fully qualified types. Untrusted calldata is bounded before Alloy allocation.

## Compatibility and migration

Macro implementation changes are consensus changes for every expanded contract even
when module source is unchanged. CI regenerates manifests/expansions and proves no
active layout/selector/codec drift. Accepted changes require schema activation and
per-contract migration/reset policy.

## Production-interface verification evidence

Inspected all macro attributes, field classification/span/slot assignment,
constructors, storage-record offset/CRUD/accessor generation, ABI signature parser,
dispatch arms/helpers and current trybuild tests. Current macros reduce boilerplate
but do not yet prove layout uniqueness/atomic CRUD or per-arm value/read-only
authority. Status remains Proposed.

## Consequences

Generated code becomes auditable protocol evidence instead of hidden convenience.
Module ADRs can cite a verified layout/dispatch manifest and focus on domain state
machines without assuming macro correctness.

## Rejected alternatives

- **Review only macro source:** one expansion can still classify a field/type
  differently than expected.
- **Use declaration order as schema:** harmless refactors become storage migrations.
- **Let EVM STATICCALL alone define “view”:** a view invoked by a normal transaction
  could mutate through generated handles.
- **Keep payable rejection at whole-dispatch level:** mixed interfaces require
  method-specific value policy.

## Open questions and technical debt

1. **Critical:** if a contract has any payable method, generated dispatch omits the
   global `reject_value`; nonpayable and view arms do not reject value themselves.
   Thus every method in a mixed interface accepts nonzero `msg.value`.
2. `#[contract_view]` only changes the helper used; generated contract fields still
   expose mutating `Slot/Map/...` methods and dispatch does not supply a read-only
   capability. A view method can mutate when called in a non-static transaction.
3. Receiver mutability is not validated: view, mutating and payable methods merely
   need any receiver. Enforce the intended receiver/capability shape.
4. Type checks for `caller: Address` and `value: U256` compare only the final type
   identifier, so unrelated/aliased same-named types can pass compile-time checks.
5. ABI signature parser accepts the tail largely as an opaque string passed into
   `sol!`; independently validate mutability/returns and ensure it agrees with marker
   kind and Rust return type.
6. Duplicate ABI selectors/signatures within an impl are not explicitly detected by
   the macro with a stable diagnostic. Add collision checks before `sol!` expansion.
7. Generated dispatch does not add an explicit journal checkpoint around a mutating
   method. Outer EVM revert may cover top-level calls, but caught Rust cross-module
   errors can retain partial writes; define/checkpoint the exact boundary.
8. Dynamic ABI arguments are generally decoded before protocol-specific size
   preflight. Integrate declared per-argument/total bounds into generated dispatch.
9. `#[storage_schema]` is currently a no-op and provides no schema version,
   fingerprint, validation or manifest.
10. Contract slot assignment permits explicit slots without overlap/backward/range
    checks; setting a lower explicit slot can collide with earlier fields.
11. Duplicate `#[attribute(order = ...)]` values are sorted without rejection, so
    source order silently breaks the purported stable order.
12. Slot arithmetic is emitted token arithmetic or unchecked proc-macro `u64`
    addition; overflow and excessive ranges need compile-time errors.
13. Unknown contract field types fall back to a one-slot `Slot<T>`, which can hide an
    unsupported multi-slot type or generate code with unintended semantics.
14. Field classification uses last path identifier, so foreign aliases named
    `Map`, `Slot`, `Optional`, etc. can be misclassified.
15. `Deprecated<T>` currently influences slot counting/type unwrapping but does not
    enforce immutability/reservation/migration behavior; `deprecated` attribute data
    is parsed but appears unused.
16. `default` attributes affect `with_key` construction but are not schema metadata
    and may change silently between builds.
17. Record offset calculation allows duplicate order and uses `unwrap()` after an
    earlier fallible call. Remove panic paths from proc-macro expansion.
18. Record existence is inferred from one field being nonzero. Valid records whose
    sentinel can be zero become absent; schema must prove sentinel invariant or use
    a dedicated presence slot.
19. Generated create/update/delete write fields sequentially without their own
    checkpoint. A provider failure midway leaves a partial record unless every caller
    supplies an enclosing checkpoint.
20. Generated delete does not check existence and clears fields sequentially;
    idempotency versus missing-record error is implicit and domain-dependent.
21. Optional fields consume two slots and their generated write/delete inherits the
    partial presence/value issue identified in ADR-B-EVM-002.
22. Dynamic String/Vec writes use mapping-derived `StorageBytes`; layout manifests
    must include head/tail derivation and deletion completeness.
23. No whole-workspace generated layout manifest/collision diff is checked into CI.
24. Trybuild currently covers only three dispatch errors and no storage layout/
    record failures, mixed payable value behavior, view mutation authority, selector
    collision or dynamic bounds.
25. Add expansion snapshot tests for every field/container/record/dispatch shape and
    production raw-call differential tests through the real EVM provider.
