# Citadel audit: off-chain computation (PoC, PoC to MVP, MVP)

> Historical audit note: reservation/`RESULT_ACCEPTED`/Desis-in-activation text
> below describes the pinned pre-remediation SHA. The current normative PoC uses
> full-result validator votes, q-forming atomic apply, request-phase
> `auction_base`, read-only apply preconditions and `unused_lysis` carry-over.
> It has no relay, public activator or durable `QUORUM_READY`, creates no owner
> reservation records and never calls Desis during apply. It also predates the
> unbounded-parent amendment: references below to `BoundedLysisResultV1`,
> `ActionStreamV1`, complete-job caps or action bytes in the activation
> transaction are findings about that historical SHA, not current protocol
> requirements. Current normative types are bounded `ResultChunkV1` plus
> constant-size `LysisResultV1`/root apply, with no total Tribute cap.

## Verdict

- Status: **NOT CITADEL**
- Confidence: **high**
- Scope/trust/atomicity assumptions: this audit covers
  `off-chain-computation.md` at SHA-256
  `ef50698111aca5b665dcea091d89d63c67378bebfe19139e1d3896ab662e8705`,
  the current Metadosis/Lysis/NodFactory execution path, Outbe's protocol-update
  machinery and the repository's canonical README/ADR contracts. It grades both
  design completeness and the reachable current production interface. It does
  not claim that the proposed OCOMP modules already exist.
- Rationale: the document has a coherent PoC story and preserves the intended
  process, finality, deterministic execution and activation seams. However, the
  proposal is not yet a complete PoC-to-MVP protocol contract, while the current
  reachable implementation still executes Lysis synchronously. The missing
  normative owner, closed activation authority, in-flight version rules,
  mixed-version readiness gate, schema migration, exact bounded-result block
  envelope and production-interface evidence create reachable upgrade halt,
  incompatible interpretation and authority-bypass risks. Any applicable
  `FAIL` makes the overall result `NOT CITADEL`.

The three audit objects are graded separately:

| Object | Status | Reason |
|---|---|---|
| PoC design | PARTIAL | The vertical slice is coherent, but result-size closure, cross-block result handoff, time semantics, activation authority, typed downstream receipts and executable acceptance evidence are incomplete. |
| PoC to MVP transition | FAIL | There is no complete upgrade FSM, immutable per-intent compatibility tuple, mixed-version readiness gate, old-job drain policy or rollback/forward-fix boundary. |
| MVP design | PARTIAL | Most production concerns are named, but several are requirements rather than protocol definitions with measurable gates and implementation evidence. |

## Sources of truth and scope

Repository precedence and inspected sources:

1. `README.md`: single Reth/Commonware node binary, scheduled protocol activation,
   startup version ceiling, block-artifact constraints and canonical-state
   authority.
2. `CONTEXT.md`: architecture spaces and protocol-flow specification ownership.
3. `docs/adr/README.md` and `docs/adr/index.md`: ADR/PFS precedence and G1-G10
   review rules.
4. `docs/adr/system/ADR-S-GOV-003-scheduled-protocol-update-activation.md`.
5. `docs/adr/system/ADR-S-KEY-001-validator-key-generation-and-secret-custody.md`.
6. `docs/adr/core/ADR-C-LYS-001-lysis-tribute-to-nod-transformation.md`.
7. `docs/adr/core/ADR-C-MET-001-metadosis-worldwide-day-fsm.md`.
8. `docs/adr/core/ADR-C-NOD-002-nod-issuance-and-gratis-mining-orchestration.md`.
9. `docs/flows/002-off-chain-poc-protocol-flow.md` and
   `docs/flows/009-multichain-auction-day.md`.
10. `docs/adr/blockchain/ADR-B-WIR-001-protocol-identifiers-and-consensus-wire-contract.md`.
11. `docs/adr/blockchain/ADR-B-EVM-001-block-lifecycle-and-system-transactions.md`.
12. `docs/adr/blockchain/ADR-B-EVM-005-stateful-precompile-runtime-framework.md`.
13. `docs/adr/blockchain/ADR-B-CAP-001-resource-metering-and-capacity-closure.md`.
14. `docs/adr/blockchain/ADR-B-SUP-001-supervision-failure-taxonomy-readiness-and-observability.md`.
15. `docs/adr/blockchain/ADR-B-TST-001-production-verification-and-evidence-architecture.md`.
16. `docs/adr/blockchain/ADR-B-RLS-001-reproducible-build-supply-chain-and-release-provenance.md`.
17. `docs/adr/blockchain/ADR-B-OCD-014-cross-store-crash-restart-reconciliation.md`.
18. `docs/adr/blockchain/ADR-B-OCD-015-authenticated-snapshot-bootstrap-and-state-recovery.md`.

Current-code evidence was obtained through CodeGraph from:

- `crates/core/metadosis/src/runtime.rs:378-483`;
- `crates/core/lysis/src/runtime.rs:31-165`;
- `crates/core/nodfactory/src/api.rs:10-17`;
- `crates/core/nodfactory/src/runtime.rs:25-86`;
- `crates/system/update/src/runtime.rs:82-180`;
- `crates/system/update/src/handlers.rs:14-47`;
- `crates/system/update/src/tests/handlers.rs:85-196`.

Relevant primary precedents:

