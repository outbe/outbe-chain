# T25 — Cross-cutting acceptance evidence and testnet soak

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §19 (Q13)
Depends on: T01–T24, T26–T36 (release gate)
Blocks: Stage 1 TESTNET activation (audit-v2 P0-8: this suite proves testnet evidence under Variant A;
the PRODUCTION/MAINNET gate is a separate, explicitly OPEN placeholder that requires the future off-chain
computation design and its own acceptance evidence — tracked in the roadmap, not closable here)

## Summary

Assemble and close the §19 acceptance-evidence package: the cross-cutting items no single task owns, an
auditable mapping of every §19 item to its owning tests, and the sustained testnet soak.

## Context

§19 defines 19 mandatory release gates. Most are owned by individual tasks (mapping below); this task owns
the cross-cutting remainder — proposer/validator cross-architecture equality, the consolidated fuzz program,
the full crash-fault-injection sweep as one suite, and the soak — plus the evidence ledger proving nothing
was dropped.

## Ownership map (audit ledger to maintain in this task)

| §19 item | Owner |
|---|---|
| 1 reference model | T04 |
| 2 golden vectors | T01–T05, T07 (pending-slot), T08 (events), T13 (artifact), T18 (proof) — sealed-root vectors live in T04; the FINAL vectors are the T24 Part B regenerated set (audit-final L-02) — the ledger links the regenerated fixtures, not the provisional ones |
| 3 SMT differential | T03/T04 |
| 4 cross-architecture root equality | **T25** |
| 5 call/revert/OOG/scheme adversarial | T07, T08, T09 |
| 6 body/event/leaf coherence, zero-sentinel | T07, T08, T12 |
| 7 wrong emitter / domain / downgrade / stale / wrong-ID | T06, T09, T18; projector wrong-emitter E2E — T20 (postfix PF-M04) |
| 8 ID generator vectors | T23 |
| 9 raw-hook rejection, system mutations | T09 |
| 10 ExEx matrix | T20 |
| 11 crash fault injection, restart matrix | T15, T16, T17, T20 → consolidated by **T25** |
| 12 snapshot conformance/adversarial | T22 |
| 13 performance report | T24 |
| 14 seal/persistence micro-benchmarks | T24 |
| 15 genesis rehearsal | T14 |
| 16 differential delete | T03 |
| 17 fuzzing program | seeded per-task → consolidated by **T25** |
| 18 testnet soak | **T25** |
| 19 cursor-skew | T21 |

## Scope

- Cross-architecture parity (§19.4): identical block-execution fixtures run on x86_64 and aarch64 CI,
  asserting equal `R_sealed`, state root, events, and attempt verdicts. The fixture set INCLUDES full
  Variant A execution (audit-final H-10): T23/T33 point-read and Lysis prefetch flows with identical
  storage/Mongo/prefetch fixtures on both architectures; equality covers state root, receipts, events,
  `R_sealed`, and resource verdicts.
- Consolidated crash-injection sweep (§19.11): one harness driving all eight crash points
  (Marshal sync, FCU, durable persist, SMT tx, ACK, Mongo commit, high-water, FinishedHeight) across
  restart-matrix classification.
- Fuzz program (§19.17): corpus + CI budget for mutation sequences, event/proof/snapshot codecs, malformed
  lengths, unknown versions, resource-boundary inputs; coverage tracked. Measurable pass criteria
  (audit-final M-08): fixed CI/nightly/release-candidate budgets (iterations and wall time), a pinned
  seed policy, persistent corpus storage, coverage-regression tracking, and a mandatory final-RC re-run
  of the full fuzz set.
- The written soak/release plan is T34 (early, no code deps — audit-v2 P1-9); this task EXECUTES it.
- Testnet soak (§19.18): full-block CE load, validator restarts, finalized catch-up, snapshot bootstrap,
  cross-source resume, datadir relocation, ExEx replay, local body loss, continued proof serving; executed
  per the release plan above; MongoDB in Docker on soak nodes.
