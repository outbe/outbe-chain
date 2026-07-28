# PFS-002: Off-chain PoC transforms sealed Tributes into Nods

- **Status:** Implemented on `feat/ocomp-poc`; exact public/E2E/isolation
  closure evidence pending
- **Actors:** Cycle, Metadosis, OCOMP kernel, four validator domains, node
  attestation gates, snapshot exporters, supervisors, workers, CAS stores,
  validator-only result-vote ZeroFee hook, public transaction path, Tribute/CE,
  Fidelity, Oracle, Lysis, NodFactory,
  Intex, Desis, PromisLimit
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

One sealed non-empty WorldwideDay creates one OCOMP intent. Consensus opens its
voting window exactly four blocks after the intent block's finality is recorded.
Four
validator domains independently reconstruct and execute the same Lysis V1
program off-chain. Each domain's `OffchainLysis Supervisor` submits one signed
full-result EVM transaction carrying the constant-size `LysisResultV1`; a
validator-only exact-selector ZeroFee hook
waives its native fee while the OCOMP module alone validates the job and vote.
Consensus stores four bounded digest/signature slots. The third matching
submission records immutable quorum, stores the canonical result once and,
inside the same checkpoint, verifies and atomically installs the exact
Nod/contributor/output roots and commits the
Tribute-retirement/carry-over/Metadosis scalar effects. Desis is already
committed in the GREEN request phase.

The fourth vote remains admissible until the exclusive response deadline after
quorum application. It closes healthy `4/4`, minority, missing-response and
equivocation evidence. No quorum by the deadline means expiry, preserved Lysis
budget, no repeated auction and no Nod. There is no synchronous fallback.

## Acceptance contract

- **Source:** Publicly issued Tributes in one sealed authenticated WWD
  collection.
- **Trigger:** Terminal Metadosis inspection of an eligible non-empty READY day
  under the active PoC protocol bundle.
- **Environment:** Four independently configured validator domains with
  finalizing consensus, separate OCOMP processes/artifacts, valid Oracle and
  Fidelity state, retained CE/Mongo inputs and generated PoC caps. Final
  closure starts from the checked-in four-validator `Final` fixture; only
  loopback ports and a process-local logical clock offset vary per run.
- **Canonical inputs:** Finalized `JobIntentV1`, request block/state root,
  sealed CE/WWD roots, exact count/nominal, frozen Metadosis values,
  budget split, apply preconditions, authenticated Tribute bodies,
  Fidelity/Oracle openings, committee snapshot and `ProtocolBundleHash`.
- **System under test:** Public Tribute/Metadosis path, consensus finality,
  OCOMP control and bulk planes, snapshot export, deterministic Lysis execution,
  node attestation, four public full-result vote transactions, q-forming
  certified domain apply and public outcome reads.
- **Expected response:** One `COMPLETED` job/day, exact terminal receipt and
  active generation; expected Nods, contributor totals, Tribute retirement,
  request-phase Desis brief and carry-over credits; multiple bounded work shards
  when the Tribute population crosses a shard boundary; byte-identical results
  across independent domains and worker schedules.
- **Response measures:** Exact 13-step demonstration passes; all
  `POC-01..POC-26` requirements have evidence at their declared layer; no
  direct state/executor injection or on-chain Lysis trace occurs.
- **Failure guarantee:** Invalid/unavailable local work produces no vote;
  invalid vote/apply changes no protected state; insufficient quorum
  expires and releases the attempt while consensus and the previous active
  state continue. Timely absence and equivocation are canonical evidence, not
  local submitter observations.

## Canonical authority and trust map

