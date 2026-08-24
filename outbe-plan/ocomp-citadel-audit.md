# Citadel audit: OCOMP lifecycle

## Verdict

- Fixed-point status: **NOT CITADEL**
- Closure status: **CITADEL for the audited lifecycle scope** in the implemented working tree
- Confidence: **high**
- Fixed point: `c11363473055be94b12b65c07edb29f93aff0182`
- Scope: canonical Metadosis job lifecycle, embedded OCOMP state, `outbe-chain` ExEx orchestration,
  asynchronous compute/vote outcomes, checkpoint/readiness, vote submission, materialization,
  payout, restart/replay, and current unit/integration/E2E evidence.
- Trust assumptions: finalized provider state is canonical; Workers and local files can be late,
  unavailable, corrupt, or interrupted; the ExEx actor serializes its in-memory mutations, while
  compute/vote/materialization/payout threads complete asynchronously.
- Atomicity domains: Metadosis/EVM mutations use the execution checkpoint; local OCOMP artifacts and
  journals use their own durable no-clobber stores; RPC submissions are externally asynchronous.

At the fixed point, the on-chain OCOMP FSM was strongly defended by typed restoration,
clone/validate/commit transitions, fault-boundary tests, and an independent generated reference
model. Its weak seam was the translation from canonical `OcompJobStatus` plus asynchronous local
outcomes into the embedded ExEx FSM: valid terminal statuses were missing, terminal states were not
absorbing, and historical jobs were never pruned from the per-block reconciliation set. The
findings and gate tables below preserve that baseline audit. The correction freezes, verification
log, and definition of done record their implemented closure; they are not descriptions of the
fixed-point code.

## Sources of truth and scope

- `README.md:327-365` — genesis activation, WWD/OCOMP lifecycle, terminal-record budget, and dynamic
  ValidatorSet membership.
- `docs/flows/002-off-chain-poc-protocol-flow.md` — normative end-to-end authority, failure, restart,
  and materialization behavior.
- `outbe-plan/ocomp-supervisor-exex.md:114-335` — embedded Validator/FullNode invariants, complete
  transition matrix, checkpoint and replay contract.
- `crates/system/ocomp-protocol/src/state.rs:60-88` — canonical terminal outcomes and job statuses.
- `crates/core/metadosis/src/ocomp/state.rs:281-385` — on-chain FSM construction, restoration, and
  atomic in-memory transition seam.
- `bin/outbe-ocomp/src/embedded.rs:13-295` — process-local embedded FSM.
- `bin/outbe-chain/src/ocomp_exex.rs:246-1153` — production reducer and asynchronous effect adapter.

The audit does not re-prove cryptographic primitives or every Lysis arithmetic formula. It verifies
that their existing typed results, receipts, and tests are connected to every production lifecycle
scenario without an uncovered state transition.

## Mutation interface and call graph

```text
finalized provider head
  -> reconcile
     -> scan_requests
     -> refresh_jobs
        -> observe_job / restore_local_result
        -> ensure_compute_started
        -> observe_completed | observe_no_quorum
        -> observe_deadline
     -> reconcile_materialization
     -> drive_payout
     -> publish_readiness

compute thread -> compute_rx -> handle_compute -> durable local result -> accept_local_result
vote thread    -> vote_rx    -> handle_vote
NOD thread     -> materialization_rx -> handle_materialization
payout thread  -> payout_rx  -> handle_payout
```

The poll order is significant: each 250 ms tick reconciles canonical state first, then drains
compute, vote, materialization, and payout outcomes (`ocomp_exex.rs:375-420`). Therefore a canonical
terminal event and an already-queued local outcome routinely occur in one actor turn.

## Side-effect ledger

| Effect | Owner | Atomicity domain | Commit/rollback point | Receipt/retry |
|---|---|---|---|---|
| OCOMP request/open/expire/conflict/complete | Metadosis | EVM checkpoint | `JobFsmState::apply` candidate validation plus storage checkpoint | typed job/terminal record and events |
| Local Lysis result | `LocalLysisResultStore` | journaled local | immutable result commit before ExEx state acceptance | content/digest-bound load and replay |
| Result vote | `SupervisorVoteSubmitterV1` | local journal + external tx | prepared signed bytes before broadcast; finalized receipt | durable reconcile/rebroadcast |
| NOD materialization | Metadosis/NOD plus submitter journal | EVM transaction + local reference journal | EVM batch rollback; finalized receipt reconciliation | retry wake and canonical FIFO cursor |
| Contributor payout | payout submitter | local journal + external tx | submitter state and finalized receipt | later finalized block tick resumes |
| Embedded checkpoint | ExEx checkpoint store | journaled local | persisted only when no live jobs and all requests materialized | exact `{height, hash}` recovery |
| Fatal evidence/status | ExEx | local diagnostic + node lifecycle | evidence persist then readiness/exit publish | repeated startup fails closed where modeled |

