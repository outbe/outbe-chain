# T04 — Independent reference model: collection tops, Root Catalog, R_sealed

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §9.2, §19.1 (Q23)
Depends on: T01, T02 (T03 only for the differential harness gate — model itself builds in parallel with T03)
Blocks: T12, T14, T18

## Summary

Build the independent reference model required by §19.1 — ordered valid mutations → final map → commitment —
including the collection-top tree, `R_collection`, the Root Catalog SMT, and `R_sealed`, used as the
differential oracle for the production engine.

## Context

§19.1 mandates an implementation that derives the commitment from first principles (simple maps, no CKB
engine, no shared production code beyond T01 primitives) so production and reference can disagree only when
one of them is wrong. §9.2 defines: per-collection fixed-depth binary top over `K_domain` shard roots
(inclusive levels `0 <= level < log2(K_domain)`), `R_collection = P(TAG_COLLECTION_ROOT; v, collection_key_f,
K_domain, top_shard_root)`, Root Catalog SMT over `collection_key → R_collection` (retired → ZERO / leaf
deleted), `R_sealed = P(TAG_SEALED_ROOT; v, catalog_root)`.

## Scope

- Pure-Rust reference: `BTreeMap`-based shard maps, naive top-tree fold, naive Root Catalog (may reuse the
  same merge math via an independent simple SMT walk), `R_sealed` derivation.
- Ordered mutation application: mint/update/delete/retire_partition with §6.1 preconditions.
- Catalog-leaf rule (pinned; **spec amendment #3 — APPLIED to concept §9.2**): only
  `retire_partition` deletes a Root Catalog leaf. A collection emptied by ordinary deletes is a *changed*
  collection per §8.2 step 4 — it KEEPS its catalog leaf with `R_collection` computed over an all-ZERO top
  (a valid non-ZERO hash). Per amended §9.2, "a never-populated collection has no Root Catalog leaf".
  Consequence for §10.3: non-membership in an emptied-by-delete collection
  proves shard absence + both upper paths; catalog-absence proves never-populated-or-retired.
- Empty shard root = ZERO as legal top input.
- Genesis empty root: `R_sealed(0)` for zero collections — exported as a named constant fixture for T14.
- Differential harness API consumed by T03/T12 tests (apply same op sequence to both, compare all roots).

## Out of scope

- Performance (reference may be O(N log N) naive); persistence; proofs beyond root recomputation.

## Acceptance criteria

1. Reference reproduces every T01–T03 golden vector (tags, keys, merges, tops, catalog, sealed root).
2. Golden vectors: empty collection/catalog, single-collection, multi-collection, retirement mid-sequence,
   `R_sealed(0)` (§19.2 items: empty collection/catalog, the emptied-by-delete ZERO-top `R_collection`
   leaf, collection/root-catalog proofs, partition retirement, sealed root).
3. Differential suite T03-engine vs reference green over randomized sequences including retirement.
4. `R_sealed(0)` fixture documented and consumed by T14 genesis test.

## Invariants

- Reference shares only T01 hash primitives with production; no CKB code, no store code.

## Tests

- Proptest: random op sequences (mint/update/delete/retire, multiple domains, singleton + partitioned) —
  identical `R_sealed` between reference and production for every prefix.

## Files

- `crates/core/compressed_entities/tests/reference_model/` (test-only; or a `ces-reference` dev-dependency crate)