| Fact | Authority | Explicitly not authority |
|---|---|---|
| request exists | canonical `JobIntentV1` state | request event |
| request is immutable/final | request block finality proof and state root | local node tip or wall time |
| complete Tribute population | sealed CE/WWD collection root, traversal, count and nominal | Mongo query/page count |
| raw body bytes | canonical body that verifies against its committed leaf | Mongo/CAS location |
| Lysis semantics | job-pinned `ProtocolBundleV1` | worker binary label or negotiation |
| shard/unit membership/order | complete manifest count plus canonical plan and `UnitId` | scheduler/worker completion order |
| one validator vote | one eligible validator index and OCOMP key epoch | process/worker count |
| result equality | exact canonical `ResultDigest` in four bounded vote slots | “equivalent” JSON or submitter choice |
| voting-window authority | `open_height = finality_recorded_height + 4` in consensus state | event, local cursor or vote payload |
| timely participation | canonical vote inclusion height and closed accountability summary | mempool/supervisor logs |
| result-apply authority | q-forming full-result vote plus private `CertifiedLysisActivation` | submitter choice or generic writes |
| output truth | finalized public state/receipts/proofs | supervisor journal or Mongo alone |

Validator-local retention is a bounded multi-job registry. Every exact
candidate/finalized attempt has its own entry and references one separately
addressed authenticated input lease. A retry is a new Job; it reuses a lease
only when the complete input commitments are identical. Old terminal evidence
may remain retained while the retry and unrelated Jobs continue.

## State and artifact model

### Consensus state

```text
Metadosis READY
  -> AWAITING_FINALITY(IntentId, intent_height, pending_nonce)
       -> VOTING_OPEN(JobId, open_height, deadline_height)
            -> COMPLETED(JobId, ResultDigest, LysisTerminalV1)
            -> READY(next nonce) after EXPIRED / CONFLICTED / CANCELED job
```

`RUNNING`, `EXPORTING`, `PLANNED` and worker progress never become consensus
states. `open_height = checked_add(finality_recorded_height, 4)` and
`deadline_height = checked_add(open_height, response_window_blocks)`; votes are accepted
only in `[open_height, deadline_height)`. Terminal result state and the response
window are orthogonal: a `COMPLETED` or `CONFLICTED` job continues accepting
only its missing fourth vote until `deadline_height`, then closes one bounded
accountability summary.

### Parent job and local work shards

One `JobIntentV1`/`JobId` covers the complete authenticated Tribute population
for the WWD. It is not one worker-sized job. The planner deterministically
creates:

```text
N authenticated Tribute
  -> X = ceil(N / max_tributes_per_work_shard) adjacent primary work shards
  -> PlanCommitmentV1(X, primary_work_unit_root)
  -> UnitSpecV1 derived lazily by ordinal and executed by a bounded pool
  -> bounded ResultChunkV1 objects
  -> one typed OutputManifestEntryV1 per chunk
  -> bounded ROOT_REDUCE LEAF(entry + summary) / NODE(summary)
  -> pure LysisProgramV1 finalization over exact verified catalogs
  -> one LysisResultV1(result_chunk_count, result_chunk_list_root)
```

The supervisor queues unit ordinals through bounded cursors; it does not keep
the complete unit vector in memory. Workers derive and consume as many units as
needed. When the first shard is full, the next Tribute becomes the first member
of the next shard; it is not rejected. There is no total Tribute ceiling. All
four validator domains independently derive and execute the complete shard set.
No shard or result chunk has an on-chain FSM, signature weight or independent
apply step.

### Local validator-domain state

```text
DISCOVERED
  -> PINNED_FINALIZED
  -> EXPORTED(InputManifestHash)
  -> PLANNED(PlanCommitmentHash)
  -> EXECUTED(ResultDigest)
  -> ATTESTED(SignOnceSubject)
  -> TERMINAL_OBSERVED
```

This journal is restart/reconciliation evidence, not chain authority.

### Persisted artifacts