## Observed FSM coverage at the fixed point

| Scenario | Current behavior | Evidence | Status |
|---|---|---|---|
| Validator local-first -> vote -> quorum | durable result and vote journal; network completion | primary OCOMP E2E + vote submitter tests | PASS |
| Validator canonical-first -> late local result | `ProtocolOwned` no-op | state unit; current uncommitted E2E passed on this fixed point | PASS in working tree, not yet delivered |
| FullNode local-first exact canonical | verifies and releases readiness | primary OCOMP E2E + state unit | PASS |
| FullNode canonical-first exact local | state transition exists | `embedded_state` only | PARTIAL |
| FullNode deadline wait -> late exact -> resume | state transition exists | `embedded_state` only; required production E2E is absent | PARTIAL |
| FullNode canonical mismatch | action exists | state unit only; evidence + isolated shutdown path is not exercised | PARTIAL |
| Voting-open `Expired` | closes Computing/WaitAtDeadline/LocalReady | state unit + public expiry E2E (Validator topology) | PARTIAL |
| Awaiting-finality `Expired` | skipped forever because `finalized == None` | on-chain test proves status is reachable; ExEx has no test | FAIL |
| `Conflicted` terminal | ExEx returns fatal error | on-chain conflict test proves status is reachable | FAIL |
| `Canceled` terminal after VotingOpen | ExEx returns fatal error | on-chain failed-day test proves status is reachable | FAIL |
| `Canceled` terminal before finality | skipped forever because `finalized == None` | on-chain failed-day test proves status is reachable | FAIL |
| Local Completed after terminal close | can rewrite `ClosedNoQuorum -> LocalReady` until next poll | direct embedded transition; no test | FAIL |
| Local Unrecoverable after terminal close | FullNode publishes fatal | production poll/outcome ordering; no test | FAIL |
| Vote Unrecoverable after canonical terminal | Validator publishes fatal | unconditional `handle_vote`; no test | FAIL |
| Temporary Worker absence/recovery | retry until Worker returns | late-result WIP E2E and worker isolation E2E | PASS for one ordering |
| Two concurrent jobs / pinned membership | independent jobs and historical membership | state unit + validator-lifecycle dynamic overlap E2E | PASS |
| Crash/restart after completed generation | exact result/vote/materialization replay | primary/capacity E2Es and journal tests | PASS |
| Crash after local result before canonical | restoration code exists | store/journal tests do not exercise the complete ExEx ordering | PARTIAL |
| Checkpoint recovery/regression/notification error | bounded typed recovery and fatal mismatch | 16 focused ExEx tests | PASS |
| Bounded materialization and restart | canonical FIFO batches and replay | 10/257-record E2Es | PASS |
| Contributor payout | certified authority through payout | primary/capacity E2Es | PASS |

## Citadel gates at the fixed point

| Gate | Status | Evidence | Gap/closure |
|---|---|---|---|
| G1 Deep, closed interface | PARTIAL | on-chain and local stores have narrow typed interfaces | ExEx reproduces terminal policy across `refresh_jobs`, `handle_compute`, and `handle_vote` |
| G2 Valid state model | FAIL | typed enums and on-chain restore validation | `record_local_result` can reopen `ClosedNoQuorum` |
| G3 Explicit FSM | FAIL | on-chain FSM has registered transition table | embedded FSM lacks canonical `Conflicted`/`Canceled` and terminal async rows |
| G4 Atomicity/error guarantees | PARTIAL | EVM checkpoint and durable local journals are explicit | late outcomes are not fenced against a newer canonical terminal decision |
| G5 Explicit effects/receipts | PARTIAL | vote/materialization/payout use typed outcomes | late `Unrecoverable` is interpreted without job phase/generation |
| G6 Deterministic bounded execution | FAIL | per-object and per-day limits exist | every finalized block rereads every process-lifetime request; maps never prune |
| G7 Single-source invariants | PARTIAL | finalized chain is authority | local state separately encodes only a subset of canonical terminal outcomes |
| G8 Replay/retry/concurrency | FAIL | actor mutations are serialized and durable stores replay | worker/vote completion races have no terminal fencing; eligibility lookup error is cached as permanent absence |
| G9 Production-interface evidence | FAIL | broad happy-path, capacity, mutation, expiry, restart, and membership E2Es | required FullNode fault/order cases and complete status/outcome matrix are absent |
| G10 Project contract | FAIL | most README/flow behavior is reflected in code | valid canonical terminal statuses contradict ExEx behavior; plan §13 claims E2Es that do not exist |

