# Off-chain PoC scope and continuity audit

Date: 2026-07-24

Verdict: **PASS — UNBOUNDED-PARENT AMENDMENT VERIFIED; READY TO RESUME IMPLEMENTATION**

Reviewed child:
[`off-chain-poc.md`](../off-chain-poc.md), SHA-256
`3b04da489c900e494a81e5b944bdddc0316efb8b7869099efb93e9bb7999a9ee`

Pinned parent:
[`off-chain-computation.md`](../off-chain-computation.md), SHA-256
`21f0664c80f1e32afda83ca749a0ce2811668af47c21f2c04e7db80c99b89a99`

Framework review:
[`ocomp-framework-review.md`](ocomp-framework-review.md)

Proposed ADR/PFS formalization:
[`ADR-S-OCM-001`](../docs/adr/system/ADR-S-OCM-001-ocomp-kernel-and-typed-program-boundary.md)
through
[`ADR-S-OCM-004`](../docs/adr/system/ADR-S-OCM-004-certified-activation-job-fsm-and-protocol-versioning.md),
and [`PFS-002`](../docs/flows/002-off-chain-poc-protocol-flow.md).

## 1. Audit questions

1. Did every PoC behavior and required dependency from the parent reach the
   child?
2. Did the child add any runtime behavior, security claim, scale claim or
   release requirement outside the parent PoC?
3. Can the child be read as the next planning layer after the parent rather than
   as a competing architecture?
4. Is the boundary between PoC, BoundedMVP and TargetLarge machine/task-planning
   clear?

## 2. Parent-to-child completeness

| Parent authority | Required PoC content | Child coverage | Result |
|---|---|---|---|
| 0, 13.1 | internal operational kernel + one Lysis V1 typed protocol; multi-program wire deferred | 1, 15, 19.3 | exact clarification; no new acceptance |
| proposed ADR-S-OCM-001..004, PFS-002 | owner decisions and test flow formalize existing PoC requirements | links plus unchanged sections 13–15 | traceability only; all remain Proposed/Draft |
| 1.1–1.4 | real vertical slice through bounded work; no on-chain Lysis; q=3/4; one typed root activation | 1–3, 9–10 | exact |
| 1.5 | frozen per-interface envelope and activation-byte cap-1/cap/cap+1 public path; no total Tribute cap | 4, POC-07, POC-20..21 | exact |
| 1.6 | thirteen-step system demonstration | 13 | exact |
| 1.7 | PoC/MVP operational boundary; crypto/finality/atomicity/process split are not deferred | 2, 11, 19 | exact |
| 1.8 | six implementation slices | 16 | exact |
| 2.1 | terminal request phase, authenticated pre-admission, split/early-effect/intent/expiry atomicity | 5.1 | exact |
| 2.2 | tentative pin, finality proof, JobId, cursor discovery, terminal retry ordering | 5.2–5.3, 6.2 | exact |
| 2.3 | one activation transaction, exclusive deadline, atomic conflict/apply/retirement | 5.4–5.5, 10 | exact |
| 3 | canonical CE root chain and untrusted body transport | 6.1 | exact |
| 4 | sibling processes and nonfatal OCOMP failure boundary | 3, 12 | exact PoC subset |
| 5 | bounded UDS control and local filesystem CAS | 7 | exact PoC adapters |
| 6 | deterministic planner, constant-size `PlanCommitmentV1`, lazily derived `UnitSpecV1`, fixed streaming reducer | 8 | exact PoC subset |
| 7 | deterministic current-Lysis Map/Reduce semantics | 8 | exact PoC subset |
| 8.1 | finality-to-body/opening input authenticity and full CE fold | 6 | exact |
| 8.2 | full independent execution, one canonical digest and q certificate | 9 | exact |
| 8.4 | separate OCOMP key and result sign-once rule | 9.1–9.2 | exact result subset |
| 9 | bounded-page source/result retention and result-root activation | 6.2, 9–10 | exact PoC subset; production custody deferred |
| 10 | consensus FSM, expiry index, no intermediate result state | 5 | exact |
| 11 | authenticated protocol admission and local abstention | 4, 5.1, 6.2 | exact bounded subset |
| 12–13 | failure and trust boundaries | 3.1–3.2, 12 | exact selected PoC failures |
| 14.1 | unchanged core from PoC to BoundedMVP | 11.1 | exact |
| 14.2 | immutable bundle and canonical object interpretation | 11 | exact |
| 14.3 | fresh prepared disposable devnet, no supported-network migration | 4, 18–19 | exact |
| 14.8 steps 1–3 | PoC implementation milestone | 15–16 | exact |
| 15 | PoC closure is exactly parent section 1.6 | 13–14 | exact; supporting tests classified separately |
| 17 | relevant implementation precedents | 20 | selected PoC subset |
| 18 | current-code implementation gaps | 18 | exact PoC-relevant subset |

No PoC dependency is missing.

## 3. Scope containment

The active child profile remains:

```text
fresh disposable devnet
+ no artificial total Tribute bound
+ deterministic worker shards <= candidate 256 Tribute
+ shard-cap+1 accepted as the next shard, never dropped
+ constant-size input/plan/result commitments over counted chunk/unit roots
+ four validator domains
+ q = 3 independent full executions
+ complete typed result commitment in one normal activation transaction
+ on-chain evidence/structure verification only
+ real atomic root/scalar domain effects without an N-action loop
+ no synchronous fallback
```