| Artifact | Owner/store | Required binding |
|---|---|---|
| bounded Job Registry and authenticated input leases | node asynchronous finality/checkpoint worker | independent candidate/finalized Job entries, exact roots, IntentId/JobId/InputLeaseId, immutable CE lease generation; armed at finality before the live marker advances |
| `JobIntentV1` and job FSM | canonical chain state | pending nonce, intent height, finalized JobId, open/deadline heights, budget split, apply preconditions, bundle |
| canonical input chunks | validator-local CAS | digest, length, manifest membership |
| `InputManifestV1` | exporter/CAS | JobId, checkpoint, input chunk count/root, source roots, count/totals, openings |
| `PlanCommitmentV1`, derived `UnitSpecV1`/artifacts | supervisor/workers/CAS | JobId, manifest, `wwd`, Lysis budget, logical evaluation time, unit count/root, program semantics, UnitId |
| `RootReduceSummaryV1`, `OutputManifestEntryV1` and `ResultChunkV1` catalog | workers/supervisor/CAS | bounded reduction summary, exact semantic-to-transport descriptors, chunk order and verified CAS bytes |
| `LysisResultV1` | pure `LysisProgramV1` finalizer hosted by supervisor | finalized intent/manifest/plan bindings, exact streamed catalogs, typed result and ResultDigest |
| sign-once record | node attestation gate | key epoch, JobId, attempt, purpose, digest |
| Supervisor vote submission journal | validator-local supervisor | prepared/submitted/included/finalized tx identity and reorg rebroadcast |
| `OcompVoteAccountabilityV1` with four slots | canonical chain state | first signed digest, validator/key epoch, inclusion height, optional equivocation and closed matching/missing/divergent/equivocation bitmaps |
| immutable quorum state | canonical chain state | q=3 digest/height/bitmap/evidence |
| q-forming transaction/apply receipt | canonical block data | typed result, stored quorum and owner-effect binding |
| active generation/`LysisTerminalV1` | canonical state | immutable JobId, result/quorum/effect commitments; no mutable accountability fields |

## Preconditions

1. Before any node starts, the disposable devnet generator has produced a base
   genesis and one canonical `Measurement` or `Final`
   `OcompForkInstallV1`. Every node has loaded the same immutable binding to
   the exact chain/genesis, `AtBlock(H)`, request profile, bundle and complete
   result committee.
2. At `H`, the executor's deterministic pre-execution hooks activate the
   protocol-version-1 Update and initialize the owner pre-admission profiles.
   The existing empty-body `OcompLifecycleBegin` then atomically installs the
   complete authority before expiry; no owner mutation is duplicated.
3. Every generated page, work shard, chunk, control message and result
   summary fits its frozen interface cap; total Tribute count is not capped.
4. Four result-validator identities and their separate OCOMP keys are registered;
   `n=4`, `q=3`; their validator EVM identities are eligible for the exact
   result-vote ZeroFee hook.
5. The day is READY, its Tribute collection is sealed and non-empty, and the
   exact current attempt has no live predecessor.
6. Oracle/Fidelity inputs, the budget split and every apply precondition
   can be frozen at the request logical context.
7. Every validator can either retain/reconstruct the finalized input or fail to
   vote; local admission cannot change chain eligibility. A missing canonical
   vote is observable accountability evidence, while monetary slashing policy
   is outside this PoC.

For every shard `j`, `AMOUNT_MAP(j)` reads the matching `FIDELITY_MAP(j)`
artifact for Tribute-to-league observations and the fixed-reduce root for the
global fraction table. Workers authenticate the exact `PlanCommitmentV1` bytes
and all job/manifest bindings before accepting either dependency. The node
attestation gate separately compares the plan's WWD, budget and logical time to
the finalized `JobIntentV1` before signing.

`LateFinalizeCredits` precedes `CycleTick`. Its fee residue is added to carry-over
and cannot form an OCOMP day limit. The Cycle terminal allocation is the sole
`base_limit`; it atomically takes any residue already accumulated before
formation. Residue arriving after formation waits for the next unformed day.

## Normative success protocol and test oracle

