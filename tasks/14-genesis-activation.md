# T14 — Genesis alloc, R_sealed(0) derivation, height-0 CE marker

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §17 (Q12, Q19)
Depends on: T04, T13, T15, T16 (Part A; unconditional); T24 (Part B re-baseline only)
Blocks: — (release gate T25 only)

## Structure (audit P1-0b): two parts

- **Part A — genesis plumbing** (implementable now): alloc seeding, derivation + fail-closed checks,
  CE-MDBX height-0 marker, block-1 boundary wiring — on provisional `K_domain`.
- **Part B — Stage 1 testnet CES genesis re-baseline/rehearsal** (Depends additionally: T24 Part B1
  constants publication — this rehearsal is a B2 consumer re-baseline, postfix PF-B03;
  audit-final L-03 — production/mainnet genesis activation remains a FUTURE gate outside this plan): the
  Stage 1 testnet `genesis.json` is frozen only after final `K_domain`/limits land, because final K
  changes the genesis root; re-run of the §19.15 rehearsal at final values is the freeze gate.

## Summary

Wire greenfield genesis activation: seed `0xEE0B` in the genesis alloc, derive and verify `R_sealed(0)`
fail-closed on every node, and initialize the CE-owned MDBX with the height-0 marker.

## Context

Genesis commits `R_sealed(0)` through EVM state only — no tag-0x08 artifact, `extra_data` stays empty
(existing Outbe genesis convention). Alloc: `0xEE0B` code `0xef`, slot 0 = 1, slot 1 = derived `R_sealed(0)`
(never operator-configured); slots 2–5 structurally empty; `0xEE0B` joins the runtime EIP-161 marker set
(CHAIN-276 pattern, precedent: `0xEE04` seeding in `scripts/seed_genesis.py`). Initialization: canonicalize
genesis entities (none for v1 → deterministic empty root), build genesis collections/shards/Root Catalog via
the Q18/Q23 registry, require exact equality derived ≡ slot 1 ≡ chainspec, and write the height-0
`last_applied` marker (`ZERO` parents valid only at height 0). Block 1 then requires genesis parent
hash/root and emits the first 0x08 artifact.

## Scope

- `scripts/seed_genesis.py`: seed `0xEE0B` (code `0xef`, slots 0–1) with the derived empty `R_sealed(0)`
  (value produced by a Rust vector tool, not recomputed in Python — Genesis V2 rules: public state only).
- Runtime EIP-161 marker-set addition for `0xEE0B`.
- Node initialization step: derive `R_sealed(0)` (T04 fixture path), compare to seeded slot and chainspec,
  fail-closed on mismatch; initialize/rebuild CE-MDBX height-0 marker (T15 API), idempotent on restart.
- Single activation predicate (postfix PF-H07): ONE fail-closed chain-spec-level
  `ces_active(chainspec, height)` predicate (true from genesis for v1) is THE source for every CES gate —
  T13 artifact expectations, T16 startup validation, T23 registration wiring, and T29 profile gating all
  consume it; ad hoc per-module "CE active" checks are forbidden.
- Block-1 boundary checks wired with T13/T16: parent root = marker root = slot value.

## Out of scope

- Preloaded genesis entities (future genesis spec uses the same path); testnet wipe operations.

## Acceptance criteria

1. Genesis rehearsal test (§19.15): deterministic empty-root verification, slot/layout/marker checks,
   height-0 CE marker rebuild, absence of 0x08 in block 0, block-1 parent/root-carrier checks.
2. Tampered slot 1 or chainspec mismatch rejects startup fail-closed.
3. Marker rebuild after deleting the CE-MDBX directory reproduces the identical height-0 marker.
4. Localnet boots from the new genesis and seals block 1 with matching slot/artifact/root.
5. Activation predicate (postfix PF-H07): `ces_active` exists at chain-spec level; T13/T16/T23/T29
   consumers route through it — grep-verified that no ad hoc CE-active check remains.

## Invariants

- `R_sealed(0)` is derived, never configured; genesis hash immutability preserved (ChainSpec rule).
- `ZERO` parent marker fields are valid only at height 0.

## Tests

- Genesis unit tests (`bin/outbe-chain/tests/genesis.rs` pattern), localnet smoke (`mise run localnet-smoke`).

## Files

- `scripts/seed_genesis.py`, `scripts/bootstrap-testnet.sh` (localnet genesis heredoc must seed 0xEE0B too),
  `bin/outbe-chain/src/main.rs` (init step), `crates/blockchain/evm` (marker set),
  `crates/core/compressed_entities/src/genesis.rs`
