# T18 — Point-proof package assembly and verification

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §10 (Q7, Q19, Q20, Q23)
Depends on: T04, T06, T13, T15, T30 (proof encoding v1)
Blocks: T19, T21, T24, T26, T33

## Summary

Implement proof generation (node side) and the verification procedure (client/library side) for the
point-proof package: shard SMT proof + `log2(K_domain)` collection-top siblings + Root Catalog proof,
bound to a finalized header.

## Context

Package fields per §10.3: chain/height/hash, scheme version, `R_sealed`, `{domain_id, partition_key_or_none,
raw_id}`, schema/hash versions, `body_bytes`, proof encoding version, `smt_proof`, `collection_shard_proof`,
`root_catalog_proof`. Verification: (1) bind package identity to the verifier's expected identity — RPC
responses must equal the request; (2) resolve fork-active ID/partition policy and `K_domain` from the
registry, derive/validate all locators — the package cannot select any derived locator, redundant transported
values must match exactly; (3) recompute leaf from exact bytes; (4–6) shard root → collection top →
`R_collection` → catalog proof → `R_sealed` → compare with the selected finalized header. Non-membership:
either the collection is absent from the catalog, or absence in the derived shard plus both upper paths.
Catalog-leaf semantics (pinned rule, T04 / spec amendment #3): a collection emptied by ordinary deletes
KEEPS a present ZERO-top `R_collection` catalog leaf — non-membership there takes the shard-absence branch;
catalog-absence proves never-populated-or-retired only.
Freshness (§10.4): latest persisted finalized state only; one MDBX read snapshot per package; older-root
proofs remain valid for that root but clients requesting current state reject them.

## Scope

- Node-side assembler over T15 read snapshots at `proof_ready_height`; `proof_encoding_version = 1` wire codec.
- Verifier library (usable off-node, no MDBX dependency): full §10.3 procedure incl. both non-membership
  branches and genesis H=0 root sourcing (chainspec derivation, no artifact).
- Height-0 and height≥1 root extraction (T13 artifact / trusted genesis derivation).
- Stale/replay defenses: package binds chain_id, height, block_hash, root, versions; verifier rejects
  mismatched checkpoint (§2.3 stale-proof row).

## Out of scope

- RPC transport and unavailable/unsupported semantics (T19); historical proofs (non-goal).

## Acceptance criteria

1. Round-trip: generated packages verify for present, absent-in-shard, and collection-absent cases across
   singleton and partitioned domains (§19.2 collection/root-catalog proof vectors), incl. the
   emptied-by-delete case: absent key in an emptied collection yields a shard-absence proof with a PRESENT
   ZERO-top catalog leaf, never a catalog-absence proof.
2. Adversarial: wrong-ID, RPC-selected encoding kind, mismatched redundant identity, wrong-height root,
   tampered body/leaf/proof bytes, and schema downgrade — a package carrying a `schema_version`/
   `hash_version` not active for the domain at the proof height is rejected before leaf recomputation —
   all rejected (§19.7, incl. its named "schema downgrade" gate).
3. Torn-read impossibility: package assembled during concurrent commit still internally consistent
   (T15 snapshot test extended end-to-end).
4. Proof for an older root verifies for that root but is rejected by a current-state client policy.

## Invariants

- The verifier trusts only the finalized header and the fork-active registry; every locator is derived.

## Tests

- Vector-driven verifier tests (shared fixtures with T04), fuzz package decoding (§19.17).

## Files

- `crates/core/compressed_entities/src/proof/{assemble.rs,verify.rs,wire.rs}`