| Step | Owner | Message/transition | Durable output | Observable test oracle | PoC requirements |
|---:|---|---|---|---|---|
| 1 | client/Tribute | issue a bounded heterogeneous population that crosses at least one work-shard boundary through public transactions | canonical Tribute/CE state and receipts | successful finalized receipts; public bodies/proofs show every Tribute, including the first record of shard 2, plus leagues/currencies/exclusion fixtures | POC-01, POC-22 |
| 2 | CE/Metadosis | seal WWD; split `day_limit`; GREEN dispatches `auction_base` to Desis or RED credits it to carry-over; create an intent for `lysis_budget` without Lysis | split receipt, `JobIntentV1`, `AWAITING_FINALITY`, request event, tentative pin | finalized diff proves the request-phase effect happened once and shows zero new Nod/contributor/Tribute-consume/unused-Lysis carry-over effect | POC-02, POC-03, POC-23 |
| 3 | consensus/node | finalize request block, derive exact `JobId`/finality proof, bind it to its `InputLeaseId` in the multi-job registry, wait four additional blocks, then atomically install `VOTING_OPEN(open_height = finality_recorded_height + 4, deadline_height)` and its deadline index | finalized Job entry, referenced input lease plus canonical open-window state | all four nodes report the same binding/open/deadline; a retained predecessor and retry coexist; vote before finality or `open_height` rejects in OCOMP state; adversarial proof/overflow vectors reject | POC-04, POC-18 |
| 4 | supervisor | discover by finalized cursor; event may only reduce latency | local `DISCOVERED` journal | dropping the request subscription still discovers exactly one job | POC-05 |
| 5 | node/exporter | open read-only checkpoint lease; full-fold CE; request openings in consecutive owner batches; deterministically bisect any proof response above the bundle-pinned control cap; verify raw bodies/openings; publish manifest/chunks | finalized pin, `InputManifestV1`, CAS objects | every domain independently reconstructs exact root/count/nominal/opening commitments; typed oversize rejection preserves the session and owner order/completeness; one-owner oversize abstains; Mongo/CAS mutation rejects | POC-06, POC-09, POC-20 |
| 6 | supervisor/planner | partition the complete manifest into adjacent bounded work shards; commit count/root and derive `UnitSpecV1`/`UnitId` lazily by ordinal | `PlanCommitmentV1`, bounded queue cursor and unit artifacts | shard capacity + 1 creates the next shard; 10,000 and 1,000,000,000 counts produce exact unit counts without proportional plan allocation; frozen bytes/hashes match golden vectors | POC-07 |
| 7 | workers/reducer | execute immutable units from the bounded local queue, retry freely, compute one checked nominal subtotal per finalized-output shard including excluded Tribute, stage one bounded result chunk per real leaf, expose its typed manifest entry only through the leaf artifact and reduce summaries through bounded NODE payloads | unit artifacts, exact `OutputManifestEntryV1`/`ResultChunkV1` catalog and final reduction summary | missing any shard/entry/chunk cannot close reduction; forged or overflowing shard subtotal and wrong descriptor/hash/bytes reject before leaf VERIFIED; reduced nominal must equal the authenticated manifest total; 1/2/4 workers and randomized completion/retry yield byte-identical artifacts/summary; reference corpus matches | POC-01, POC-08 |
| 8 | supervisor-hosted typed finalizer, then node attestation gate | after durable complete admission and `VOTING_OPEN`, stream exact plan/manifest-entry/chunk catalogs through pure `LysisProgramV1` finalization; derive completion fields from finalized intent and the LYSIS_V1 zero-count/canonical-empty pre-result semantic-event commitment; node independently reloads finalized job/export authority, verifies the constant-size candidate and durably signs one exact digest | one `LysisResultV1`, sign-once journal plus one signature/domain | missing/duplicate/reordered/substituted artifact, entry or chunk causes no vote; descriptor and semantic roots must close over the same bytes; completion-field or semantic-event count/root mutation rejects; no caller-provided root is accepted; pre-open signing rejects; exact retry returns the same signature and a second digest refuses after restart | POC-08, POC-12 |
| 9 | each validator-domain Supervisor | wrap its node-attested full `LysisResultV1` in `ResultVoteV1`, read the canonical `latest` account nonce, use the frozen gas envelope, submit the exact-selector validator ZeroFee EVM transaction through RPC/txpool/P2P/proposer/import/replay and track inclusion/finality/reorg | Supervisor single-writer submission journal plus exact signed transaction bytes and one canonical digest/signature/height slot per domain | no validator native-fee debit, `eth_estimateGas` or pending-block execution; OCOMP—not ZeroFee—decodes the result and rejects pre-open/wrong/late/invalid votes; orphaned inclusion rebroadcasts the same bytes without double-counting; conflicting signed result records equivocation without replacing the first | POC-10, POC-13, POC-18, POC-19 |
| 10 | q-forming result-vote handler | when the current full-result submission creates three matching slots, store quorum and one canonical result and verify finalized job/bundle/result/preconditions without executing Lysis | immutable q=3 evidence and private `CertifiedLysisActivation` in the current execution frame | first/second vote has no owner effects; third matching vote enters apply; result-vote cap-1/cap succeeds and cap+1 rejects consistently; one-byte/JobId/root/count/completion mutation rejects before a slot or owner write | POC-03, POC-14, POC-18, POC-20, POC-21 |
| 11 | certified domain owners | in the same q-forming checkpoint, install certified Nod/contributor/output roots, retire the sealed Tribute generation, apply carry-over/Metadosis scalars and verify constant-size receipts; APPLIED hashes four owner events in fixed order, conflict hashes an empty apply-event payload | `COMPLETED` or defined `CONFLICTED`, active generation and terminal/effect receipts | no `N`-action on-chain loop; owner failure or receipt mutation rolls back the q-forming slot, quorum and every owner effect; expected conflict commits quorum plus zero owner effects; Desis is untouched | POC-15, POC-16, POC-17, POC-25 |
| 12 | remaining validator/consensus | accept the fourth full-result vote until the exclusive deadline and then close accountability | matching/minority/missing/equivocation summary separate from terminal result | healthy run records 4/4; one-domain-unavailable run retains completed q=3 plus one missing bit; fourth vote cannot change terminal state | POC-10, POC-18, POC-19 |
| 13 | consensus/client | finalize the q-forming block and verify outputs through public interfaces | finalized state, receipts, CE roots/proofs | every expected effect and conservation equation verifies; old Tribute partition is logically retired; no supervisor/CAS read is used as outcome authority | POC-22 |

