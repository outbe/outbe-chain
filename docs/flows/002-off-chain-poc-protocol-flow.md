# PFS-002: Off-chain PoC transforms sealed Tributes into Nods

- **Status:** Draft; target PoC specified, implementation absent
- **Actors:** Cycle, Metadosis, OCOMP kernel, four validator domains, node
  attestation gates, snapshot exporters, supervisors, workers, CAS stores,
  untrusted relay, Tribute/CE, Fidelity, Oracle, Lysis, NodFactory, Intex,
  Desis, PromisLimit
- **Trigger:** Terminal Metadosis inspection selects an eligible non-empty
  sealed READY WorldwideDay
- **Topology/services:** Fresh four-validator PoC devnet; one independent node,
  supervisor, exporter, worker pool and CAS per validator domain; public
  RPC/txpool/P2P/block path; transaction-capable Mongo body transport
- **Referenced ADRs:** ADR-B-CNS-001, ADR-B-CNS-003, ADR-B-EVM-001,
  ADR-B-TXP-001, ADR-B-RPC-001, ADR-B-OCD-004 through ADR-B-OCD-015,
  ADR-B-CAP-001, ADR-B-SUP-001, ADR-B-TST-001; ADR-S-CYC-001,
  ADR-S-ORC-001, ADR-S-KEY-001, ADR-S-OCM-001 through ADR-S-OCM-004;
  ADR-C-TRB-001, ADR-C-NOD-001, ADR-C-NOD-002, ADR-C-MET-001,
  ADR-C-LYS-001, ADR-C-FID-001, ADR-C-PRM-003, ADR-C-DES-001
- **Supersedes:** The synchronous sequence previously recorded by PFS-002

## Outcome

One sealed non-empty WorldwideDay becomes one finalized OCOMP job. Four
validator domains independently reconstruct and execute the same Lysis V1
program off-chain. Any relay may submit three matching attestations and the
complete bounded typed result. Consensus verifies evidence and structure without
executing Lysis, then atomically commits the exact Nod/contributor/Tribute/
carry-over/Metadosis activation effects. Desis is already committed in the
GREEN request phase.

No quorum means exclusive-deadline expiry, preserved Lysis budget, no repeated
auction and no Nod. There is no synchronous fallback.

## Acceptance contract

- **Source:** Publicly issued Tributes in one sealed authenticated WWD
  collection.
- **Trigger:** Terminal Metadosis inspection of an eligible non-empty READY day
  under the active PoC protocol bundle.
- **Environment:** Four independently configured validator domains with
  finalizing consensus, separate OCOMP processes/artifacts, valid Oracle and
  Fidelity state, retained CE/Mongo inputs and generated PoC caps.
- **Canonical inputs:** Finalized `JobIntentV1`, request block/state root,
  sealed CE/WWD roots, exact count/nominal, frozen Metadosis values,
  budget split, activation preconditions, authenticated Tribute bodies,
  Fidelity/Oracle openings, committee snapshot and `ProtocolBundleHash`.
- **System under test:** Public Tribute/Metadosis path, consensus finality,
  OCOMP control and bulk planes, snapshot export, deterministic Lysis execution,
  node attestation, untrusted relay, public activation transaction, certified
  domain apply and public outcome reads.
- **Expected response:** One `COMPLETED` job/day, exact terminal receipt and
  active generation; expected Nods, contributor totals, Tribute retirement,
  request-phase Desis brief and carry-over credits; byte-identical results across independent
  domains and worker schedules.
- **Response measures:** Exact 13-step demonstration passes; all
  `POC-01..POC-26` requirements have evidence at their declared layer; no
  direct state/executor injection or on-chain Lysis trace occurs.
- **Failure guarantee:** Invalid/unavailable local work produces no signature;
  invalid activation changes no state; insufficient quorum expires and releases
  the attempt while consensus and the previous active state continue.

## Canonical authority and trust map