- [Cosmos SDK x/upgrade](https://docs.cosmos.network/sdk/latest/modules/upgrade/README):
  binaries install an upgrade handler before the scheduled height; store
  migrations and handler identity are explicit.
- [Cosmos ADR-041](https://docs.cosmos.network/sdk/latest/reference/architecture/adr-041-in-place-store-migrations):
  module consensus versions and ordered migrations are persisted.
- [Ethereum Engine API common definitions](https://github.com/ethereum/execution-apis/blob/main/src/engine/common.md):
  methods and structures are independently versioned and peers exchange exact
  supported capability names.
- [Hyperledger Fabric chaincode lifecycle](https://hyperledger-fabric.readthedocs.io/en/latest/chaincode_lifecycle.html):
  code/definition installation, readiness approval, sequence and commit are
  separate lifecycle steps.
- [Kubernetes version-skew policy](https://kubernetes.io/releases/version-skew-policy/):
  supported skew and upgrade order are explicit compatibility contracts.

## Mutation interface and call graph

Current reachable production path:

```text
block lifecycle
  -> Metadosis::process_metadosis
     -> calculate_metadosis
     -> outbe_lysis::runtime::lysis
        -> load all day Tribute bodies
        -> Fidelity::league for each Tribute
        -> compute allocation
        -> NodFactory::issue_nod for each Tribute
        -> Intex::record_contributors
        -> Tribute::consume_lysis_partition
     -> Desis::dispatch_auction_brief -> bool
     -> PromisLimit::add_to_total_unallocated
     -> mark WWD COMPLETED
     -> retire Tribute partition
```

The current `lysis` function owns a storage checkpoint, but
`nodfactory::api::issue_nod` is a public Rust mutation seam whose authority is
conventional. Current Metadosis has no `OFFCHAIN_PENDING`, result certificate or
activation command.

Proposed seam:

```text
terminal request command
  -> JobIntent + reservations + OFFCHAIN_PENDING + expiry index

finalized local control plane
  -> immutable checkpoint/export
  -> execute_lysis
  -> validator-local sign-once gate

ordinary evidence command
  -> validate certificate + bind finalized JobId + RESULT_ACCEPTED

bounded begin-zone activation command
  -> validate immutable intent/profile tuple
  -> sealed CertifiedLysisActivation capability
  -> Nod/contributor/Tribute/Desis/Promis/Metadosis effects
  -> COMPLETED
```

This is the correct target shape, but the proposal must close who can construct
the activation capability and how each downstream module consumes a typed,
non-ignorable receipt.

## Side-effect ledger

| Effect | Owner | Atomicity domain | Commit/rollback/compensation point | Receipt | Retry/idempotency |
|---|---|---|---|---|---|
| Create intent, reserve targets, insert expiry, move WWD | proposed OCOMP/Metadosis command | one executor checkpoint | request block commit or full block rollback | `IntentCreated` or typed `Deferred` | `(WWD, pending_nonce)` and `IntentId` bind retry |
| Finalized snapshot pin | node-local retention coordinator | local durable journal, not EVM transaction | write-before-prune; reconcile from finalized state | versioned pin record | state-derived reconstruction, conservative floor |
| Snapshot export and CAS writes | exporter | local page journal + immutable CAS objects | source certificate after exact root rebuild | typed export certificate | page cursor and content digest |
| Candidate signing | node attestation gate | durable sign-once journal before key operation | journal commit precedes signature release | signed result or typed refusal | one `(key_epoch, JobId, attempt)` digest |
| Evidence acceptance | proposed OCOMP command | EVM transaction checkpoint | record/binding/index all commit or revert | typed evidence receipt | identical evidence is idempotent; conflict rejects |
| Result payload handoff from evidence block to activation block | proposed OCOMP/executor system transaction | canonical block-body witness plus small consensus locator | evidence body commits bytes/hash; next begin-zone validates exact carried bytes before applying | `PendingActivationPayloadV1` receipt | exact locator/hash replay; terminal cleanup is idempotent |
| Nod creation | currently public NodFactory API; proposed activation capability | activation executor checkpoint + CE overlay | block rollback must cover all Nod bodies/events | **missing proposed typed batch receipt** | duplicate Nod IDs reject; command-level replay must return terminal receipt |
| Contributor recording | Intex | activation checkpoint | same block rollback | **not yet specified** | exact generation/series binding required |
| Tribute consumption/logical retirement | Tribute/CE | activation checkpoint; physical GC is local later | catalog pointer/FSM commit together; GC cursor separate | retirement receipt required | logical transition once; GC restart-safe |
| Desis brief | Desis | activation checkpoint | current API returns `bool`; proposed path must consume a typed receipt | **current bool is ambiguous** | one reserved brief identity |
| Promis delta | PromisLimit | activation checkpoint | exact conservation check before commit | typed delta/conservation receipt required | one job-bound delta |
| Metadosis completion/event | Metadosis | activation checkpoint | only after all effects validate | terminal activation receipt | terminal replay returns same receipt |
| Profile/schema activation | Update module handler | begin-block checkpoint | handler, active version and status commit atomically | `UpgradeActivated` event | handler replays only if prior activation did not commit |

## Observed FSM

Current production FSM:

| Current | Event | Guard | Effects | Next/error | Rollback owner |
|---|---|---|---|---|---|
| READY | current Metadosis tick | valid non-empty day | synchronous Lysis plus all downstream effects | COMPLETED or block error | current Lysis checkpoint plus outer block |
| READY | current empty day | valid day, zero Tribute | Desis/Promis/completion/retirement | COMPLETED | outer block |

Proposed job FSM:

| Current | Event | Guard | Effects | Next/error | Rollback owner |
|---|---|---|---|---|---|
| READY | request | admission/profile ready and exact nonce | reserve, intent, expiry, event | OFFCHAIN_PENDING | request command checkpoint |
| OFFCHAIN_PENDING | accept evidence | finalized binding, before deadline, matching immutable tuple | store result/certificate, activation due entry | RESULT_ACCEPTED | evidence transaction |
| RESULT_ACCEPTED | activate | canonical due order, CAS/reservations valid | all typed domain effects | COMPLETED | activation command checkpoint |
| OFFCHAIN_PENDING or RESULT_ACCEPTED | deadline | exact intent/nonce and no successful activation | archive, release, requeue | EXPIRED plus READY(new nonce) | expiry command checkpoint |
| RESULT_ACCEPTED | activation conflict | stale reservation/CAS | archive, release, requeue | CONFLICTED plus READY(new nonce) | activation command checkpoint |
| terminal | duplicate same intent | terminal receipt matches | no new effects | same terminal state/receipt | none |
| any | malformed persisted tag | strict decoder | no effects | fatal corruption | block/recovery coordinator |

Required, but absent, upgrade FSM:

| Current | Event | Guard | Effects | Next/error | Rollback owner |
|---|---|---|---|---|---|
| DRAFT | build | immutable source/profile/schema manifest | candidate artifacts | BUILT | release pipeline |
| BUILT | validator preflight | exact binary/capability/key/storage/capacity identity | signed readiness for one activation plan | READY_TO_SCHEDULE | readiness registry |
| READY_TO_SCHEDULE | governance schedule | required readiness quorum and cancellation window | activation plan | SCHEDULED | governance/update module |
| SCHEDULED | cancel before height | governed cancellation passes | remove plan, keep old profile | CANCELED | update module |
| SCHEDULED | reach height | binary ceiling, handler and state preflight pass | atomic schema/profile activation | ACTIVE_NEW | update handler checkpoint |
| ACTIVE_NEW | old intent finishes | intent pins old interpreter and old committee | terminal receipt under old rules | DRAINING_OLD | job command |
| DRAINING_OLD | last old intent terminal and retention expires | no live old references | retire old admission; preserve historical replay decoder | RETIRED_FOR_ADMISSION | governed cleanup |
| ACTIVE_NEW | defect after activation | downgrade forbidden | schedule monotonic repair version | FORWARD_FIX_PENDING | new update handler |

## Citadel gates

| Gate | Status | Evidence | Gap/closure |
|---|---|---|---|
| G1 — Deep, closed interface | FAIL | Proposed `apply_certified_lysis` is typed, but current `NodFactory::issue_nod` is callable without an unforgeable Lysis/activation capability. | Seal activation construction inside the executor and require typed capabilities/receipts at every downstream mutation. |
| G2 — Valid state model | PARTIAL | Job/expiry/reservation fields are described; strict persisted decoders and cross-index equivalences are not specified as one schema contract. | Define versioned records, closed enums and record/index/reservation equivalences with corruption behavior. |
| G3 — Explicit FSM | FAIL | Job FSM exists; transition FSM does not. Current canonical Metadosis FSM has no `OFFCHAIN_PENDING`. | Add OCOMP job FSM and upgrade FSM to canonical ADR/PFS, including every illegal transition and progress path. |
| G4 — Atomicity domains | PARTIAL | Request/evidence/activation checkpoints are intended; node-local pin, CAS, signer and GC correctly remain separate journals. | Specify typed handoff/outbox identities, crash points and proof that block rollback covers every proposed domain effect. |
| G5 — Explicit effects and receipts | FAIL | Current Desis orchestration returns `bool`; proposed activation lists writes but not consumed batch receipts and exact conservation proof. | Replace ambiguous effects with job-bound typed receipts; define all-or-none conservation across Nod/Contributor/Tribute/Desis/Promis. |
| G6 — Deterministic, bounded execution | FAIL | Deterministic planner and caps are strong, but `512 KiB` is a proposal without a worst-case encoded/gas/block-body proof. | Derive the cap from one versioned capacity profile and maximum-shape production benchmark before fork activation. |
| G7 — Single-source invariants | PARTIAL | Intent/finalized roots and active generation are identified as authorities. | Define exact authoritative owner and equivalence for job, expiry, activation, reservations, WWD state and generation records. |
| G8 — Replay, retry, reentrancy, concurrency | PARTIAL | `IntentId`, `JobId`, sign-once and content addressing cover core duplicates. | Pin every interpreter/committee/key/profile field in the intent; define old-job drain, reorg, nested activation exclusion and local journal generations. |
| G9 — Production-interface evidence | FAIL | Section 1.6 is a credible test story, but none of the new production interfaces exists or has evidence. Repository verification ADR also records mixed-version gaps. | Implement the ledger and run public Tribute-to-Nod, fault, capacity, mixed-version, replay and migration tests through released binaries. |
| G10 — Migration and project contract | FAIL | Main document is an architecture proposal; canonical Metadosis/Lysis/Nod/Update ADRs and a PFS do not yet own the new protocol. | Accept ADR/PFS changes, assign protocol/schema versions, implement atomic migration and release/operator procedures. |

## Project-contract axis

| Rule/source | Status | Evidence | Required action |
|---|---|---|---|
| Root README precedence | PARTIAL | Proposal acknowledges current code and fork activation. | State that no OCOMP profile can activate until canonical ADR/PFS and root contract changes are accepted. |
| ADR-S-GOV-003 | FAIL | Current update code checks only binary ceiling at activation and runs registered handlers; it has no distributed readiness quorum or recovery when a handler never succeeds. | Add governed OCOMP activation manifest, preflight readiness evidence, cancellation window and forward-fix runbook. |
| ADR-C-MET-001 | FAIL | Canonical FSM does not contain `OFFCHAIN_PENDING`; current code calls Lysis synchronously. | Amend ADR and FSM under the same fork. |
| ADR-C-LYS-001 | PARTIAL | Proposed pure-execute/typed-apply seam matches direction. | Specify exact compatibility oracle and effect receipt contract. |
| ADR-C-NOD-002 | FAIL | Current `issue_nod` authority is conventional/public. | Introduce a sealed certified-activation capability and close raw bypasses. |
| PFS-002 and PFS-009 | FAIL | Both Draft flows describe the synchronous Lysis path and do not contain request/finality/evidence/activation boundaries. | Replace the relevant sequence with the OCOMP split, keep exact conservation and add live finality/recovery evidence. |
| ADR-B-WIR-001 | PARTIAL | Proposal versions several objects, but not one immutable compatibility tuple or generated registry. | Register all identifiers, codecs, tags, limits and retired versions in the protocol manifest. |
| ADR-B-CAP-001 | FAIL | Proposed caps are not derived from a measured block bill; `512 KiB` is not closed against gas/body/finality. | Generate maximum bytes/work before allocation and publish benchmark/headroom evidence. |
| ADR-B-SUP-001 | PARTIAL | Separate services and nonfatal OCOMP readiness are sound. | Register every node-local OCOMP task with bounded restart/join and typed failure codes. |
| ADR-B-TST-001 | FAIL | No VerificationLedger entries or production tests exist for OCOMP. | Add stable requirement IDs and exact CI/release evidence. |
| ADR-B-RLS-001 | PARTIAL | Pinned program/image is proposed. | Bind the exact OCOMP binaries, reference implementation, schemas and worker image to ReleaseManifest and NetworkManifest. |
| ADR-B-OCD-014/015 | PARTIAL | Pin/export/recovery concepts are present. | Add OCOMP local stores to the recovery vector and authenticated snapshot/restore profile; preserve historical job data through bootstrap. |
| Primary upgrade precedents | PARTIAL | Document cites process and execution precedents. | Adopt explicit capability exchange, sequence/version map, pre-install/readiness and supported skew; do not cite them as correctness proof. |

## Findings

### CITADEL-001 — OCOMP has no canonical normative owner

- Severity: Critical
- Gate: G3, G10
- Evidence: `off-chain-computation.md` is explicitly an architecture proposal;
  `ADR-C-MET-001` still defines READY followed by synchronous Lysis and reserves
  `IN_PROGRESS` rather than accepting `OFFCHAIN_PENDING`.
- Reachable failure mode: code and operators can implement different request
  order, snapshot boundary, expiry tie-break or activation effects while all
  claiming conformance to a non-normative design.
- Structural closure: create/accept one OCOMP ADR plus a
  Tribute→Metadosis→OCOMP→Nod PFS; amend Metadosis, Lysis, Nod, Desis, Promis,
  Update, wire/capacity and release owners by reference.
- Closure test: generated owner/identifier/PFS manifest rejects an OCOMP
  mutation, state tag, codec or lifecycle phase without one normative owner.

### CITADEL-002 — PoC-to-MVP compatibility tuple is incomplete

- Severity: Critical
- Gate: G2, G3, G8, G10
- Evidence: the document says a job pins versions but `JobIntentV1` does not
  explicitly bind action codec, result schema, activation verifier, committee
  key scheme, sign-once domain, block evidence codec and immutable limit set.
- Reachable failure mode: two upgraded components can hash, validate, sign or
  apply the same in-flight job under different semantics.
- Structural closure: define `JobCompatibilityV1` as a complete immutable tuple
  committed inside `IntentId`; every control message, artifact, certificate and
  activation command echoes and checks its hash.
- Closure test: vary each field independently across node/supervisor/worker/
  relayer and require refusal without signing or state change.

### CITADEL-003 — Mixed-version rollout can activate before the result quorum is usable

- Severity: Critical
- Gate: G3, G8, G10
- Evidence: current `Update::activate_scheduled_update` checks the local binary
  ceiling and registered handler, but not that at least `q` committee members
  installed matching OCOMP code, keys, storage schema and capacity.
- Reachable failure mode: the fork creates jobs, but fewer than `q` validators
  can execute/sign them; or an old running validator rejects new block semantics,
  halting liveness at activation.
- Structural closure: an activation plan commits artifact/profile digests and a
  bounded historical readiness snapshot. Governance may schedule only after the
  required consensus and OCOMP readiness thresholds are met, with a cancellation
  safety window.
- Closure test: mixed old/new 4-validator matrices for every count `0..4`,
  restart around activation, missing handler/key/capacity and one Byzantine
  readiness claim.

### CITADEL-004 — In-flight job drain and rollback policy is undefined

- Severity: Critical
- Gate: G3, G8, G10
- Evidence: “support current and previous active job versions until drain” has
  no admission cutoff, maximum support window, old committee retention,
  interpreter retirement guard or rollback boundary.
- Reachable failure mode: an upgrade removes old logic or keys while a valid old
  intent can still produce evidence; conversely, an unsafe post-activation
  downgrade reinterprets already committed state.
- Structural closure: new activation stops old-profile admission but preserves
  old verification/apply logic until every old intent is terminal plus evidence
  retention. Before activation, governance may cancel; after activation,
  protocol versions are monotonic and defects use forward-fix.
- Closure test: create jobs at activation `H-1`, `H`, and `H+1`; restart each
  component on both releases and prove identical terminal outcomes and
  historical replay.

### CITADEL-005 — Certified activation does not yet have unforgeable authority

- Severity: Critical
- Gate: G1, G5
- Evidence: current `nodfactory::api::issue_nod` forwards directly to the runtime
  without a Lysis/activation authority parameter. The proposed document names a
  deep module but does not close constructors/callers.
- Reachable failure mode: another internal caller can reproduce only part of
  activation or issue a Nod outside the certificate/reservation/conservation
  checks.
- Structural closure: the executor creates a private
  `CertifiedLysisActivation` capability only after complete evidence validation;
  downstream batch APIs consume it and return typed receipts. Remove or make
  inaccessible raw equivalent mutation seams.
- Closure test: compile-fail/private-boundary tests plus runtime attempts from
  every other module/ABI/static/nested-call path.

### CITADEL-006 — PoC result size and apply work are not capacity-closed

- Severity: High
- Gate: G6, G9
- Evidence: `MAX_ACTION_STREAM_BYTES = 512 KiB` is provisional and section 1.5
  itself defers maximum encoding proof. The root wire contract limits header
  `extra_data` to 64 KiB, while block-body/gas/internal-work limits still require
  independent closure.
- Reachable failure mode: a protocol-valid maximum result cannot fit or cannot
  apply within block/finality budget, making an accepted job permanently
  unactivatable or allowing proposer/validator resource divergence.
- Structural closure: derive `MAX_POC_TRIBUTES` and result bytes from exact
  canonical encoding, transaction/block byte limit, signature checks, CE writes,
  logs, gas/internal work and measured minimum-machine headroom.
- Closure test: generated `cap-1/cap/cap+1` payloads through proposer, validator,
  import and historical replay under cold maximum-shape state.

### CITADEL-007 — Downstream effects lack one typed conservation contract

- Severity: High
- Gate: G4, G5, G7
- Evidence: current Desis returns `bool`; Promis receives the resulting amount;
  proposed activation lists effects but no exact receipt algebra.
- Reachable failure mode: activation may classify a rejected/duplicate brief as
  success, route the wrong remainder, or commit a state/event combination that
  differs from pre-fork Lysis behavior.
- Structural closure: pure execution emits a typed `LysisEffectPlanV1` with
  explicit Nod, contributor, Tribute, Desis and Promis totals; activation
  validates conservation and consumes one typed receipt from every owner before
  the terminal state/event.
- Closure test: fail before/after each sub-effect, duplicate every receipt and
  compare complete observable pre/post state to the legacy golden oracle.

### CITADEL-008 — OCOMP key lifecycle and committee handover are requirements, not a protocol

- Severity: High
- Gate: G2, G8, G10
- Evidence: PoC uses static separate secp256k1 keys; MVP mentions HSM, epoch
  changes and historical snapshots, while `ADR-S-KEY-001` records unresolved
  production custody/rotation gaps.
- Reachable failure mode: key rotation loses the ability to verify an old job,
  reuses a signing domain, permits equivocation after journal restore or counts
  the same validator twice across handover.
- Structural closure: define key-purpose metadata, epoch snapshot identity,
  overlap/handover FSM, historical-key retention, sign-once journal migration,
  revocation and incident recovery. Never reuse the consensus key.
- Closure test: rotate before/during/after a job, restore an old journal,
  compromise one old/new key and attempt cross-epoch duplicate/conflicting
  certificates.

### CITADEL-009 — OCOMP durable stores are absent from recovery/bootstrap authority

- Severity: High
- Gate: G4, G8, G10
- Evidence: proposal defines pin/export/signing/CAS journals, but canonical
  `RecoveryVectorV1` and `NodeSnapshotManifestV1` do not include an OCOMP
  extension or retention dependency.
- Reachable failure mode: a restarted or bootstrapped validator advertises OCOMP
  readiness without the old sign-once history, source bytes or evidence needed
  for in-flight/historical jobs.
- Structural closure: extend role readiness and recovery vectors with exact
  OCOMP key epoch, journal generation, finalized cursor, pin set and retained
  compatibility set. Define which data is reconstructible, snapshot-carried or
  separately protected secret state.
- Closure test: crash/copy/bootstrap at every journal/checkpoint boundary and
  prove either exact readiness or explicit OCOMP abstention without consensus
  failure.

### CITADEL-010 — IPC compatibility and authentication have no executable skew contract

- Severity: Medium
- Gate: G1, G8, G10
- Evidence: `Hello` carries a protocol range, but no method/structure capability
  set, downgrade rule, session-generation binding or supported skew table is
  defined.
- Reachable failure mode: node and supervisor connect successfully but disagree
  on one message or optional field; retry loops or signing the wrong
  interpretation follows.
- Structural closure: exchange exact versioned method/codec capabilities,
  negotiate one intersection bound to the session and job compatibility hash,
  reject unknown mandatory fields and publish a node/supervisor/exporter/worker
  skew matrix.
- Closure test: full Cartesian compatibility matrix plus downgrade, replay,
  reconnect and removed-method cases.

### CITADEL-011 — MVP completion is not objectively decidable

- Severity: High
- Gate: G6, G9, G10
- Evidence: the MVP column says “measured”, “production”, “hardened” and
  “exhaustive” without numeric SLO/RPO/RTO, minimum machine, alert/runbook,
  required evidence IDs or release decision rule.
- Reachable failure mode: the profile is enabled after a successful happy path
  despite unclosed recovery, security or maximum-load behavior.
- Structural closure: define a `BoundedMVPReleaseGateV1` with binary inputs:
  supported cap, minimum node profile, latency/finality headroom, availability,
  recovery, key custody, chaos, mixed-version, incident response and signed
  exact-artifact evidence.
- Closure test: release pipeline refuses the profile when any requirement is
  Gap, Expired, advisory, skipped or refers to another artifact.

### CITADEL-012 — Full result bytes have no durable consensus handoff to the next block

- Severity: High
- Gate: G2, G4, G8
- Evidence: evidence is accepted by an ordinary transaction in block `h`, while
  activation runs in begin-zone `h+1`. The document says the complete action
  bytes are in block data, but it does not identify the authoritative locator,
  historical-body read, activation system transaction, temporary state or
  cleanup protocol that supplies those bytes at `h+1`.
- Reachable failure mode: the result hash is accepted, but the next block cannot
  deterministically obtain the actions; a restart/pruning boundary can make an
  otherwise certified job unactivatable. Storing hundreds of KiB in ordinary
  EVM slots would create a different unmetered capacity problem.
- Structural closure: define one `PendingActivationPayloadV1` handoff. The
  recommended bounded-PoC design stores the canonical payload once in the
  evidence transaction body and commits an exact
  `(block_hash, tx_index, payload_hash, encoded_bytes)` locator. The next
  activation system transaction carries/replays those exact bytes, validators
  compare them to the locator/hash before mutation, and terminal commit removes
  only the small pending record. Reth body retention and replay are part of the
  consensus prerequisite.
- Closure test: crash/restart/prune between evidence commit and activation,
  mutate the carried bytes/locator, omit the activation payload and re-import
  both blocks through the production executor.

### CITADEL-013 — Request-time and activation-time semantics are not separated

- Severity: High
- Gate: G2, G6, G7
- Evidence: `JobIntentV1` names `logical_evaluation_time`, but the proposed
  `BoundedLysisResultV1` and activation contract do not map every timestamp and
  event field to request or activation time. Current Nod issuance reads
  `storage.timestamp()` and current Desis/Metadosis events use the execution
  block timestamp/number.
- Reachable failure mode: the same certified semantic result applied after a
  different delay produces different Nod bodies, auction timing or event bytes;
  native/reference executions can agree while proposer/validator replay differs
  at activation.
- Structural closure: define a field-level time table. Economic Lysis inputs and
  result identity use the request's frozen logical time; operational activation
  height/time are separate metadata and cannot alter signed Nod/Desis/Promis
  values. If a downstream invariant genuinely requires activation time, that
  field is excluded from the semantic result and derived identically during
  activation.
- Closure test: activate the same fixture after delays of `1`, `2` and
  `deadline-1` blocks and require identical semantic output plus only the
  explicitly allowed activation metadata differences.

### CITADEL-014 — Finality proof and OCOMP committee snapshot are still placeholders

- Severity: High
- Gate: G1, G2, G8, G10
- Evidence: `PoCResultEvidenceV1` contains `finalization_proof`, and the intent
  contains `result_committee_epoch_hash`, but no canonical proof codec,
  historical committee/key registry, proof-of-possession, duplicate-key rule or
  exact Commonware-to-EVM verification seam is specified.
- Reachable failure mode: validators disagree about the finalized request
  identity or which public keys count toward `q`; stale/unbound/duplicate keys
  can be counted, or a proof can become unverifiable after committee rotation.
- Structural closure: define bounded `FinalizedIntentProofV1` and
  `OcompCommitteeSnapshotV1` codecs in the wire registry. Snapshot creation
  binds one unique OCOMP key and proof-of-possession to each validator identity,
  fixes activation/retirement heights and remains historically readable.
- Closure test: wrong block/state root/intent inclusion, malformed or oversized
  proof, stale epoch, duplicate validator/key, missing proof-of-possession and
  rotation across an in-flight job.

### CITADEL-015 — One signed result has two incompatible digest definitions

- Severity: Critical
- Gate: G2, G6, G8
- Evidence: section 1.3 defines `ResultDigest` from `JobId`,
  `ActionStreamHash`, counts, totals and event summary, while section 8.2 defines
  the same domain-separated digest from `canonical(ActivationPayloadV1)`. Those
  preimages are not byte-identical and no precedence rule exists.
- Reachable failure mode: honest validators or node/supervisor versions sign and
  verify different digests for the same semantic result; quorum becomes
  impossible or an implementation accepts a signature over an unintended
  subset.
- Structural closure: define `ActionStreamV1` and `ActivationPayloadV1` once,
  require every redundant summary to recompute from the stream, and make the
  only valid formula
  `H("OUTBE_OCOMP_RESULT_V1", canonical(ActivationPayloadV1))`.
- Closure test: frozen cross-language bytes/hashes plus a source/manifest check
  that the domain has exactly one normative preimage definition.

### CITADEL-016 — Upgrade handler identity is not committed by the current update plan

- Severity: Critical
- Gate: G4, G7, G10
- Evidence: current schedule state commits protocol version, height and free-form
  info. `UpgradeHandlerRegistry::lookup` may be empty, and activation without a
  handler is explicitly tested as success. Two binaries can therefore advertise
  the same protocol ceiling while one runs an OCOMP migration handler and the
  other performs only the active-version write.
- Reachable failure mode: at activation height both nodes accept the scheduled
  version but compute different state roots.
- Structural closure: schedule and startup bind an exact
  `ProtocolBundleHash`/`migration_manifest_hash` and ordered required-handler
  set. Missing, extra or differently hashed required handlers fail readiness
  before scheduling and fail closed at activation.
- Closure test: same protocol version with empty, missing, extra and modified
  registries must never reach an activation block with divergent writes.

### CITADEL-017 — Common semantic bug has no governed pause/recovery transition

- Severity: High
- Gate: G3, G5, G8, G10
- Evidence: the failure table correctly says a quorum can reproduce a common
  software bug, but the job/profile FSM has no consensus-visible pause or drain
  mode and no rule for already accepted but not yet activated evidence.
- Reachable failure mode: operators discover a common bug after certification;
  new intents and queued activations continue deterministically applying the
  known-bad semantics while a normal protocol upgrade is prepared.
- Structural closure: add governed profile modes `ENABLED`, `DRAIN_ONLY` and
  `PAUSED`. `PAUSED` creates no new intents and converts non-activated pending
  jobs to a bounded typed cancel/expire path; already activated state remains
  authoritative until a monotonic repair version proves and applies a
  deterministic correction. Local health can abstain but cannot switch
  consensus mode.
- Closure test: pause before request, after request, after result acceptance and
  after activation; prove exact terminal/reservation behavior and forward-fix
  replay on all validators.

## Target architecture

```text
canonical ADR/PFS + versioned protocol manifest
        |
        v
terminal RequestCommand
  [closed FSM + module-owned checkpoint]
        |
        v
JobIntent(JobCompatibilityHash, immutable input/reservations)
        |
        +--> node-local retention outbox/pin journal
        |
        v
version-negotiated OcompControl
        |
        v
exporter -> deterministic workers -> reducer
        |
        v
node-local sign-once gate -> q certificate
        |
        v
EvidenceCommand [typed receipt, idempotent binding]
        |
        v
ActivationCommand
  validate -> private CertifiedLysisActivation
  -> typed downstream batch adapters and receipts
  -> conservation check
  -> atomic record/index/event commit
        |
        v
terminal state + separately reconciled retention/GC
```

The PoC and MVP use this identical core path. MVP changes implementations and
enabled limits, not intent identity, evidence meaning or activation authority.
`TargetLarge` is a later protocol profile because proof, DA, witness-based state
and claims change the evidence and state representation.

## Verification plan

1. Assign stable requirement IDs for request, evidence, activation, expiry,
   recovery, compatibility and release gates.
2. Build an independent pure Lysis reference and frozen pre-fork golden corpus.
3. Add stateful model tests covering every legal/illegal job and upgrade
   transition, including deadline and activation ties.
4. Add maximum-shape codec/work generators before selecting PoC/MVP caps.
5. Execute the four-validator public Tribute-to-Nod PoC with one domain absent
   and tampered evidence.
6. Prove the finalized-intent proof, cross-block payload handoff and delayed
   activation time semantics.
7. Inject failure before and after every EVM/CE/event/downstream receipt boundary.
8. Run the node/supervisor/exporter/worker compatibility Cartesian matrix.
9. Run `H-1/H/H+1` mixed-version activation and old-job drain tests.
10. Kill and restore pin, exporter, CAS, signer and supervisor journals at every
   durable boundary.
11. Bind results to exact release artifacts and fail closed on any missing,
    stale, skipped or advisory requirement.

## Verification log

| Check | Result |
|---|---|
| Document identity before audit | SHA-256 `ef50698111aca5b665dcea091d89d63c67378bebfe19139e1d3896ab662e8705` |
| Worktree isolation | Only pre-existing ZeroFee changes plus untracked architecture document observed; no production code modified by this audit. |
| Current Metadosis call path | CodeGraph confirms synchronous `process_metadosis -> lysis -> Nod/Intex/Tribute -> Desis/Promis/completion/retirement`. |
| Current Lysis atomicity | One `StorageHandle::with_checkpoint` wraps `lysis_inner`; outer Metadosis/block atomicity remains required for later effects. |
| Current Nod authority | CodeGraph confirms public `nodfactory::api::issue_nod` has validation but no unforgeable Lysis caller capability. |
| Current update activation | CodeGraph confirms handlers run before active-version write in one checkpoint; failure leaves update scheduled and causes fatal block execution error. |
| Existing handler tests | Success, failure rollback and no replay are covered at module level; distributed readiness and mixed-version rollout are not. |
| Repository capacity contract | ADR-B-CAP-001 requires one versioned capacity profile, pre-allocation bounds and maximum-machine evidence; current project closure remains Proposed. |
| External practice check | Cosmos versioned migrations, Fabric definition readiness/sequence, Engine API capability exchange and Kubernetes explicit skew/order support the proposed transition controls. |

### Post-remediation design re-audit

The verdict at the top of this report applies to the audited pre-remediation
document SHA `ef506981…`. The remediated architecture specification was frozen
and independently re-audited at:

```text
off-chain-computation.md
SHA-256 3582e1699765e9f5ec9994f89eade74eb23b4319b4fad534fa5d35cb2dc165ba
3417 lines
177436 bytes
```

Final specification-level verdict: **APPROVE by all three independent review
groups**. Each group inspected that exact SHA without editing it. Static checks
also found one normative `ActionStreamHash`, one normative `ResultDigest`, an
even code-fence count, no stale two-phase PoC wire type, no missing local links
and no whitespace errors.

This approval means the document has no known blocking architectural ambiguity
after the adversarial review. It does **not** mean PoC or MVP exists in code.
Current production still executes synchronous Lysis and all implementation,
test, migration, evidence and acceptance gates in the Definition of Done below
remain unchecked.

| Original finding | Specification closure in final SHA |
|---|---|
| CITADEL-001 | exact ADR/PFS ownership map, accepted revision/section digests and mandatory no-removal release catalog |
| CITADEL-002 | transitive protocol/object/semantics registries plus domain-separated canonical hash/signature cores |
| CITADEL-003 | deterministic cutoff, separate consensus/result readiness predicates and stable committee/key snapshots |
| CITADEL-004 | active-marker writer rule, arbitrary live-job support set, replay set, drain and forward-only recovery |
| CITADEL-005 | move-only private certified capability and module-owned checkpoint-scoped receipts |
| CITADEL-006 | generated cap with public RPC, txpool, gossip, proposal, import and replay path |
| CITADEL-007 | frozen receipt schemas, reservation bindings and cross-module conservation equations |
| CITADEL-008 | purpose-bound HSM signing, sign-once readiness, rotation, compromise revocation and historical as-of verification |
| CITADEL-009 | pin/export/sign journals, bootstrap roots, evidence retention and bounded recovery contracts |
| CITADEL-010 | exact Hello/HelloAck versions, capabilities, session generation and N/N+1 skew matrix |
| CITADEL-011 | authority-anchored release gate, monotonic catalog/policy and signed verification-ledger cores |
| CITADEL-012 | one activation transaction carries, verifies and atomically applies bounded result bytes |
| CITADEL-013 | request logical time is separate from activation receipt time |
| CITADEL-014 | exact current Commonware finalization proof chain and authenticated historical committee authority |
| CITADEL-015 | one canonical action/result digest definition |
| CITADEL-016 | acyclic PlanCore→Gate→Envelope hashes, committed handler set, preparation, recoverable cutoff/activation failure |
| CITADEL-017 | O(1) pause barrier, bounded restartable cancellation cursor, same-height ordering and forward repair |

The re-audit also closed issues found only after the first remediation: terminal
retry ordering, `CANCELED` pin release, active-bundle selection after failed `H`,
approval self-signing cycles, plan/gate hash cycles, signer-to-OCOMP identity
binding, `UnitId`, `SourceSubjectId`, `ActivationCallId`, fragment/ledger
signature cores and the full Fidelity/committee/genesis/operations/txpool owner
set.

## Decisions required

1. Is PoC strictly a disposable new devnet fork? **Recommended: yes.** It avoids
   migrating existing live WWD records while proving the real protocol path.
2. Does BoundedMVP retain the same `BoundedLysisResultV1` semantics? **Required:
   yes**, with a new version only for a semantic or wire change.
3. Can old in-flight jobs be converted? **Recommended: no.** Drain/expire them
   under the exact old compatibility tuple; never reinterpret or relabel.
4. What is rollback after protocol activation? **Recommended: no downgrade.**
   Cancel before activation; use a monotonic forward-fix after activation.
5. What readiness threshold is required before scheduling? At minimum the
   consensus safety threshold plus enough distinct OCOMP-capable committee
   domains to form `q`; the exact governance rule must be fixed.
6. Does MVP require HSM/remote signing at first supported-network activation?
   **Recommended: yes**, unless a narrower explicitly non-production network
   profile and risk acceptance is named.

## Migration and documentation impact

- Add an OCOMP owner ADR and a Tribute→Metadosis→OCOMP→Nod PFS.
- Amend Metadosis, Lysis, NodFactory, Desis, PromisLimit, Update, wire, capacity,
  supervision, testing, release, key and recovery ADRs.
- Register `OFFCHAIN_PENDING`, all OCOMP records/indexes, message/result codecs,
  domains, key purposes, caps and retired values in the protocol manifest.
- Add an atomic update handler for state tags, indexes, reservations and module
  schema versions. The handler must validate existing READY state and remain
  restart/replay safe.
- Add operator rollout, cancellation, forward-fix, key rotation, journal repair,
  bootstrap/restore and profile-disable runbooks.
- Preserve historical compatibility decoders/interpreters for replay even after
  they are retired from new admission.

## Definition of done

- [ ] G1: only the executor can construct certified activation authority; raw
  equivalent mutation paths are closed.
- [ ] G2: every OCOMP persisted record is strictly versioned and every
  record/index/reservation invariant is enforced.
- [ ] G3: canonical job and upgrade FSMs enumerate all legal/illegal transitions
  and progress/recovery paths.
- [ ] G4: request/evidence/activation atomicity and every local journal handoff
  pass crash-boundary tests.
- [ ] G5: every downstream effect returns and consumes a typed, job-bound receipt
  with exact conservation.
- [ ] G6: PoC and MVP caps are generated from exact encoded/work maxima and pass
  cold minimum-machine benchmarks.
- [ ] G7: one owner exists for each job, expiry, reservation, generation and
  committee fact; all duplicate indexes have checked equivalences.
- [ ] G8: duplicate, stale, reorg, equivocation, old-job drain, key handover and
  mixed-version histories pass.
- [ ] G9: public production-interface, model, fault, capacity, upgrade and
  recovery evidence is present in the VerificationLedger for the exact release.
- [ ] G10: OCOMP ADR/PFS, schema/protocol activation, migration and operator
  contracts are accepted.
- [ ] PoC: the exact four-validator Tribute-to-Nod demonstration passes without
  any on-chain Lysis call or fallback.
- [ ] Transition: `H-1/H/H+1` jobs complete or expire under their original
  compatibility tuple across independent rolling upgrades.
- [ ] MVP: every `BoundedMVPReleaseGateV1` item passes; none is skipped,
  advisory, stale or produced for another artifact.