## Exact PoC acceptance choreography

The PoC is complete only when this exact story passes from public Tribute
issuance through public Nod reads on a four-validator devnet:

1. issue a bounded WWD with at least `max_tributes_per_work_shard + 1` Tribute, different
   Fidelity leagues, currencies and at least one
   `exclude_from_intex_issuance` Tribute;
2. seal the WWD and reach terminal Metadosis;
3. inspect the finalized split and `JobIntent`; prove the request-phase effect
   happened once and there are zero new Nod/contributor/Tribute-consume effects;
4. in the healthy workflow run all four domains, include all four full-result
   votes, prove that the third matching vote atomically completed the job and
   that the fourth changed only accountability, finalize and verify the public
   result;
5. start a separately initialized workflow with a fresh WWD and `JobIntent`,
   repeat the request-only checks from steps 1–3, then stop one validator’s
   supervisor before execution; show the other three domains independently
   rebuild that workflow's input root, derive the same multi-shard plan and
   include three matching `ResultVoteV1` transactions;
6. observe the third matching `ResultVoteV1` atomically record q=3 and complete
   the typed apply; keep the fourth vote slot open until the response deadline
   and close its missing bit;
7. finalize the degraded workflow's q-forming block and query every expected Nod,
   contributor total, Metadosis state, request-phase Desis brief, carry-over
   credit and retired Tribute partition;
8. compare the result with an offline reference/golden corpus, never an on-chain
   Lysis execution;
9. repeat with 1, 2 and 4 workers and randomized completion order;
10. request a second digest signature for the same job and observe sign-once
    refusal; exercise exact vote retry, wrong signer/key epoch, late vote and a
    conflicting signed vote; prove first-vote immutability and canonical
    equivocation evidence;
11. delay otherwise identical q-forming votes by different block counts and prove
    byte-identical Nod/contributor/Tribute/carry-over results with no repeated
    request effect;