## Fixed-point findings

### OCOMP-001 — Canonical terminal statuses are not completely translated

- Severity: **High**
- Gates: G2, G3, G7, G10
- Evidence:
  - canonical status set includes `Expired`, `Conflicted`, and `Canceled`
    (`ocomp-protocol/src/state.rs:60-88`);
  - production commits valid conflicted and canceled records
    (`metadosis/src/ocomp/transitions.rs:60-118, 643-679`);
  - awaiting-finality expiry is a tested production transition
    (`metadosis/src/tests/ocomp_request.rs:812`);
  - ExEx ignores every record without `finalized` before inspecting status
    (`ocomp_exex.rs:583-585`), and bails for `Conflicted | Canceled`
    (`ocomp_exex.rs:623-635`).
- Reachable failure mode:
  - `AwaitingFinality -> Expired/Canceled` never enters `materialized_requests`, so the durable
    checkpoint cannot advance;
  - finalized `Conflicted` or `Canceled` makes `reconcile` fail and the poll loop publishes a fatal
    node shutdown.
- Structural closure: introduce one exhaustive canonical-outcome reducer before the
  `finalized` requirement. Terminal-without-canonical outcomes must close/cancel local work and
  satisfy the request barrier whether or not a finalized job payload exists. Preserve the exact
  reason rather than calling every closure `NoQuorum`.
- Closure test: table-drive all six `OcompJobStatus` variants with both legal finalized shapes;
  assert local phase, cancellation, readiness, checkpoint, node-exit behavior, and replay.

### OCOMP-002 — Late asynchronous outcomes are not fenced by terminal authority

- Severity: **High**
- Gates: G2, G3, G4, G8
- Evidence:
  - canonical reconciliation runs before channel draining (`ocomp_exex.rs:375-420`);
  - no-quorum sets cancellation (`ocomp_exex.rs:744-756`), but a compute result/error may already
    be queued and compute sends do not perform an atomic terminal-generation check
    (`embedded_runtime.rs:395-440`);
  - `record_local_result` unconditionally writes `LocalReady` before deciding the action
    (`embedded.rs:119-165`);
  - FullNode local failure and vote failure publish fatal without consulting current terminal state
    (`ocomp_exex.rs:761-771, 877-884`).
- Reachable failure mode: after finalized expiry/cancellation, a queued Completed result resurrects
  the closed local state; a queued Unrecoverable compute outcome shuts down a FullNode; a queued
  vote setup failure can shut down a Validator after the protocol already owns the outcome.
- Structural closure: make canonical terminal state absorbing and correlate every local outcome to
  the current job generation/phase. `LocalCompleted`, `LocalFailed`, `VoteFinalized`, and
  `VoteFailed` must all be reduced through the same terminal-aware interface.
- Closure test: deterministic actor-order tests enqueue each local outcome, advance canonical state
  to Completed/Expired/Conflicted/Canceled, then drain the outcome and assert no reopening, no late
  transaction, no false fatal, and stable replay.

### OCOMP-003 — Reconciliation work grows with process-lifetime history

- Severity: **Medium**
- Gates: G6, G8
- Evidence: `refresh_jobs` copies and rereads all `requests` on every finalized height
  (`ocomp_exex.rs:572-651`); `requests`, `jobs`, `materialized_requests`, and
  `materialization_attempt_heights` have no `remove`, `retain`, or `clear`; completed jobs retain
  cloned canonical results.
- Reachable failure mode: legitimate terminal attempts monotonically increase per-block state reads,
  allocations, and resident memory until process restart. The on-chain per-WWD cap does not bound
  cumulative process-lifetime work.
- Structural closure: after a durable checkpoint covers a terminal request, remove it from active
  reconciliation state. Keep only active jobs and the minimum bounded materialization retry state.
