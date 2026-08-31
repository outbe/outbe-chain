# ADR-C-PRM-002: PromisFactory owns Promis conversion

- **Status:** Proposed; profiled against the current implementation
- **Date:** 2026-08-31
- **Decision owners:** Promis protocol maintainers
- **Scope:** `crates/core/promisfactory`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-C-GRT-001, ADR-C-GRT-002, ADR-C-FID-001, ADR-C-PRM-001
- **Related:** ADR-C-CRD-002, ADR-C-INX-002, ADR-C-GEM-002
- **Supersedes:** PromisFactory portions of former broad pre-space Gratis/economic aggregate (previously numbered 030)

## Context

PromisFactory is the workflow authority above the Promis ledger. It provides two user
conversions — Promis to native COEN and Promis to Gratis — and one internal issuance
seam. It owns atomic choreography, not any underlying ledger.

Promis is fidelity-neutral: the ledger carries no acquisition cohorts and no holding
age. Fidelity is a property of the Gratis ledger, where an acquisition cohort opens on
every mint and a sale consumes one on every burn to COEN. A conversion that produces
Gratis therefore starts a holding, and one that consumes Promis ends nothing.

## Decision

Each conversion is hosted by the factory of the token it consumes. The factory of the
token it produces supplies the mint, called as an internal cross-module seam, so a
ledger is only ever minted by its own owner.

The internal `mint(account, amount)` command is the only normal issuance seam for
GemFactory and IntexFactory. It mints Promis and touches nothing else.

The public commands are:

```text
mineCoen(amount):
  burn caller Promis
  increase caller native COEN balance 1:1

mineGratis(amount):
  burn caller Promis
  mint equal Gratis through GratisFactory's mint seam,
    which opens a Fidelity acquisition cohort at canonical block time
```

Both commands are exposed on `IPromisFactory` (0x2337), the factory of the burned
token. `mineGratis` carries two modify authorizations, one per confidential ledger,
each binding `amount` to that ledger's own current op-nonce.

A Promis holding converted to Gratis begins its Fidelity age at conversion, exactly as
any other gratis acquisition does. Age is not carried across the conversion, because
the Promis side never had any.

Every sequence and event is one EVM transaction.

## Authority and invariants

User ABI derives account from `msg.sender` and rejects native value. Internal mint is
privileged and its producers are exhaustively enumerated.

For every successful command:

- factory mint: `Promis +amount`, with Fidelity untouched;
- COEN mining: `Promis -amount == native COEN +amount`, with Fidelity untouched;
- Gratis conversion: `Promis -amount == Gratis +amount`, with one Fidelity acquisition
  cohort of `amount` opened at block time.

No downstream failure may leave only one side committed. Events are evidence of a
committed outcome, not a substitute for these equations.

## Replay, failure and security

Balance consumption prevents direct conversion replay. Internal producers must provide
their own one-time Gem/Intex consumption and call mint in the same frame. Zero or
insufficient amount, invalid account, native balance overflow, or a Gratis or Fidelity
failure reverts the complete command.

Each confidential ledger authorizes its own side independently: a valid Promis burn
authorization grants nothing on the Gratis ledger, and the conversion reverts unless
both hold.

Internal Rust visibility is not sufficient access control. A new caller can create
unbacked Promis and therefore requires an ADR/index update and structural test.

## Compatibility and evidence

The 1:1 base-unit rules, the hosting rule, selectors, events and caller set are
protocol economics. Inspected public dispatch and all three runtime paths. Current
tests do not establish exhaustive callers or injected rollback at every step.

## Consequences

All Promis representation changes have one atomicity owner. A reader looking for what
consumes a token finds it on that token's own factory, and a ledger is minted only by
the module that owns it.

Because conversion to Gratis opens a fresh cohort, moving value through Promis and back
into Gratis restarts its Fidelity age. This is a property of the model, not an
oversight: age measures an uninterrupted Gratis holding.

## Rejected alternatives

- **Mint directly from Gem/Intex:** it bypasses the single issuance seam.
- **Host the conversion on the produced token's factory:** the consuming ledger is the
  one under authorization, and it splits the burn from the command that causes it.
- **Preserve a Fidelity age across the conversion:** Promis carries no age to preserve,
  so any carried value would have to be invented.
- **Leave native increase outside the transaction:** burned Promis could be lost.
- **Make conversion rate caller-configurable:** it destroys deterministic supply.

## Open questions and technical debt

1. Add structural tests proving GemFactory and IntexFactory are the complete internal
   mint caller set and consume matching source units atomically.
2. Prove whether Metadosis/Desis unallocated Promis is eventually minted through this
   factory or represents a separate capacity concept; document the exact seam with
   ADR-C-PRM-003.
3. Add failure injection after Promis burn/mint, native balance change, Gratis mint and
   event emission.
4. Define and test native-balance supply authority for the 1:1 COEN increase; ensure it
   does not bypass a global emission cap unintentionally.
5. Define zero address/zero amount behavior for internal mint and public calls at the
   ABI boundary.
6. Pin event ordering when downstream modules also emit ledger events.
7. Add ABI-level replay/front-running tests; caller binding must prevent converting
   another account's balance.
8. Human economics review is required for both 1:1 conversions.
