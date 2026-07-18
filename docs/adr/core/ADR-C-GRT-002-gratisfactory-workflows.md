# ADR-C-GRT-002: Gratisfactory owns Gratis business workflows

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Gratis protocol maintainers
- **Scope:** `crates/core/gratisfactory`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-C-GRT-001, ADR-C-GRT-003, ADR-C-FID-001
- **Related:** ADR-C-PRM-001, ADR-C-PRM-002
- **Supersedes:** Gratisfactory portions of former broad pre-space Gratis/economic aggregate (previously numbered 030)

## Context

Gratis deliberately has no public transfer or policy engine. Gratisfactory is the
orchestrator that turns domain outcomes into Gratis, converts Gratis into native
COEN, and couples public pledge/unpledge commands to shielded notes and Fidelity.
It owns workflow atomicity but not the underlying ledgers or proof system.

## Decision

Gratisfactory exposes these commands:

- mint earned Gratis and record a Fidelity acquisition cohort;
- mint Gratis converted from Promis without creating a new cohort, preserving the
  original acquisition age;
- mine native COEN by burning Gratis and consuming Fidelity cohorts;
- pledge one supported denomination by appending a shielded commitment and moving
  the exact Gratis amount into shared Credis escrow;
- unpledge by verifying and consuming a bound shielded note, then releasing the
  exact escrow amount to the caller.

The orchestrator validates caller, denomination, eligibility and all derived
amounts before mutation where possible. Every workflow is one EVM transaction
across Gratis, Fidelity, GratisPool, native balances and emitted events.

### Required ordering

Mint and mining must either commit both economic balance and Fidelity history or
neither. Pledge must not leave a spendable note without escrow backing, nor escrow
without the corresponding note. Unpledge verifies proof and replay guards before
releasing escrow; a downstream failure rolls the consumed nullifier back.

Promis-to-Gratis conversion uses an explicitly separate privileged entrypoint so
ordinary mint cannot accidentally preserve age and conversion cannot reset it.

## Interfaces and invariants

The ABI dispatch is the user authority boundary. Internal mint entrypoints are
restricted to an enumerated set of factories/system modules.

For each successful command:

- ordinary mint: `Gratis +amount` and Fidelity active cohorts `+amount`;
- mine COEN: `Gratis -amount`, Fidelity active quantity `-amount`, native receiver
  balance `+amount` under the defined 1:1 base-unit rule;
- pledge: one pool commitment plus escrow/pledged `+denomination_amount`;
- unpledge: one newly consumed nullifier plus escrow/pledged
  `-denomination_amount` and caller liquid `+amount`;
- conversion mint changes Gratis but preserves pre-existing Fidelity provenance.

## Failure, replay and security

Invalid proof/root/nullifier, unsupported denomination, insufficient balance,
failed native transfer or downstream storage error reverts the whole command.
Nullifiers provide pledge-note replay protection; upstream modules provide mint
replay protection. Internal APIs are not safe merely because they are absent from
the Solidity ABI.

Fidelity league and denomination eligibility are protocol policy. They may not be
left as a sentinel check or supplied by an untrusted caller. Receiver/action/chain
binding is delegated to ADR-C-GRT-003.

## Compatibility and evidence

Command selectors, denomination mapping, 1:1 COEN conversion, Fidelity provenance
rule and authorized internal callers require activation review. Production paths
for all five workflows were inspected. Current tests do not close caller authority,
all nested rollback points, or the unfinished pledge eligibility policy.

## Consequences

The Gratis ledger stays policy-free and the public interface remains task-oriented.
module audits can test Gratisfactory as the atomicity owner without making it own
Merkle or cohort internals.

## Rejected alternatives

- **Put proof verification in Gratis:** it couples the ledger to one privacy scheme.
- **Reset Fidelity age on Promis conversion:** it changes retained-value economics.
- **Release escrow before proof consumption:** copied transactions could race value.
- **Allow arbitrary internal mint callers:** all cap/issuance policy becomes bypassable.

## Open questions and technical debt

1. Pledge eligibility contains an ineffective placeholder Fidelity check. Specify
   the exact league/denomination matrix and reject unsupported users explicitly.
2. Enumerate and structurally test every privileged mint and conversion caller.
3. Inject failure after every Gratis, Fidelity, pool, native-balance and event step
   and compare semantic pre-state to prove rollback.
4. Confirm whether native COEN conversion is exactly 1:1 in base units under all
   supply/fee conditions and name the source of minted native balance.
5. Define zero amounts, zero commitments and duplicate commitments consistently.
6. Prove Promis conversion cannot be called without a matching Promis burn in the
   same frame; the age-preserving entrypoint is otherwise an issuance bypass.
7. Specify whether an account may transfer/rotate control of outstanding shielded
   notes independently of the pledged-balance attribution.
8. Add ABI-level end-to-end tests for pledge/unpledge using production proof
   verification, not only direct internal APIs.
9. Define gas/capacity bounds for commitment insertion and proof verification.
10. Human economics review is required for eligibility and age preservation.