| Fact | Authority | Explicitly not authority |
|---|---|---|
| request exists | canonical `JobIntentV1` state | request event |
| request is immutable/final | request block finality proof and state root | local node tip or wall time |
| complete Tribute population | sealed CE/WWD collection root, traversal, count and nominal | Mongo query/page count |
| raw body bytes | canonical body that verifies against its committed leaf | Mongo/CAS location |
| Lysis semantics | job-pinned `ProtocolBundleV1` | worker binary label or negotiation |
| unit membership/order | canonical plan and `UnitId` | scheduler/worker completion order |
| one validator vote | one eligible validator index and OCOMP key epoch | process/worker count |
| result equality | exact canonical `ResultDigest` | “equivalent” JSON or relay choice |
| activation authority | verified certificate plus private `CertifiedLysisActivation` | relay or generic writes |
| output truth | finalized public state/receipts/proofs | supervisor journal or Mongo alone |

## State and artifact model

### Consensus state

```text
Metadosis READY
  -> OFFCHAIN_PENDING(IntentId, pending_nonce)
       -> COMPLETED(JobId, ResultDigest, TerminalReceipt)
       -> READY(next nonce) after EXPIRED / CONFLICTED / CANCELED job
```

`RUNNING`, `EXPORTING`, `PLANNED` and worker progress never become consensus
states.

### Local validator-domain state

```text
DISCOVERED
  -> PINNED_FINALIZED
  -> EXPORTED(InputManifestHash)
  -> PLANNED(PlanHash)
  -> EXECUTED(ResultDigest)
  -> ATTESTED(SignOnceSubject)
  -> TERMINAL_OBSERVED
```

This journal is restart/reconciliation evidence, not chain authority.

### Persisted artifacts

| Artifact | Owner/store | Required binding |
|---|---|---|
| tentative/finalized retention pin | node/checkpoint manager | candidate/finalized block, roots, IntentId/JobId |
| `JobIntentV1` and job FSM | canonical chain state | pending nonce, budget split, activation preconditions, deadline, bundle |
| canonical input chunks | validator-local CAS | digest, length, manifest membership |
| `InputManifestV1` | exporter/CAS | JobId, checkpoint, roots, count/totals, openings |
| `UnitSpecV1`/unit artifacts | supervisor/workers/CAS | JobId, plan, program semantics, UnitId |
| reducer/result artifact | supervisor/CAS | exact typed result and ResultDigest |
| sign-once record | node attestation gate | key epoch, JobId, attempt, purpose, digest |
| activation transaction/receipt | canonical block data | finality proof, typed result, certificate |
| active generation/terminal receipt | canonical state | JobId, result/effect commitments |

## Preconditions

1. The PoC fork/profile and one exact `ProtocolBundleV1` are active on a fresh
   disposable devnet.
2. The generated WWD input fits the frozen Tribute, artifact, activation and
   block caps.
3. Four result-validator identities and their separate OCOMP keys are registered;
   `n=4`, `q=3`.
4. The day is READY, its Tribute collection is sealed and non-empty, and the
   exact current attempt has no live predecessor.
5. Oracle/Fidelity inputs, the budget split and every activation precondition
   can be frozen at the request logical context.
6. Every validator can either retain/reconstruct the finalized input or
   explicitly abstain; local admission cannot change chain eligibility.

## Normative success protocol and test oracle