- Closure test: process more than one terminal batch, advance checkpoint, and prove subsequent tick
  work is a function of active jobs rather than historical jobs; repeat after restart.

### OCOMP-004 — Snapshot lookup failure becomes permanent vote ineligibility

- Severity: **Medium**
- Gates: G3, G5, G8
- Evidence: `validator_vote_eligible` converts every provider/decoding error to `false`
  (`ocomp_exex.rs:850-873`); that boolean is stored once at discovery (`ocomp_exex.rs:598-615`) and
  is never recomputed.
- Reachable failure mode: a transient exact-state read failure permanently makes an eligible
  Validator abstain for that job. The state cannot distinguish “not in pinned snapshot” from
  “authority temporarily unavailable.”
- Structural closure: use a typed `Eligible | NotMember | Unavailable` result. Only `NotMember` is
  terminal abstention; `Unavailable` retains the durable local result and retries the exact snapshot
  lookup without creating a vote journal.
- Closure test: first lookup fails and later resolves eligible -> exactly one vote; exact absence ->
  permanent no-vote; restart yields the same decision.

### OCOMP-005 — Required FullNode and terminal-order evidence is absent

- Severity: **Medium**
- Gate: G9
- Evidence: `embedded_state.rs` has seven local tests and ExEx has sixteen checkpoint/wake/helper
  tests, but none exercises `refresh_jobs -> observe_completed/observe_no_quorum -> channel drain`.
  Current feature scenarios cover happy path, materialization, capacity, worker isolation, Validator
  late result (working tree), voting-open expiry, mutation, and dynamic membership. The production
  FullNode deadline wait/resume, canonical-first, mismatch evidence/shutdown, local-result crash
  window, and the complete terminal-status matrix are absent despite
  `outbe-plan/ocomp-supervisor-exex.md:438-471` requiring them.
- Structural closure: add one production-interface reducer harness, not more private-method tests.
  Use it for a table-driven status/outcome matrix, then retain only the few E2Es needed to prove the
  real Worker/ExEx/readiness/node-lifecycle seam.
- Closure test: the matrix plus dedicated FullNode deadline/resume and mismatch-isolated-shutdown
  release E2Es.

## Target architecture

```text
finalized job record + local async outcome
                  |
                  v
       one exhaustive JobReducer
       - canonical status authority
       - terminal/generation fence
       - typed eligibility retry
                  |
        transition + effect plan
                  |
   durable stores / vote / readiness adapters
                  |
       bounded active-job projection
```

This does not require a new wire format, scheduler, process, or consensus rule. It consolidates the
already-intended lifecycle into one owner and deletes duplicated caller policy.

## Finalized-awaiting-open correction freeze

The release SGX/no-attest run at
`/tmp/ocomp-citadel-e2e/run-1787405521-836300` disproved one classifier row added during the
hardening work. `AwaitingFinality` does not imply that the finalized payload is absent. Metadosis
first records the immutable finalized request binding and deliberately retains
`AwaitingFinality`; only the exact `open_height` transition changes the status to `VotingOpen`.

Scope is one correction pass in `bin/outbe-chain/src/ocomp_exex.rs`. Non-goals are changes to the
canonical record, Metadosis transitions, embedded FSM/runtime, ABI, storage, wire types, voting
timing, or E2E semantics.

The complete corrected classifier matrix is:

| Canonical status | Finalized payload | ExEx effect | Error/replay |
|---|---:|---|---|
| `AwaitingFinality` | absent | retain the request barrier; do not discover or start the job | retry from canonical state |
| `AwaitingFinality` | present | validate and discover the exact job and mark the request materialized; leave any durable local result dormant until `VotingOpen` | idempotent on every reconcile/restart; no compute, reducer event, or vote before open |
| `VotingOpen` | present | existing discovery and compute/vote policy | idempotent existing path |
| `Completed` | present | existing canonical-result policy | absorbing existing path |
| `VotingOpen` or `Completed` | absent | fatal invalid canonical shape | sticky fatal evidence |
| `Expired`, `Conflicted`, or `Canceled` | either | existing typed terminal absorption | idempotent terminal replay |

The invariant owner remains the private canonical classifier and `refresh_jobs` orchestration in
`ocomp_exex.rs`; no public or canonical type changes. RED changes the inline matrix test so the
finalized-awaiting-open row is legal and still proves that live statuses without their required
payload are rejected. GREEN introduces a private `FinalizedAwaitingOpen` disposition and restores
the pre-hardening behavior: early discovery without early computation. The exact release
`@ocomp-late-local-result` SGX/no-attest scenario is the integration gate before PR5 resumes.

