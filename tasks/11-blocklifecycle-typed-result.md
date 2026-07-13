# T11 — BlockLifecycle associated EndBlockResult refactor

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §8.2 (Q22)
Depends on: —
Blocks: T12

## Summary

Extend the shared lifecycle contract with a typed end-block result:
`trait BlockLifecycle { type EndBlockResult; … end_block(ctx) -> Result<Self::EndBlockResult> }`, migrating
every existing lifecycle module to `EndBlockResult = ()`.

## Context

`outbe_primitives::block::BlockLifecycle` today has unit-Result `begin_block/end_block` on zero-sized marker
types. The seal must return `SealOutput { R_sealed, staged_tree_batch }` directly to the executor — the spec
forbids a global output registry, type erasure, or a mutable result slot in `BlockRuntimeContext`. This is a
cross-cutting refactor over all existing lifecycle modules and the executor's explicit, hard-fork-governed
call sequence.

## Scope

- `type EndBlockResult` (default-free; each impl declares it) on `BlockLifecycle`; ordinary modules declare `()`.
- Executor call sites updated; lifecycle ordering stays explicit in the executor (no plugin registration).
- No behavior change for existing modules — pure signature migration.

## Out of scope

- The CE lifecycle module itself (T12); any reordering of existing lifecycle steps.

## Acceptance criteria

1. Workspace compiles; all existing lifecycle modules migrated with `EndBlockResult = ()`.
2. No `Box<dyn Any>`, no result slot in `BlockRuntimeContext`, no registry — typed value returned directly.
3. Existing lifecycle tests green unchanged (behavioral no-op).

## Invariants

- Lifecycle ordering remains explicit and hard-fork governed in the executor (repo rule §8.6/CLAUDE.md).

## Tests

- Existing suites; one new compile-time usage test demonstrating a non-unit `EndBlockResult` consumer.

## Files

- `crates/blockchain/primitives/src/block.rs`
- all `BlockLifecycle` impls + `crates/blockchain/evm/src/executor.rs` call sites