| Step | Owner | Message/transition | Durable output | Observable test oracle | PoC requirements |
|---:|---|---|---|---|---|
| 1 | client/Tribute | issue bounded heterogeneous Tributes through public transactions | canonical Tribute/CE state and receipts | successful finalized receipts; public bodies/proofs show leagues/currencies/exclusion fixtures | POC-01, POC-22 |
| 2 | CE/Metadosis | seal WWD; split `day_limit`; GREEN dispatches `auction_base` to Desis or RED credits it to carry-over; create an intent for `lysis_budget` without Lysis | split receipt, `JobIntentV1`, expiry index, request event, tentative pin | finalized diff proves the request-phase effect happened once and shows zero new Nod/contributor/Tribute-consume/unused-Lysis carry-over effect | POC-02, POC-03, POC-23 |
| 3 | consensus/node | finalize request block and derive exact `JobId`/finality proof | finalized cursor entry and finalized pin | all four nodes report the same canonical job binding; adversarial proof vectors reject | POC-04 |
| 4 | supervisor | discover by finalized cursor; event may only reduce latency | local `DISCOVERED` journal | dropping the request subscription still discovers exactly one job | POC-05 |
| 5 | node/exporter | open read-only checkpoint lease; full-fold CE; verify raw bodies/openings; publish manifest/chunks | finalized pin, `InputManifestV1`, CAS objects | every domain independently reconstructs exact root/count/nominal/opening commitments; Mongo/CAS mutation rejects | POC-06, POC-09, POC-20 |
| 6 | supervisor/planner | derive canonical plan and all `UnitSpecV1`/`UnitId`s | `PlanHash`, unit objects | frozen bytes/hashes match golden vectors in all domains | POC-07 |
| 7 | workers/reducer | execute immutable units, retry freely and reduce in fixed order | unit artifacts and `BoundedLysisResultV1` | 1/2/4 workers and randomized completion/retry yield byte-identical result; reference corpus matches | POC-01, POC-08 |
| 8 | node attestation gate | reload job, verify candidate/caps and durably sign one exact digest | sign-once journal plus one signature/domain | exact retry returns same signature; second digest refuses after restart | POC-12 |
| 9 | untrusted relay | group three distinct matching signatures and build activation transaction | public transaction bytes | stopped fourth supervisor is not used; duplicate/wrong/mixed signer sets reject | POC-10, POC-13, POC-19 |
| 10 | RPC/txpool/P2P/proposer/import | submit and include one bounded `activateLysis` transaction | canonical transaction/receipt candidate | cap-1/cap succeeds under profile; cap+1 rejects consistently across public path/replay | POC-20, POC-21 |
| 11 | OCOMP/Lysis verifier | verify terminal/live job, finality, bundle, deadline, certificate, typed result and activation preconditions without Lysis execution | private `CertifiedLysisActivation` in execution frame only | one-byte, JobId, order, root/count, precondition and deadline mutations reject with no state diff; trace contains no Lysis/Fidelity/Oracle calculation | POC-03, POC-14, POC-18 |
| 12 | certified domain owners | apply Nod/contributor/Tribute/carry-over/Metadosis effects and verify four activation receipts plus the request split receipt in one checkpoint | `COMPLETED`, active generation and terminal/effect receipts | representative owner failure or receipt mutation rolls back activation effects; Desis is untouched; delayed activation changes only activation metadata | POC-15, POC-16, POC-17, POC-25 |
| 13 | consensus/client | finalize activation and verify outputs through public interfaces | finalized state, receipts, CE roots/proofs | every expected effect and conservation equation verifies; old Tribute partition is logically retired; no supervisor/CAS read is used as outcome authority | POC-22 |

## Exact PoC acceptance choreography

The PoC is complete only when this exact story passes from public Tribute
issuance through public Nod reads on a four-validator devnet:

1. issue a bounded WWD with different Fidelity leagues, currencies and at least
   one `exclude_from_intex_issuance` Tribute;
2. seal the WWD and reach terminal Metadosis;
3. inspect the finalized split and `JobIntent`; prove the request-phase effect
   happened once and there are zero new Nod/contributor/Tribute-consume effects;
4. stop one validator’s supervisor;
5. show the other three domains independently rebuild the same input root and
   produce the same `ResultDigest`;
6. submit their certificate and exact typed result in one activation transaction
   through the untrusted relay;
7. finalize it and query every expected Nod, contributor total, Metadosis state,
   request-phase Desis brief, carry-over credit and retired Tribute partition;
8. compare the result with an offline reference/golden corpus, never an on-chain
   Lysis execution;
9. repeat with 1, 2 and 4 workers and randomized completion order;
10. request a second digest signature for the same job and observe sign-once
    refusal; mutate one result byte, signer, JobId and ordering field and observe
    consensus rejection;
11. delay otherwise identical activations by different block counts and prove
    byte-identical Nod/contributor/Tribute/carry-over results with no repeated
    request effect;
12. run another job with two validators unavailable and observe expiry,
    preserved budget, no repeated auction and no Nod;
13. record a trace proving no on-chain call to Lysis, Fidelity or Oracle
    calculation paths.

Direct executor invocation, direct storage injection or a central calculator
does not satisfy this acceptance story.