This was an explicitly authorized classifier correction pass for `ocomp_exex.rs`. The subsequent
release run discovered the separate asynchronous-outcome composition defect frozen below; that
newly authorized correction supersedes the earlier "final" wording without widening its product
scope.

## Async outcome, pruning, and canonical-first correction freeze

The release SGX/no-attest run at
`/tmp/ocomp-late-result-e2e/run-1787407134-901287` proved that the canonical reducer and durable
checkpoint are individually correct but their composition is not. A canonical terminal can be
checkpointed and pruned while a compute or vote callback is already in flight. The callback then
reaches the strict reducer after its private projection has been retired and is incorrectly
reported as an unknown-job fatal. The same audit found two adjacent ordering rows: a FullNode still
awaiting exact local verification must fail closed on an unrecoverable local computation, and
restart must apply canonical authority before replaying a durable local result.

### Scope and non-goals

This correction has exactly two production owners:

- `bin/outbe-chain/src/ocomp_exex.rs`: private async-outcome projection classification and
  canonical-before-restore orchestration;
- `bin/outbe-ocomp/src/embedded.rs`: the `LocalFailed` state transition.

Tests are limited to the inline ExEx tests and
`bin/outbe-ocomp/tests/embedded_state.rs`. This correction does not change the canonical OCOMP
record, Metadosis, `embedded_runtime.rs`, ABI, storage formats, wire types, voting/deadline timing,
SGX behavior, or E2E scenario semantics. It does not retain terminal jobs indefinitely and does not
weaken the public reducer's `UnknownJob` error.

### Invariants and ownership

1. Finalized chain state remains the canonical lifecycle authority.
2. A private compute/vote callback is correlated against both process-local projections before any
   side effect. Runtime and reducer generations must either both exist and agree, or both be absent
   after a durable checkpoint prune. One-sided presence or disagreement is a fatal projection
   invariant.
3. Both projections absent means this actor previously owned and durably retired the job. The late
   private callback is `ProtocolOwned`; it cannot vote, reopen readiness, or create a generic fatal.
   This rule is private to the actor-owned callback channels and does not make arbitrary unknown
   reducer events valid.
4. A FullNode canonical `Completed` state is not locally terminal while its reducer remains
   `Computing` or `WaitAtDeadline`: exact local verification is still mandatory. Current-generation
   `LocalFailed` in either state is `FatalLocalFailure`. Validator `Completed`, FullNode `Verified`
   or `FatalMismatch`, and canonical `Expired`/`Conflicted`/`Canceled` are absorbing.
5. On discovery/restart, canonical disposition is applied before a durable local result can enter
   the reducer. `FinalizedAwaitingOpen` never restores, computes, or votes; `VotingOpen` may restore
   before starting new compute; `Completed` is observed before FullNode restore/verification;
   closed terminals cancel and never restore.
6. Checkpoint pruning remains bounded and immediate. Waiting for callbacks or delaying pruning is
   not a liveness mechanism because callback latency is unbounded.

### Complete transition matrix

| Projection / canonical state | Event and actor | Effect | Error, replay, and restart |
|---|---|---|---|
| runtime and reducer generations present and equal | current or stale compute/vote callback | delegate to the strict reducer; stale event generation remains `ProtocolOwned` | existing deterministic reducer semantics |
| both projections absent after checkpoint retirement | `LocalCompleted` | persist the late result when possible for diagnostic/reuse value, then no reducer/effect action | persistence failure is diagnostic only because canonical protocol already owns the retired job; replay remains a no-op |
| both projections absent after checkpoint retirement | `LocalFailed`, `VoteFinalized`, or `VoteFailed` | diagnostic `ProtocolOwned` no-op | no zero-JobId fatal; restart has no process-local callback queue |
| only one projection exists, or their stored generations disagree | any private callback | sticky fatal projection invariant before durable callback side effects | fail closed; never reinterpret corruption as retirement |
| `Completed`; FullNode `Computing` or `WaitAtDeadline` | current `LocalCompleted` exact / different | `ReleaseProgress` / `FatalMismatch` | existing evidence and shutdown behavior |
| `Completed`; FullNode `Computing` or `WaitAtDeadline` | current `LocalFailed` | `FatalLocalFailure` | persist local-failure evidence and shut down only this node |
| `Completed`; Validator `Verified`, or FullNode `Verified` / `FatalMismatch` | any later local/vote outcome | `ProtocolOwned` | absorbing replay |
| `Expired`, `Conflicted`, or `Canceled`; either role | any later local/vote outcome | `ProtocolOwned` | absorbing replay |
| restart/discovery at `FinalizedAwaitingOpen` | durable local result exists | keep it dormant; do not reduce, compute, or vote | reconsider only after exact canonical `VotingOpen` |
| restart/discovery at `VotingOpen` | durable local result exists / absent | restore first / start compute | membership retry may re-run restore; `vote_started` keeps submission single-flight |
| restart/discovery at `Completed` | either role | observe canonical completion first; then restore only for FullNode exact verification; start FullNode compute only when no durable result exists | Validator never creates a new vote; FullNode preserves canonical-first verification |
| restart/discovery at `Expired`, `Conflicted`, or `Canceled` | any durable local result or vote journal | apply terminal cancellation; do not restore or start work | idempotent terminal replay |

