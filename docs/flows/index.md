# Protocol Flow Specifications

Protocol Flow Specifications (PFS) describe end-to-end behavior that crosses
multiple independently owned modules. They are the canonical source for integration
and e2e scenarios; ADRs remain the canonical source for architectural decisions,
module authority, state machines and local invariants.

## Why this is a separate document type

An ADR answers: **why is one architectural boundary shaped this way?** A PFS
answers: **what must happen across all boundaries for one protocol outcome to be
true?** Combining those questions creates oversized ADRs and weak module audits.

A PFS may import many ADRs but may not redefine their state or authority. If a flow
needs a behavior an ADR does not permit, that is an architectural conflict to
resolve, not an implicit override.

## Required flow contract

Every flow document contains:

- status, actors, trigger and referenced ADRs;
- an evidence-oriented acceptance contract: Source, Trigger, Environment,
  Canonical inputs, System under test, Expected response, Response measures and
  Failure guarantee;
- preconditions and canonical inputs;
- numbered success sequence with the owner of every step;
- transaction/checkpoint/finality boundaries;
- cross-module conservation and outcome invariants;
- externally observable evidence at ABI, receipt, RPC and projection layers;
- replay, retry, restart and partial-failure behavior;
- an e2e scenario matrix with stable scenario ids;
- current automation mapping and explicit coverage gaps; and
- open questions and technical debt.

Flow status vocabulary:

| Status | Meaning |
|---|---|
| Draft | Reconstructed from code; outcome contract is not accepted. |
| Accepted | Normative cross-module behavior. |
| Automated | Accepted and fully exercised through production interfaces. |
| Superseded | Replaced by a named flow. |

`Automated` requires every listed mandatory scenario, including failure/replay and
observable-state assertions. A single happy-path feature is insufficient.

## E2E scenario conventions

Scenario ids use `PFS-<flow>-<number>`, are stable, and should appear in Gherkin
scenario tags or test names. Each scenario declares the minimum topology and
external services. Assertions distinguish:

- **submitted** — a transaction hash exists;
- **executed** — canonical receipt succeeded;
- **finalized** — the containing block is finalized;
- **materialized** — off-chain projection/CE persistence reached the documented
  checkpoint; and
- **verified** — independent reads/proofs reconcile with canonical state.

Tests must never use “transaction sent” as evidence for a completed protocol flow.
Each matrix row maps to the strongest feasible level: live-node harness first,
then an in-process cross-module test, otherwise documentation-only with the missing
seam or unresolved policy named explicitly.

## Index

| Flow | Outcome | Principal ADRs | Status | Automation |
|---|---|---|---|---|
| [PFS-001](001-encrypted-tribute-materialization.md) | Encrypted Tribute offer becomes finalized, projected and authenticated | ADR-B-CNS-002, ADR-B-CNS-003, ADR-B-CLI-001, ADR-B-MCP-001, ADR-B-OCD-003 through ADR-B-OCD-006, ADR-B-OCD-013; ADR-S-TEE-001 through ADR-S-TEE-002; ADR-C-TRB-001 through ADR-C-TRB-002 | Draft | Partial: `tribute_projection.feature` |
| [PFS-002](002-worldwide-day-tribute-to-nod.md) | WorldwideDay advances and atomically transforms sealed Tributes to Nods | ADR-B-CNS-003; ADR-S-CYC-001, ADR-S-ORC-001; ADR-C-MET-001, ADR-C-LYS-001 | Draft | Partial: in-process WWD/Lysis/Nod/Gratis |
| [PFS-003](003-gratis-pledge-credis-repayment.md) | Gratis pledge opens Credis and installments release reclaim notes | ADR-B-SMA-001; ADR-S-ORC-001; ADR-C-GRT-001 through ADR-C-GRT-003, ADR-C-FID-001, ADR-C-CRD-001 through ADR-C-CRD-002, ADR-C-VLT-001 | Draft | Partial: in-process Credis lifecycle; proof/vault stubs |
| [PFS-004](004-intex-settlement-to-promis.md) | Intex issuance/qualification/settlement is mined into Promis | ADR-B-CNS-002, ADR-B-CNS-003, ADR-B-XCH-001; ADR-S-ORC-001; ADR-C-PRM-001 through ADR-C-PRM-003, ADR-C-VLT-001, ADR-C-TOK-001 through ADR-C-TOK-002, ADR-C-INX-001 through ADR-C-INX-007, ADR-C-DES-001 | Draft | Documentation-only full flow; module/Foundry fragments |
| [PFS-005](005-governance-vote-protocol-activation.md) | Validator vote schedules and activates a supported protocol version | ADR-B-CNS-003; ADR-S-VAL-001, ADR-S-GOV-001 through ADR-S-GOV-003 | Draft | Partial: live update + in-process edges |
| [PFS-006](006-validator-join-operation-and-exit.md) | Validator joins, earns, exits or is punished without partial cross-module state | ADR-B-NOD-001, ADR-B-GEN-001, ADR-B-CNS-001 through ADR-B-CNS-003; ADR-S-CYC-001, ADR-S-VAL-001, ADR-S-STK-001, ADR-S-RWD-001, ADR-S-SLS-001, ADR-S-KEY-001, ADR-S-ACC-001 | Draft | Partial: lifecycle/DKG/stale-join/downtime features |
| [PFS-007](007-zerofee-sponsorship-and-paid-fallback.md) | EIP-7702 delegation receives bounded sponsorship and retains paid fallback | ADR-B-GEN-001, ADR-B-EVM-001, ADR-B-TXP-001, ADR-B-CLI-001; ADR-S-FEE-001; ADR-C-AGR-001 | Draft | Live Rust/Cucumber feature; replay/restart gaps |
| [PFS-008](008-follower-sync-recovery-and-warm-promotion.md) | Followers synchronize, validators recover and warm data is promoted safely | ADR-B-NOD-001, ADR-B-CNS-001 through ADR-B-CNS-003, ADR-B-OPS-001; ADR-S-VAL-001, ADR-S-STK-001 | Draft | Live composite follower feature |

## Relationship to test documentation

The repository-wide [E2E evidence inventory](e2e-inventory.md) lists live
multi-node runners, in-process module compositions and Foundry suites without
conflating their verification boundaries.

`crates/testing/e2e-harness/README.md` explains how to run the harness. Feature
files implement scenarios. This directory specifies what outcomes those features
must prove. Operational demo instructions belong in `docs/` runbooks and may cite a
PFS, but manual screenshots or Mongo queries do not replace automated evidence.

## Open questions and technical debt

- Decide whether accepted PFS changes require the same reviewers as every imported
  ADR or a separate integration/e2e owner.
- Add a machine-readable manifest mapping scenario ids to feature files and CI jobs.
- Define a CI check that every `Automated` flow has all mandatory scenario ids and
  that removed tests cannot silently leave stale status.
- Reconstruct remaining protocol flows: daily emission/rewards, bridge
  auction/proceeds, snapshot recovery, Oracle publication/admission and validator
  committee resharing (if it cannot be expressed as a scenario of PFS-006).
- Add flow links back from each participating ADR without copying flow sequences
  into module decision records.