## E2E scenario matrix

`PFS-002-01..08` retain the intent of the previous Draft matrix. New OCOMP
scenarios append from `PFS-002-09`; an implementation must not renumber them to
match test order. Rows explicitly marked DEFERRED preserve a stable historical
scenario identity but are not PoC acceptance requirements.

| Id | Scenario | Given / When / Then | Minimum topology and external services | Oracle/evidence | PoC requirements |
|---|---|---|---|---|---|
| PFS-002-01 | complete populated-day PoC demonstration | Given the heterogeneous bounded WWD, when the exact thirteen-step acceptance choreography runs, then every success, mutation, isolation and expiry assertion closes with exact conserved outputs | four validator domains, Mongo/CE, OCOMP processes, public RPC/P2P | finalized public state/proofs, reference corpus and thirteen-step report | POC-01..POC-25 as decomposed below |
| PFS-002-02 | empty Tribute compatibility branch | Given a READY sealed empty partition, when terminal Metadosis runs, then no OCOMP job/Nod exists and the exact direct empty-branch remainder/retirement commits | four nodes; compute plane may be stopped | finalized Metadosis/Promis/CE public reads | POC-26 |
| PFS-002-03 | zero-limit compatibility branch | Given zero Lysis limit, when terminal Metadosis runs, then the pinned FAILED/no-brief branch commits with no Tribute/Nod mutation | four nodes; no OCOMP requirement | receipt and public state diff | POC-26 |
| PFS-002-04 | authenticated totals/body mismatch | Given one omitted/changed Mongo body or opening, when exporter reconstructs the job input, then root/count/nominal verification fails and no signature exists | one validator domain plus real Mongo/CE checkpoint; repeat in four-domain story | exporter typed failure and absent sign-once record | POC-06 |
| PFS-002-05 | duplicate owner/day identity | Given one canonical Tribute identity, when a duplicate is submitted before sealing, then admission rejects and no duplicate enters the later manifest/result | four nodes, public Tribute path and Mongo/CE proof | reverted receipt, unchanged indexes/bodies/proofs | POC-26; imported PFS-001 admission evidence |
| PFS-002-06 | certified owner failure | Given valid evidence/result, when one later Nod/effect owner fails, then activation/job/domain effects all roll back | four nodes and public activation; focused production-seam failpoint | complete canonical pre/post state and receipt trace | POC-15 |
| PFS-002-07 | CE persistence/replay recovery | Given activation execution reaches CE persistence, when persistence/restart is faulted, then recovery exposes either the full certified roots/outcome or pre-state, never a partial generation | four nodes with real Reth/CE MDBX restart | **DEFERRED to BoundedMVP:** historical scenario ID; POC-25 requires normal finalized replay, not this crash matrix | none |
| PFS-002-08 | long timestamp/backlog compatibility | Given multiple due days/slots across a timestamp jump, when block lifecycle catches up, then canonical ordering processes each eligible request once and preserves non-PoC branches | four nodes; bounded generated backlog | **DEFERRED:** historical scenario ID; backlog policy remains explicit debt | none |
| PFS-002-09 | lost request event | Given a finalized request and dropped subscription event, when supervisor resumes its finalized cursor, then it discovers the exact job once | one real validator domain, repeated across four; UDS | cursor/journal plus canonical job read | POC-05 |
| PFS-002-10 | orphaned request and tentative pin | Given a tentative pin/local prework, when the request candidate reorgs before finality, then work is non-signable and the pin releases | finalizing/reorg-capable four-node harness | canonical hash mismatch, pin journal, attestation refusal | POC-23 |
| PFS-002-11 | CAS corruption and TOCTOU | Given an exported manifest, when a chunk is truncated/reordered/changed before or during worker consumption, then stream digest/membership fails or the chunk is rebuilt and no bad result is signed | one real exporter/worker/CAS domain | expected/actual digest and unchanged sign journal | POC-06, POC-20 |
| PFS-002-12 | deterministic worker schedules | Given one exact job, when it runs with 1, 2 and 4 workers plus random kills/retries/order, then plan/result bytes remain identical | one domain is sufficient for schedule variants; compare all four domains | golden plan/unit/result hashes | POC-07, POC-08 |
| PFS-002-13 | one validator domain unavailable | Given one stopped supervisor and four live nodes, when remaining domains execute, then three distinct matching signatures activate while finality continues | four nodes, three compute domains | process health, certificate indexes and finalized output | POC-09, POC-10, POC-19 |
| PFS-002-14 | two validator domains unavailable | Given two stopped supervisors, when the exclusive deadline arrives, then begin-zone expiry advances the attempt with the same Lysis budget, no repeated auction and no Nod | four nodes, two compute domains | finalized FSM/budget/state diff | POC-11, POC-18, POC-19 |
| PFS-002-15 | sign-once equivocation/restart | Given one durable signature subject, when a different digest is requested before and after node restart, then the gate refuses and retains the first binding | one real node attestation gate and durable journal | signature/journal bytes and typed refusal | POC-12 |
| PFS-002-16 | certificate signer mutation | Given a valid candidate, when duplicate/wrong-epoch/unknown/replaced signer evidence is submitted, then activation rejects with no state change | four nodes and public activation path | failed receipt and canonical pre/post equality | POC-13 |
| PFS-002-17 | result/job/order mutation | Given valid evidence/result, when one result byte, `JobId`, order, count or root changes, then digest/binding verification rejects atomically | four nodes and public activation path | failed receipt; live job remains unchanged | POC-14 |
| PFS-002-18 | effect receipt mutation | Given valid owner effects in a focused candidate, when one receipt carries a wrong job/count/root, then aggregate verification reverts the outer checkpoint | execution integration through real owner APIs | mutated receipt and full canonical state diff | POC-16 |
| PFS-002-19 | logical-time delay | Given identical jobs/results activated at different valid heights, when both finalize, then semantic outputs match and only declared activation metadata differs | repeatable four-node fresh-devnet fixtures | public record/receipt byte comparison | POC-17 |
| PFS-002-20 | exclusive deadline boundary | Given otherwise valid activation before and at deadline, when block ordering executes, then the former may succeed and the latter observes prior expiry | four nodes with controlled block height | block trace, receipt and FSM records | POC-18 |
| PFS-002-21 | bounded interface/public cap | Given cap-1/cap/cap+1 UDS, CAS and activation fixtures, when they enter their real interfaces, then allowed shapes behave consistently and cap+1 rejects before unbounded work | one local control domain plus four-node public path | decoder/resource assertions and proposer/import/replay parity | POC-20, POC-21 |
| PFS-002-22 | protocol bundle mismatch | Given one supervisor without the active bundle, when handshake/discovery runs, then only local OCOMP readiness refuses and its node continues finality | four nodes; one incompatible supervisor | `ocomp_ready=false`, unchanged consensus readiness/blocks | POC-24 |
| PFS-002-23 | finalized generation replay | Given completed activation, when nodes and compute processes restart/replay, then public outcome selects the same generation/result without CAS authority | four nodes and preserved datadirs | finalized state/receipt/proof reads | POC-25 |
| PFS-002-24 | no on-chain computation trace | Given request and activation blocks, when their complete execution traces are inspected, then no on-chain call reaches Lysis, Fidelity league or Oracle calculation | proposer/import/replay trace on four nodes | registered call-boundary/static trace assertion | POC-03 |

