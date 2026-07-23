# Off-chain PoC scope and continuity audit

Date: 2026-07-23

Verdict: **PASS — READY_FOR_IMPLEMENTATION_PLANNING**

Reviewed child:
[`off-chain-poc.md`](../off-chain-poc.md), SHA-256
`fe9d14a6a697a3b4acef1e013188a9a82f21d5665664de5b72e374dbfbd534ce`

Pinned parent:
[`off-chain-computation.md`](../off-chain-computation.md), SHA-256
`5bc67987f36a0dd0e5da4c66ce44b1e11ab479f9921350a055dccc6cd0779834`

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
| 1.1–1.4 | real bounded vertical slice; no on-chain Lysis; q=3/4; one typed activation | 1–3, 9–10 | exact |
| 1.5 | frozen small envelope and cap-1/cap/cap+1 public path | 4, POC-21 | exact |
| 1.6 | thirteen-step system demonstration | 13 | exact |
| 1.7 | PoC/MVP operational boundary; crypto/finality/atomicity/process split are not deferred | 2, 11, 19 | exact |
| 1.8 | six implementation slices | 16 | exact |
| 2.1 | terminal request phase, authenticated pre-admission, split/early-effect/intent/expiry atomicity | 5.1 | exact |
| 2.2 | tentative pin, finality proof, JobId, cursor discovery, terminal retry ordering | 5.2–5.3, 6.2 | exact |
| 2.3 | one activation transaction, exclusive deadline, atomic conflict/apply/retirement | 5.4–5.5, 10 | exact |
| 3 | canonical CE root chain and untrusted body transport | 6.1 | exact |
| 4 | sibling processes and nonfatal OCOMP failure boundary | 3, 12 | exact PoC subset |
| 5 | bounded UDS control and local filesystem CAS | 7 | exact PoC adapters |
| 6 | deterministic planner, `UnitSpecV1`, fixed units/reducer | 8 | exact bounded subset |
| 7 | deterministic current-Lysis Map/Reduce semantics | 8 | exact bounded subset |
| 8.1 | finality-to-body/opening input authenticity and full CE fold | 6 | exact |
| 8.2 | full independent execution, one canonical digest and q certificate | 9 | exact |
| 8.4 | separate OCOMP key and result sign-once rule | 9.1–9.2 | exact result subset |
| 9 | bounded source retention and result bytes in canonical block data | 6.2, 10 | exact bounded subset |
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
+ T <= generated bound, proposed 256
+ four validator domains
+ q = 3 independent full executions
+ complete typed result in one normal activation transaction
+ on-chain evidence/structure verification only
+ real atomic domain effects
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

## 6. Logical continuity

The documents now form one directed chain:

```text
off-chain-computation.md
  selects the architecture and profiles
        |
        v
off-chain-poc.md
  narrows that architecture to the real bounded PoC
  and defines its planning/evidence boundary
        |
        +-> ADR-S-OCM-001..004
        |     own kernel/input/evidence/activation decisions
        |
        +-> PFS-002
        |     owns the cross-module protocol/test flow
        |
        v
future implementation plan
  resolves section 22 parameters first,
  then decomposes the six vertical slices
```

The child does not override the parent. A parent SHA change invalidates this
audit until the traceability review is rerun.

The framework clarification preserves that chain. It classifies existing
lifecycle/evidence/process responsibilities as kernel-owned and existing
input/planner/result/apply responsibilities as Lysis-owned. No consensus type,
hash domain, test ID or thirteen-step acceptance action was added or changed.

## 7. Readiness decision

`off-chain-poc.md` may be used now to create the implementation dependency graph
and task set.

It is not yet a byte-complete protocol specification. The implementation plan
must place each child section 22 decision ahead of dependent consensus/runtime
work. This is normal first-wave planning work and is not an unresolved
architectural choice.

PoC completion remains:

1. the parent/child thirteen-step system demonstration passes;
2. the source-defined supporting PoC contract/fork checks pass;
3. no mocked validator domain, direct executor injection or on-chain Lysis is
   used.
