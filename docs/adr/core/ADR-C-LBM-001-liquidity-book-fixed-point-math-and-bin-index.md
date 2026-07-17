# ADR-C-LBM-001: Liquidity Book math and bin indexes are a versioned deterministic library

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** protocol-math and bin-ledger maintainers
- **Scope:** `outbe-primitives::math`
- **Depends on:** ADR-B-WIR-001, ADR-B-EVM-003, ADR-B-EVM-002
- **Related:** ADR-C-LYS-001, ADR-C-INX-001, ADR-C-GEM-001, ADR-C-POW-001

## Context

Gem, Nod and Intex use a Rust port of PancakeSwap Liquidity Book: 128.128
fixed-point prices, 512-bit multiplication/division, logarithm/power, 24-bit bin ids
and a three-level radix-256 bitmap. Its rounding and traversal determine consensus
state. This ADR owns those shared primitives, not domain conservation or PoW.

## Decision

### Frozen math profile

Define `OutbeLbMathV1` with an immutable upstream revision, constants, numeric
formats, rounding and every intentional deviation. It fixes 256-bit operands,
512-bit intermediates, 128 fractional bits, `1e18` decimal precision, basis-point
denominator, real-id shift, the uint24 range and exact `log2`/`pow` bounds.

No floats or platform-dependent behavior participate. Every operation is total on a
typed input domain or returns an exhaustive math error. Wrapping arithmetic is used
only where the modular proof requires it. Function types/names expose floor/ceil;
consensus vectors compare exact words, never approximate tolerances.

The library does not construct ABI reverts or choose domain saturation. Owning
modules map math errors exhaustively and explicitly decide whether an out-of-range
price reverts or saturates.

### Canonical bin index

`BinId` validates `0..=2^24-1`. A bitmap is a derived index of canonical non-empty
bin records: level 0 tracks top bytes, level 1 middle bytes and level 2 low bytes.
Add/remove updates every affected level under one journal checkpoint. Traversal is
explicitly inclusive/exclusive, returns numeric order and takes a bounded number of
reads per step. Parent/child inconsistency is corruption, not absence.

The index implementation owns structural closure; domain modules validate a returned
bin against their canonical record before mutation. It offers verify/rebuild tooling
whose input authority is the canonical record set, never bitmap parents.

### Reference evidence

Pin and vendor the exact Solidity reference/vector set. Differential tests cover all
public operations, boundaries and randomized inputs against Solidity and an
independent big-integer model. Model/property tests exhaust the bitmap domain and
fault injection fails after every tree-level write.

## Authoritative interfaces

| Responsibility | Authority |
|---|---|
| Math version/constants/rounding | `OutbeLbMathV1` |
| Valid price-bin identity | `BinId` |
| Derived occupied-bin index | checkpointed bin-index API |
| Meaning and lifecycle of a bin | Gem/Nod/Intex owner ADR |
| Slots and journaling | ADR-B-EVM-003 and ADR-B-EVM-002 |

## Invariants

- Equal profile and inputs produce exact equal words on every validator.
- No invalid high bits can alias a valid bin.
- Overflow, underflow, rounding and saturation never change silently.
- A parent tree bit exists exactly when its descendant subtree is non-empty.
- Failed tree mutation leaves all three levels unchanged.
- Traversal is bounded, ordered and cannot skip or duplicate an eligible bit.
- Canonical module records remain authority over the derived tree.

## Atomicity, replay and failure

Tree mutations use ADR-B-EVM-002 checkpoints. Corruption fails before domain mutation.
Adding an existing or removing an absent bin has a specified idempotent result while
still validating closure. The library performs one bounded traversal step; owning
hooks own processing budgets and persistent cursors.

## Compatibility and migration

Profile/constants, word encoding, rounding, saturation, errors and tree key/slot
derivation are protocol compatibility. Changes require a new activated version,
before/after vectors and index rebuild/migration where layout changes.

## Production-interface verification evidence

Inspected bit helpers, constants, 128.128 log/pow, 512-bit arithmetic, price/id
conversion, `BinTreeStorage`, add/remove/traversal and callers in Gem, Nod and
IntexFactory. Selected unit tests exist; immutable upstream provenance, exhaustive
differential evidence and atomic fault injection do not. Status remains Proposed.

## Consequences

Bin-module audits can rely on one explicit numeric/index contract. Dependency or
helper refactors cannot silently change price or traversal consensus.

## Rejected alternatives

- **Combine PoW and price math as utilities:** different authorities and threats.
- **Use upstream prose as the specification:** the port deliberately differs.
- **Copy helpers per module:** rounding and repair drift.
- **Make bitmap authoritative:** corruption would redefine domain records.

## Open questions and technical debt

1. Comments cite `pancakeswap/infinity-core@main` plus a date, not an immutable
   commit. Pin source, license and golden artifacts.
2. `get_id_from_price` saturates while upstream `safe24` reverts. Accept or remove
   this protocol deviation and prove exact reachable boundaries.
3. Helpers accept raw `u32` ids; `get_exponent` does not enforce uint24. Introduce
   `BinId` so invalid high bits cannot reach split/arithmetic paths.
4. Validate the allowed `bin_step`; zero and values above `BASIS_POINT_MAX` currently
   lack a clear public contract.
5. Math returns general `PrecompileError` with strings. Introduce a pure typed error
   and exhaustive owner mappings.
6. `pow` uses wrapping operations and tests tolerate ULP differences. Add exact
   differential vectors for the entire accepted base/exponent boundary.
7. `pow` rejects `|y| >= 2^20` although ids span 24 bits. Prove production inputs are
   always narrower or reject at a typed boundary.
8. Independently verify `log2` signed conversion/negation at zero, one, max and sign
   boundaries.
9. `mul_mod_512` loops 512 times; bind worst-case native CPU to ADR-B-EVM-001 gas.
10. Property-test schoolbook multiplication and division against arbitrary precision
    for adversarial all-max limbs and denominator boundaries.
11. `least_significant_bit(0)` returns 255, also a valid bit. Return `Option` or accept
    a nonzero word type.
12. Several bit helper preconditions exist only in comments. Encode them in types or
    checked results.
13. Audit every tree public method for raw ids whose high bits could be masked away.
14. Add failure injection after each level-0/1/2 write for every storage adapter.
15. Specify add-existing/remove-absent behavior and still detect inconsistent parent
    bits on those paths.
16. Add boundary traversal conformance at every byte edge and `MAX_BIN_ID`, including
    no skip/duplicate and cursor restart.
17. Deepen the six-method storage trait into an index authority that owns validation,
    mutation and rebuild rather than exposing partial writes.
18. Add offline verify/rebuild tooling driven by canonical module records.
19. Prove each monetary caller applies the intended rounding direction consistently
    at debit/credit and conservation boundaries.