## POC requirement coverage

Every PoC ID appears in at least one normative step or failure flow:

| Requirements | Primary flow evidence |
|---|---|
| POC-01..POC-03 | steps 1–2, 7, 11 and acceptance trace |
| POC-04..POC-06 | steps 3–5; lost-event/orphan/source/CAS flows |
| POC-07..POC-10 | steps 5–9; worker and one-domain flows |
| POC-11..POC-14 | two-domain, sign-once, certificate and result mutation flows |
| POC-15..POC-18 | owner/receipt/delay/deadline flows |
| POC-19..POC-21 | process isolation and cap/public-ingress flows |
| POC-22..POC-26 | steps 12–13; pin, bundle, generation and fork flows |

The authoritative requirement wording remains the `off-chain-poc.md`
verification matrix. This table supplies flow traceability and does not add
product behavior.

## Transaction, checkpoint and finality boundaries

```text
request block execution checkpoint
  budget split + early effect + JobIntent + expiry + OFFCHAIN_PENDING | none
        |
        v finality (job authority begins)
local export/compute/sign journals
  never canonical state; no domain writes
        |
        v public activation transaction
activation outer checkpoint
  evidence + typed verification + every owner receipt + COMPLETED | none
        |
        v activation block finality
public outcome/retention release
```

CE end-block persistence must agree with the execution-sealed roots or fail the
block. Mongo projection may lag and recover; it cannot turn an unfinalized or
invalid transition into authority.