The single-compute-worker contract emits at most one compute outcome per generation. A synthetic
second, conflicting `LocalCompleted` after a closed terminal remains fail-closed as internal
corruption; making arbitrary duplicate callbacks absorbing is explicitly outside this slice.
Likewise, the vote submitter does not emit `VoteFinalized { success: false }`; reverted receipts
remain retryable and this impossible private callback is not used to expand the state machine.

### TDD, paths, and evidence

RED is one batched matrix wave before GREEN:

- `bin/outbe-ocomp/tests/embedded_state.rs`: FullNode canonical-first exact, mismatch, current
  failure from `Computing` and `WaitAtDeadline`, plus Validator/Verified/FatalMismatch and all
  non-result terminal absorption rows;
- inline `ocomp_exex.rs` tests: both-present/both-absent/one-sided/mismatched projection
  classification; post-prune compute and vote dispositions; canonical-before-restore policy for
  `FinalizedAwaitingOpen`, `VotingOpen`, `Completed`, and every closed terminal.

GREEN is one implementation wave in the two owner files. Focused crate tests, fmt, clippy, and the
same release `@ocomp-late-local-result` SGX/no-attest scenario are required before PR5 resumes.
Any third production path, public/canonical type change, or second correction after this pass is a
new stop-and-re-plan trigger.

## Verification plan

1. RED: canonical status matrix for AwaitingFinality/VotingOpen and all terminal outcomes.
2. GREEN: exhaustive canonical-outcome reducer; terminal states absorb late outcomes.
3. RED/GREEN: late Completed/Unrecoverable compute and vote outcomes after every terminal reason.
4. RED/GREEN: transient eligibility lookup recovery versus exact non-membership.
5. RED/GREEN: terminal checkpoint pruning and bounded subsequent reconciliation work.
6. Production-interface FullNode deadline/resume and mismatch shutdown tests.
7. Release SGX/no-attest OCOMP E2E, including the current Validator late-result scenario.

## Verification log

- `cargo nextest run -p outbe-ocomp --test embedded_state --features test-protocol-overrides` —
  **13 passed**.
- `cargo test -p outbe-chain --bin outbe-chain ocomp_exex::tests -- --nocapture` — **22 passed**.
- `cargo nextest run -p outbe-ocomp --features test-protocol-overrides --no-fail-fast` —
  **169 passed, 1 skipped**.
- `cargo clippy -p outbe-ocomp -p outbe-chain --all-targets --features
  test-protocol-overrides -- -D warnings` — **passed**.
- `cargo nextest run -p outbe-metadosis --test ocomp_fsm_model` — **5 passed**.
- `cargo nextest run -p outbe-metadosis certified_conflict_is_terminal_for_the_old_job_and_requeues_the_same_budget` — **1 passed**.
- `cargo nextest run -p outbe-metadosis skipped_response_deadline_fails_day_and_retains_closed_vote_accountability` — **1 passed**.
- `cargo nextest run -p outbe-metadosis awaiting_finality_expires_at_own_deadline_and_releases_live_capacity` — **1 passed**.
- `cargo nextest run -p outbe-metadosis skipped_awaiting_finality_deadline_fails_day_on_first_attempt` — **1 passed**.
- Validator late-result release E2E on real SGX/no-attest with sudo: **10/10 steps passed**;
  evidence `/tmp/ocomp-late-result-e2e.Orng9V/evidence/scenario-001.json` records
  `result = passed`, `log_audit.clean = true`, and zero fatal/panic findings.