The following parent features are mentioned only as exclusions or interface
evolution boundaries and are not PoC deliverables:

- supported-network preparation/arming/activation;
- changing committees and key handover;
- HSM/remote signer and mTLS;
- production launch broker, scheduling, recovery and SLOs;
- recursive proofs and `CountedRangeTreeV1`;
- DA/custody/repair;
- witness-based large state and contributor claims;
- billion-record performance or security claims;
- consensus program registry, generic program envelopes or dispatcher;
- a second Nod/Gem off-chain program and cross-program scheduling.

## 4. Child-only material classification

| Child material | Classification | Runtime scope effect |
|---|---|---|
| PoC-MUST/SCAFFOLD/DEFERRED labels | editorial classification | none |
| readiness contract and parent inheritance rules | planning governance | none |
| trust/access table | restatement of parent section 13 | none |
| POC-01..POC-26 IDs | evidence decomposition of existing requirements | none |
| implementation surfaces and six slices | planning inventory from parent 1.8/14.8 | none |
| section 22 decisions | first blocking planning tasks for unspecified bytes/adapters | none until explicitly frozen in the bundle |
| readiness review | audit metadata | none |
| kernel/Lysis ownership clarification | source/module boundary for already required behavior | none; no new wire object or acceptance step |
| deferred second-program qualification gate | prevents a false general-framework claim | none; Gem/Nod remain outside PoC |
| ADR-S-OCM-001..004 | separate owner records for existing kernel/input/evidence/activation decisions | none; status Proposed and no implementation claim |
| rewritten PFS-002 | test oracle and failure-flow decomposition of section 13 plus POC-01..26 | none; canonical acceptance and requirement wording remain in this child |

The evidence matrix explicitly preserves parent section 1.6 as the only PoC
system demonstration. Focused `CORE`, `FORK-GATE` and `COMPAT` checks validate
already required behavior; they do not introduce additional network features or
MVP chaos stories.

## 5. Corrections made during this audit

1. Added explicit parent-child inheritance rules.
2. Reclassified the evidence matrix so the thirteen-step story remains the PoC
   closure and supporting tests cannot be read as product scope.
3. Removed an over-specific requirement for a new public finalized-proof RPC.
   The parent requires `FinalizedIntentProofV1`, not a new endpoint. Planning
   must first determine whether existing finalized public data is sufficient or
   a bounded read adapter is necessary.
4. Made the exact field schemas, proof production, codecs, capacity values and
   storage/process adapters explicit first decision tasks rather than silently
   invented implementation details.
5. Made explicit that the PoC implements an internal operational kernel wired to
   one concrete Lysis V1 protocol. It does not add a consensus registry, generic
   envelopes or a second program.
6. Formalized the existing architecture as four owner ADRs and replaced the
   obsolete synchronous PFS-002 sequence with the PoC flow. This adds
   traceability/test orchestration, not runtime behavior.
7. Removed the mistaken complete-job `T<=512` admission rule. `JobIntentV1`
   covers arbitrary `N`; only pages, chunks, work units, the live worker pool
   and constant-size activation interfaces are bounded. Added root/count
   commitments and synthetic 10,000/1,000,000,000 planning evidence.

## 6. Logical continuity

The documents now form one directed chain:

```text
off-chain-computation.md
  selects the architecture and profiles
        |
        v
off-chain-poc.md
  narrows that architecture to the real PoC over bounded work
  and defines its planning/evidence boundary
        |
        +-> ADR-S-OCM-001..004
        |     own kernel/input/evidence/activation decisions
        |
        +-> PFS-002
        |     owns the cross-module protocol/test flow
        |
        v
outbe-plan/off-chain-poc-implementation-plan.md
  resolves section 22 parameters in dependency order
  and decomposes the six vertical slices
```

The child does not override the parent. A parent SHA change invalidates this
audit until the traceability review is rerun.

The framework clarification preserves that chain. It classifies existing
lifecycle/evidence/process responsibilities as kernel-owned and existing
input/planner/result/apply responsibilities as Lysis-owned. No consensus type,
hash domain, test ID or thirteen-step acceptance action was added or changed.

## 7. Readiness decision

`off-chain-poc.md` and the existing implementation dependency graph may now be
used to resume implementation. The amendment preserves the core architecture:
one complete parent job, any number of bounded work units, bounded local queues
and chunks, one constant-size result commitment and one constant-size
activation.

This verdict does not claim that the PoC is implemented or that
measurement-only capacity/network values are armed. Those remain explicit
implementation-plan gates.

Verification completed on 2026-07-24:

- `cargo fmt --all -- --check`;
- `cargo test -p outbe-ocomp-protocol`;
- `cargo test -p outbe-metadosis -p outbe-tribute`;
- `cargo test --locked -p outbe-e2e-harness --test ocomp_evidence_verifier`;
- `cargo check --workspace --all-targets`;
- `git diff --check`.

The behavioral tests prove that a full shard plus one starts the next shard,
larger populations cross multiple shards, and synthetic populations of 10,000
and 1,000,000,000 derive 40 and 3,906,250 primary work units without a
population-sized plan allocation. Tests do not inspect Rust source text to
claim behavior.

PoC completion remains:

1. the parent/child thirteen-step system demonstration passes;
2. the source-defined supporting PoC contract/fork checks pass;
3. no mocked validator domain, direct executor injection or on-chain Lysis is
   used.