- Testnet vs production split (audit P0-1.7): everything this suite proves under Variant A is TESTNET
  activation evidence. Variant A results are NEVER mainnet activation evidence; the production/mainnet
  gate requires the future off-chain-computation design with its own release evidence — tracked as an
  explicit open gate here, not silently closed.
- Real T19↔T21 peer-recovery E2E (two live nodes: node B recovers a lost body from node A's `outbe_getBody`
  with full proof verification) — the T21-stage test uses a fake peer server; the live pairing lands here.
- Stage 1 Variant A evidence matrix (audit v3 P1-10 — separate from the §19 ledger; every row needs a
  concrete artifact):
  | Variant A item | Owner |
  |---|---|
  | all body-dependent adapters covered (matrix complete) | T33 AC1 |
  | same-block prohibition (incl. reverted-first) | T33 AC4 |
  | typed unavailability behavior per class (ProjectionNotReady / BodyDataUnavailable) | T33 AC2/AC7 |
  | non-catchable outcome (adversarial catch test) | T33 AC3 |
  | single-validator Mongo outage → local recovery | T20 AC5 / T34 scenario |
  | finalized-candidate recovery (positive): row loss → abstain → quorum finalizes → parent-version recovery within window → finalized-block import → catch-up → role re-enable | T33 AC7b / T34 scenario |
  | finalized-candidate recovery (negative): window expired everywhere → validator remains NOT_READY, no false rejoin, operator manual paired-restore status per runbook | T33 AC7b negative / T34 scenario |
  | quorum Mongo outage → accepted testnet halt | T34 scenario (soak) |
  | no participation before readiness (fresh validator) | T22 AC1b / T28 AC1 |
  | production hard-disable | T33 AC6 |
  | manual paired-restore rehearsal (positive): stop → restore paired Reth+CE+body checkpoint (T34 contract) → verify → catch-up → READY → vote | **T25** soak scenario (audit-final H-03) |
- Immutable release candidate (audit-final H-11; construction fixed per postfix PF-B04 — a manifest
  merged into the RC source would change the very commit it attests): the source RC is FROZEN FIRST (an
  annotated tag on the RC commit); the release manifest is an EXTERNAL, content-addressed attestation
  artifact stored WITH the evidence (never merged into the RC source) binding: RC commit hash,
  `Cargo.lock` hash, genesis/spec/constants hashes, hardware profile ID, release-plan version, container
  image and toolchain digests. Any change to a bound input INVALIDATES the affected evidence per a
  written invalidation policy; mixed-revision evidence is rejected at sign-off; a repository commit that
  records the attestation is NOT the source RC.
- Evidence ledger: per-item link to tests/reports; release sign-off checklist.
- Docs artifact refresh (nice-to-have, NOT a §19 release gate): update
  `compressed_entities_v6_architecture.html` to the Q23 collection/Root-Catalog model (or retire it from
  the review trail) — the current diagram shows the superseded flat-256-shard scheme.

## Acceptance criteria

1. Every §19 item mapped to green, linkable evidence; no item marked "covered" without a concrete artifact.
2. Cross-arch CI job green on both architectures.
3. Consolidated crash sweep green; each crash point provably exercised (harness assertion, not assumption).
4. Soak executed per release plan with an archived report (restarts survived, proofs served throughout).
5. Every Variant A evidence-matrix row is linked to green evidence; no row is satisfied only by a §19
   ledger row (audit v4 P1-7).
6. Release manifest (audit-final H-11/postfix PF-B04): source RC tagged first; the external
   content-addressed manifest + invalidation policy published with the evidence; every ledger row
   references artifacts produced from the TAGGED RC revision.
7. Paired-restore rehearsal (audit-final H-03) executed on soak: restore → verify → catch-up → READY →
   vote, with archived evidence.

## Invariants

- Tests target module interfaces and observable roots/events/errors; private tree internals only via the
  differential suites. No formal verification required.

## Files

- `tasks/25-acceptance-suite.md` (ledger lives here), CI workflows, soak runbook under `docs/`/`outbe-plan/`