12. run another job with two validators unavailable and observe response-window expiry,
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
| PFS-002-03 | **RETIRED:** zero-limit compatibility branch | The former scenario required a zero-Lysis-limit READY day, but production Cycle forms the day limit before Metadosis and a WWD leaving FORMING resolves to GREEN or RED; no replacement scenario may manufacture this state | none; historical identity only | machine-readable `RETIRED` tombstone; no runtime evidence | none |
| PFS-002-04 | authenticated totals/body mismatch | Given one omitted/changed Mongo body or opening, when exporter reconstructs the job input, then root/count/nominal verification fails and no signature exists | one validator domain plus real Mongo/CE checkpoint; repeat in four-domain story | exporter typed failure and absent sign-once record | POC-06 |
| PFS-002-05 | duplicate owner/day identity | Given one canonical Tribute identity, when a duplicate is submitted before sealing, then admission rejects and no duplicate enters the later manifest/result | four nodes, public Tribute path and Mongo/CE proof | reverted receipt, unchanged indexes/bodies/proofs | POC-26; imported PFS-001 admission evidence |
| PFS-002-06 | certified owner failure | Given a valid q-forming full-result vote, when one later Nod/effect owner fails, then the third slot/quorum/job/domain effects all roll back | four nodes and public vote path; focused production-seam failpoint | complete canonical pre/post state and receipt trace | POC-15 |
| PFS-002-07 | CE persistence/replay recovery | Given quorum apply reaches CE persistence, when persistence/restart is faulted, then recovery exposes either the full certified roots/outcome or pre-state, never a partial generation | four nodes with real Reth/CE MDBX restart | **DEFERRED to BoundedMVP:** historical scenario ID; POC-25 requires normal finalized replay, not this crash matrix | none |
| PFS-002-08 | long timestamp/backlog compatibility | Given multiple due days/slots across a timestamp jump, when block lifecycle catches up, then canonical ordering processes each eligible request once and preserves non-PoC branches | four nodes; bounded generated backlog | **DEFERRED:** historical scenario ID; backlog policy remains explicit debt | none |
| PFS-002-09 | lost request event | Given a finalized request and dropped subscription event, when supervisor resumes its finalized cursor, then it discovers the exact job once | one real validator domain, repeated across four; UDS | cursor/journal plus canonical job read | POC-05 |
| PFS-002-10 | orphaned request and tentative pin | Given a tentative pin/local prework, when persisted consensus finality identifies a different block at the candidate height, then work is non-signable and the pin releases; restart cannot restore authority | deterministic consensus-boundary fixture driving the production node pin coordinator, durable journal and attestation gate | canonical hash mismatch, released journal record, typed attestation refusal before and after restart | POC-23 |
| PFS-002-11 | CAS corruption and TOCTOU | Given an exported manifest, when a chunk is truncated/reordered/changed before or during worker consumption, then stream digest/membership fails or the chunk is rebuilt and no bad result is signed | one real exporter/worker/CAS domain | expected/actual digest and unchanged sign journal | POC-06, POC-20 |
| PFS-002-12 | deterministic multi-shard worker schedules | Given one exact parent job containing `max_tributes_per_work_shard + 1` Tribute, when its complete shard/unit DAG runs with 1, 2 and 4 workers plus random kills/retries/order, then the last Tribute is in shard 2 and plan/result bytes remain identical | one domain is sufficient for schedule variants; compare all four domains | golden shard/range/plan/unit/result hashes and exact coverage bitmap | POC-07, POC-08 |
| PFS-002-13 | one validator domain unavailable | Given one stopped supervisor and four live nodes, when remaining domains execute and submit full-result votes, then the third matching on-chain submission atomically establishes quorum and applies the result while finality continues; deadline close records one missing validator | four nodes, three compute domains | process health, vote slots/quorum/accountability summary and finalized output | POC-09, POC-10, POC-18, POC-19 |
| PFS-002-14 | two validator domains unavailable | Given two stopped supervisors, when the response deadline arrives, then begin-zone close finds no quorum, expires the attempt with the same Lysis budget, no repeated auction and no Nod | four nodes, two compute domains | finalized FSM/budget/vote/accountability state diff | POC-11, POC-18, POC-19 |
| PFS-002-15 | sign-once equivocation/restart | Given one durable signature subject, when a different digest is requested before and after node restart, then the gate refuses and retains the first binding | one real node attestation gate and durable journal | signature/journal bytes and typed refusal | POC-12 |
| PFS-002-16 | result-vote and equivocation rules | Given a live job, when exact duplicate, wrong-epoch/unknown/replaced signer, late vote and a second conflicting signed digest are submitted, then only the first eligible timely vote counts; invalid/late cases leave the slot unchanged and the conflicting valid vote records bounded equivocation evidence | four nodes and public vote path | vote receipts, slot/tally state and canonical pre/post equality | POC-13 |
| PFS-002-17 | result/job/order mutation | Given a full-result vote, when one result byte, `JobId`, order, count or root changes, then digest/binding verification rejects atomically before slot/application | four nodes and public vote path | failed receipt; live job remains unchanged | POC-14 |
| PFS-002-18 | effect receipt mutation | Given valid owner effects in a focused candidate, when one receipt carries a wrong job/count/root, then aggregate verification reverts the outer checkpoint | execution integration through real owner APIs | mutated receipt and full canonical state diff | POC-16 |
| PFS-002-19 | logical-time delay | Given identical jobs/results whose q-forming votes land at different valid heights, when both finalize, then semantic outputs match and only declared apply metadata differs | repeatable four-node fresh-devnet fixtures | public record/receipt byte comparison | POC-17 |
| PFS-002-20 | response deadline and terminal accountability | Given votes before and at the deadline, when block ordering executes, then only pre-deadline votes fill slots; no-quorum closes to EXPIRED, while pre-deadline q=3 was already applied by its q-forming vote and the fourth slot remains accountability-only | four nodes with controlled block height | block trace, vote/accountability receipts and FSM records | POC-18 |
| PFS-002-21 | bounded interface/public cap | Given worker-shard-cap+1 and full-result-vote cap-1/cap/cap+1 fixtures for both non-q and q-forming paths, when they enter their real interfaces, then shard-cap+1 creates another canonical shard without loss while only an oversized transaction/chunk/control frame rejects before unbounded work | one local control domain plus four-node public path | exact shard coverage, decoder/resource assertions and proposer/import/replay parity | POC-20, POC-21 |
| PFS-002-22 | protocol bundle mismatch | Given one supervisor without the active bundle, when handshake/discovery runs, then only local OCOMP readiness refuses and its node continues finality | four nodes; one incompatible supervisor | `ocomp_ready=false`, unchanged consensus readiness/blocks | POC-24 |
| PFS-002-23 | finalized generation replay | Given a completed quorum apply, when nodes and compute processes restart/replay, then public outcome selects the same generation/result without CAS authority | four nodes and preserved datadirs | finalized state/receipt/proof reads | POC-25 |
| PFS-002-24 | no on-chain computation trace | Given request and q-forming blocks, when their complete execution traces are inspected, then no on-chain call reaches Lysis, Fidelity league or Oracle calculation | proposer/import/replay trace on four nodes | registered call-boundary/static trace assertion | POC-03 |
| PFS-002-25 | population-independent planning | Given synthetic manifests for 10,000 and 1,000,000,000 Tribute, when the planner commits the job, then it derives exactly `ceil(N/S)` primary work units through bounded cursors without allocating a vector proportional to `N` | one supervisor/planner process; no billion-record body fixture required | exact count derivation, bounded-allocation instrumentation and deterministic sampled/boundary `UnitSpecV1` vectors | POC-07, POC-20 |