## Conservation and outcome invariants

```text
created Nod count = consumed authenticated Tribute count
sum(Nod gratis_load) + returned remainder = supplied Lysis budget
consumed nominal = sealed authenticated Tribute nominal
contributor records/totals = exact included Lysis population
job COMPLETED <=> exact certified effect receipt set committed
```

Every excluded-from-Intex Tribute still participates in Lysis/Nod conservation
but follows the pinned contributor exclusion rule. Logical Tribute retirement,
active generation and public queries remain consistent across replay/restart.

## Normative protocol versus test harness

The protocol defines actors, messages, states, bindings, ordering and outcomes.
The harness may only orchestrate real production boundaries:

| Harness may | Harness must not |
|---|---|
| create isolated four-validator devnet/genesis | replace validator domains with one calculator |
| issue public transactions and query public APIs | call Lysis/activation handlers directly |
| stop/restart named supervisor/worker processes | stop a node when testing supervisor isolation |
| drop relay/event delivery and wait on finalized cursor | inject a job/result directly into storage |
| corrupt owned temporary Mongo/CAS files through a declared fault control | use Mongo/CAS as canonical outcome evidence |
| use registered owner-call failpoints in focused CORE tests | bypass RPC/txpool/P2P/import for FORK-GATE evidence |
| retain bounded traces/evidence manifests | run on-chain Lysis as a comparison oracle |

An implementation-only hook is permitted for focused fault injection when the
production entrypoint, transaction/checkpoint and assertions remain real and the
substitution is declared. The final thirteen-step demonstration permits no
mocked validator domain, direct state injection or undocumented manual step.

## Observable completion contract

The strongest outcome level is **verified**, not merely submitted/executed:

1. activation transaction is included successfully;
2. its block is finalized;
3. Metadosis/job terminal state and active generation are canonical;
4. request-phase Desis/carry-over and activation Nod/contributor/
   Tribute/carry-over effects are readable through public interfaces;
5. CE roots/proofs and conservation reconcile independently;
6. replay/restart selects the same outcome; and
7. no assertion depends solely on supervisor/CAS/Mongo internals.

Local UDS health/journal and bounded artifact diagnostics are valid evidence for
process/failure requirements, not for canonical product completion.

## Current automation and implementation boundary

Current in-process tests exercise the old synchronous Lysis path and some
rollback/conservation rules. Existing localnet/Mongo features prove encrypted
Tribute projection and CE proofs, not OCOMP. No production `JobIntent`,
supervisor/exporter/worker, control API, CAS manifest, OCOMP key/sign-once,
certificate, activation transaction or certified effect path exists.

Therefore every new OCOMP process/localnet row is currently a Gap under
ADR-B-TST-001. This PFS is ready to drive harness planning; it does not claim
automation or implementation.

## Open questions and technical debt

1. Freeze every section 22 codec/hash/cap/finality/checkpoint decision before
   implementing dependent scenarios.
2. Add stable Gherkin tags for the PFS-002 scenario rows and a machine-readable
   POC-to-scenario/test/CI ledger.
3. Extend the harness with separately owned supervisor/exporter/worker/CAS
   handles and failure controls; do not overload node process handles.
4. Add public read/proof support for every terminal effect needed by step 13.
5. Define production-seam failpoints for one owner failure and one receipt
   mutation without creating a direct success bypass.
6. Produce an independent reference/golden Lysis corpus and exact trace filter
   proving no on-chain Lysis/Fidelity/Oracle calculation.
7. Keep PFS-005/PFS-009 changes outside PoC acceptance but amend them before
   BoundedMVP/supported-network claims.