- FullNode deadline/restart/late-exact release E2E on real SGX/no-attest with sudo:
  **14/14 steps passed**; evidence
  `/tmp/ocomp-fullnode-deadline-restart.AIM0Gp/evidence/scenario-001.json` records
  `result = passed`, `log_audit.clean = true`, and zero fatal/panic findings.
- FullNode mismatch isolation and sticky restart release E2E on real SGX/no-attest with sudo:
  **12/12 steps passed**; evidence
  `/tmp/ocomp-fullnode-mismatch-rerun.vRBjXN/evidence/scenario-001.json` records
  `result = passed`, `log_audit.clean = true`, six exact expected mismatch shutdown records, no
  unexpected findings, and zero panic records.
- FullNode local-before-canonical restart and changed-binding isolation release E2E on real
  SGX/no-attest with sudo: **14/14 steps passed**; evidence
  `/tmp/ocomp-public-mutation.pD5J5y/evidence/scenario-001.json` records
  `result = passed`, `log_audit.clean = true`, and zero fatal/panic findings.
- Exact mismatch shutdown-bundle audit regression, including wrong job/node, missing, reordered,
  duplicate, unrelated fatal, and panic negative cases: **1 passed**.
- `cargo clippy -p outbe-e2e-harness --all-targets --features ocomp-integration -- -D warnings` —
  **passed**.

These gates cover the corrected ExEx mappings, canonical-first FullNode failure rows, post-prune
private callbacks, the production Validator late-result ordering, and the FullNode
deadline/restart/mismatch/mutation lifecycle through the production interface.

## Closure verdict

The fixed-point findings OCOMP-001 through OCOMP-005 are closed by Beads slices
`outbe-chain-1gd.1` through `.7`. Canonical record validation, embedded transitions, generation
fencing, typed eligibility, bounded retention, durable submission recovery, and production
FullNode lifecycle evidence each retain one explicit owner. The late correction in
`ocomp_exex.rs` and `embedded.rs` does not reassign the earlier PR4 ownership of
`embedded_runtime.rs` and `vote_submitter.rs`; those files implement the already-frozen runtime and
submission seams rather than the post-prune correction.

The closure claim is limited to reachable production callbacks and the audited lifecycle. The
public reducer remains strict for arbitrary unknown jobs, synthetic duplicate compute callbacks
remain fail-closed, and impossible private `VoteFinalized { success: false }` callbacks are not
made valid. Within those boundaries, exhaustive Rust matches, table-driven reducer/adapter tests,
restart tests, and the four retained release SGX scenarios cover the owned state transitions and
their production effects. No wire, ABI, storage-layout, quorum-timing, Lysis-arithmetic, or
cryptographic rule changed.

## Resolved decisions

1. Retain a typed terminal reason beside the closed local state so `Expired`, `Conflicted`, and
   `Canceled` remain distinct canonical outcomes without making either conflict or cancellation a
   local fatal.
2. Apply terminal absorption by local ownership state, not by canonical status alone. Validator
   `Completed`, FullNode `Verified`/`FatalMismatch`, and `Expired`/`Conflicted`/`Canceled` absorb
   late outcomes. FullNode `Completed` while still `Computing`/`WaitAtDeadline` must finish exact
   verification, and current-generation local failure is fatal.

## Migration and documentation impact

No wire, storage, ABI, or consensus migration is needed for the identified closures. The embedded
state/action types and ExEx tests change locally. The supervisor plan's `NoQuorum` terminology and
test claims should be corrected to cover `Expired`, `Conflicted`, and `Canceled` explicitly.

## Definition of done

- [x] Every legal canonical `OcompJobStatus` shape has one non-fatal/fatal local transition by
      explicit policy, including terminal records without `finalized`.
- [x] Terminal local states cannot be reopened by any late compute/vote/deadline/canonical event.
- [x] Late terminal outcomes cannot create a vote, fatal, or readiness regression.
- [x] Transient eligibility errors retry; exact non-membership abstains durably.
- [x] Per-tick reconciliation work is bounded by active work, not process-lifetime history.
- [x] FullNode canonical-first, deadline/resume, mismatch shutdown, and crash-window behavior are
      proven through the production interface.
- [x] Existing on-chain model/fault tests, vote journals, materialization/capacity/restart E2Es, and
      the Validator late-result E2E remain green.