## POC requirement coverage

Every PoC ID appears in at least one normative step or failure flow:

| Requirements | Primary flow evidence |
|---|---|
| POC-01..POC-03 | steps 1–2, 7, 11 and acceptance trace |
| POC-04..POC-06 | steps 3–5; lost-event/orphan/source/CAS flows |
| POC-07..POC-10 | steps 5–9; worker and one-domain flows |
| POC-11..POC-14 | two-domain, sign-once, result-vote/equivocation and result mutation flows |
| POC-15..POC-18 | owner/receipt/delay/deadline flows |
| POC-19..POC-21 | process isolation and cap/public-ingress flows |
| POC-22..POC-26 | steps 12–13; pin, bundle, generation and fork flows |

The authoritative requirement wording remains the `off-chain-poc.md`
verification matrix. This table supplies flow traceability and does not add
product behavior.

## Transaction, checkpoint and finality boundaries

```text
request block execution checkpoint
  budget split + early effect + JobIntent + AWAITING_FINALITY | none
        |
        v finality recorded + four additional blocks
voting-open checkpoint
  finalized JobId + open/deadline heights + deadline index | none
        |
local export/compute/sign/submit journals
  never canonical state; no domain writes; Supervisor submits vote tx
        |
        v four public full-result vote transactions
vote/apply checkpoints
  first two slots: bounded accountability only
  q-forming slot: immutable q=3 + one stored LysisResultV1
    + typed verification + every owner receipt
    + immutable LysisTerminalV1 + COMPLETED | none
        |
        v q-forming block finality
separate accountability record remains open until deadline
  fourth slot / first conflict / closed summary only
  never changes terminal receipt, active generation or exact retry
        |
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
but follows the pinned contributor exclusion rule. Each bounded
`OUTPUT_FINALIZE` run commits one checked subtotal over all its Tribute;
`ROOT_REDUCE` checked-adds those subtotals and the finalizer requires exact
equality with the sealed manifest total. Logical Tribute retirement, active
generation and public queries remain consistent across replay/restart.

## Normative protocol versus test harness

The protocol defines actors, messages, states, bindings, ordering and outcomes.
The harness may only orchestrate real production boundaries:

| Harness may | Harness must not |
|---|---|
| create isolated four-validator devnet/genesis | replace validator domains with one calculator |
| issue public transactions and query public APIs | call Lysis/quorum-apply handlers directly |
| stop/restart named supervisor/worker processes | stop a node when testing supervisor isolation |
| drop event delivery and wait on finalized cursor | inject a job/result/vote directly into storage |
| corrupt owned temporary Mongo/CAS files through a declared fault control | use Mongo/CAS as canonical outcome evidence |
| use registered owner-call failpoints in focused CORE tests | bypass RPC/txpool/P2P/import for FORK-GATE evidence |
| retain bounded traces/evidence manifests | run on-chain Lysis as a comparison oracle |

An implementation-only hook is permitted for focused fault injection when the
production entrypoint, transaction/checkpoint and assertions remain real and the
substitution is declared. The final thirteen-step demonstration permits no
mocked validator domain, direct state injection or undocumented manual step.

## Observable completion contract

The strongest outcome level is **verified**, not merely submitted/executed:

1. three matching full-result vote transactions are included and the third
   atomically records quorum, applies the typed result and exposes `COMPLETED`;
2. the q-forming block is finalized;
3. no separate activation transaction exists;
4. Metadosis/job terminal state and active generation are canonical;
5. request-phase Desis/carry-over and quorum-apply Nod/contributor/
   Tribute/carry-over effects are readable through public interfaces;
6. CE roots/proofs and conservation reconcile independently;
7. the separate fourth-slot/accountability summary is canonical after deadline
   while terminal receipt, active generation and exact-retry identity are
   unchanged;
8. replay/restart selects the same outcome; and
9. no assertion depends solely on supervisor/CAS/Mongo internals.

Local UDS health/journal and bounded artifact diagnostics are valid evidence for
process/failure requirements, not for canonical product completion.

## Current automation and implementation boundary

The feature branch implements the direct Supervisor-to-chain ZeroFee
`ResultVoteV1` path, four-block-after-finality voting-open transition, four
compact first-vote slots, separate accountability and q-forming atomic apply.
The relay, digest-only vote and separate public activation paths are absent.

Unit and integration coverage exists for protocol bytes, finality binding,
sign-once submission, q=3 apply/rollback, fourth-slot accountability, ZeroFee,
txpool and direct-injection prevention. This PFS is not yet a release claim:
the exact four-domain public/E2E/isolation run and hash-indexed closure bundle
remain mandatory under ADR-B-TST-001.

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
7. Add a separate monetary-slashing ADR before penalties are enabled; PFS-002
   proves only canonical missing-response/equivocation evidence.
8. Keep PFS-005/PFS-009 changes outside PoC acceptance but amend them before
   BoundedMVP/supported-network claims.
