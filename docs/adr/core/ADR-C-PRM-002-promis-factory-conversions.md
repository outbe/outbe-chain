# ADR-C-PRM-002: PromisFactory owns Promis conversion and Fidelity coupling

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Promis protocol maintainers
- **Scope:** `crates/core/promisfactory`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-C-GRT-001, ADR-C-GRT-002, ADR-C-FID-001, ADR-C-PRM-001
- **Related:** ADR-C-CRD-002, ADR-C-INX-002, ADR-C-GEM-002
- **Supersedes:** PromisFactory portions of former broad pre-space Gratis/economic aggregate (previously numbered 030)

## Context

PromisFactory is the workflow authority above the Promis ledger. It couples minted
Promis to Fidelity acquisition history and provides two user conversions: Promis to
native COEN and Promis to Gratis. It owns atomic choreography, not any underlying
ledger.

## Decision

The internal `mint(account, amount)` command is the only normal issuance seam for
GemFactory and IntexFactory. It mints Promis and appends an equal Fidelity active
cohort at canonical block time.

The public commands are:

```text
mineCoen(amount):
  burn caller Promis
  consume equal Fidelity quantity LIFO
  increase caller native COEN balance 1:1

# exposed on IGratisFactory (0x2003), not the Promis factory
mineFromPromis(amount):
  burn caller Promis
  mint equal Gratis through Gratisfactory's age-preserving seam
  do not mutate Fidelity
```

The Promis-to-Gratis command lives on the Gratis factory (`IGratisFactory.mineFromPromis`)
so the gratis mint stays where gratis minting is owned; it burns the caller's Promis
directly. The absence of Fidelity mutation during Promis-to-Gratis conversion is
deliberate: the same economic holding changes representation without resetting
acquisition age.
Every sequence and event is one EVM transaction.

## Authority and invariants

User ABI derives account from `msg.sender` and rejects native value. Internal mint
is privileged and its producers are exhaustively enumerated.

For every successful command:

- factory mint: `Promis +amount == Fidelity active +amount`;
- COEN mining: `Promis -amount == Fidelity active -amount == native COEN +amount`;
- Gratis conversion: `Promis -amount == Gratis +amount`, while Fidelity active
  quantity and timestamps are unchanged.

No downstream failure may leave only one side committed. Events are evidence of a
committed outcome, not a substitute for these equations.

## Replay, failure and security

Balance consumption prevents direct conversion replay. Internal producers must
provide their own one-time Gem/Intex consumption and call mint in the same frame.
Zero/insufficient amount, invalid account, cohort mismatch, native balance overflow
or Gratis failure reverts the complete command.

Internal Rust visibility is not sufficient access control. A new caller can create
unbacked Promis or bypass Fidelity and therefore requires an ADR/index update and
structural test.

## Compatibility and evidence

The 1:1 base-unit rules, age-preservation rule, selectors, events and caller set are
protocol economics. Inspected public dispatch and all three runtime paths. Current
tests do not establish exhaustive callers, injected rollback at every step, or
Fidelity quantity closure after arbitrary representation changes.

## Consequences

All Promis representation changes have one atomicity owner. The ledger stays simple,
and Fidelity provenance cannot accidentally be updated differently by each producer.

## Rejected alternatives

- **Mint directly from Gem/Intex:** it makes Fidelity optional.
- **Create a new Fidelity cohort on Gratis conversion:** users could reset or split
  age through representation changes.
- **Leave native increase outside the transaction:** burned Promis could be lost.
- **Make conversion rate caller-configurable:** it destroys deterministic supply.

## Open questions and technical debt

1. Add structural tests proving GemFactory and IntexFactory are the complete
   internal mint caller set and consume matching source units atomically.
2. Prove whether Metadosis/Desis unallocated Promis is eventually minted through
   this factory or represents a separate capacity concept; document the exact seam
   with ADR-C-PRM-003.
3. Add failure injection after Promis burn/mint, Fidelity mutation, native balance
   change, Gratis mint and event emission.
4. Define and test native-balance supply authority for the 1:1 COEN increase; ensure
   it does not bypass a global emission cap unintentionally.
5. Fidelity's current clamp on insufficient active cohorts can allow Promis burn
   without equivalent history consumption; ADR-C-FID-001 must make this a hard failure.
6. Add generated representation-change models proving age and quantity closure
   across repeated mint, convert-to-Gratis and mining sequences.
7. Define zero address/zero amount behavior for internal mint and public calls at the
   ABI boundary.
8. Pin event ordering when downstream modules also emit ledger events.
9. Add ABI-level replay/front-running tests; caller binding must prevent converting
   another account's balance.
10. Human economics review is required for both 1:1 conversions and provenance
    preservation.
