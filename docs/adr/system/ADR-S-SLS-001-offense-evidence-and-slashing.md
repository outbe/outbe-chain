# ADR-S-SLS-001: SlashIndicator owns offense evidence and punishment intent

- **Status:** Proposed; current implementation profiled; not an architecture-conformance verdict
- **Date:** 2026-07-17
- **Owners/scope:** `crates/system/slashindicator`; miss counters, evidence
  verification/deduplication, felony decisions, slash intent and reporter reward
- **Depends on:** ADR-B-GEN-001, ADR-B-CNS-001, ADR-B-CNS-002, ADR-B-CNS-003, ADR-B-EVM-004, ADR-S-VAL-001, ADR-S-STK-001,
  ADR-S-RWD-001
- **Supersedes:** The SlashIndicator-local portions of the deleted pre-space validator aggregate

## Context

Slashing combines proof verification, a punishment policy and effects owned by two
other modules. SlashIndicator must decide whether one canonical offense deserves one
punishment; ValidatorSet owns jail/exit state and Staking owns burned value. Without
a singular offense identity and atomic receipt, retries or duplicated evidence can
compound penalties.

## Decision

SlashIndicator is the sole owner of slashable offense classification, per-epoch
proposer/voter miss counters, cumulative felony count, evidence verification and
deduplication. It produces one atomic punishment outcome that invokes typed
ValidatorSet and Staking commands. It never owns stake or committee membership
directly.

There are two offense families:

- liveness misses imported from authenticated finalized-parent/window-close data,
  escalated at configured misdemeanor/felony thresholds; and
- cryptographic Byzantine evidence: double proposal, conflicting vote families,
  invalid VRF proof, seed-partial equivocation and invalid seed partial.

External evidence submission is restricted to current `ACTIVE` validators and
charged a heavy selector-specific gas base. Consensus/lifecycle-originated miss and
Byzantine commands require internal phase capabilities rather than an EOA caller.

## Evidence identity and verification

Every evidence type has a canonical domain-separated identity binding all fields
that distinguish intent. Pair evidence normalizes payload order. Vote signatures
are verified under the historical epoch committee namespace. VRF/seed evidence
binds canonical child/Phase-1 transaction, age/epoch schedule, committee snapshot,
proposer/signer attribution and cryptographic failure class.

The dedup key is written only after complete validation and in the same EVM
transaction as jail, stake burn, reporter reward and event. Same key/same intent is
an explicit duplicate outcome; same key/different intent is fatal collision or
corruption. Evidence bytes are bounded and canonical codecs reject truncation,
trailing bytes, unsupported versions and malformed lengths.

## Persistent state and invariants

State contains configured thresholds/percentages, per-validator epoch miss counters,
cumulative felony count, permanent evidence-family guards, per-finalized-block voter
and proposer window guards, and a bounded ring for the latter guards.

```text
processed[evidence_id] => exactly one committed punishment receipt
window_slashed[fb_hash] => every canonical event in that window was applied once
misdemeanor_threshold < felony_threshold < effective epoch opportunity bound
felony_count[v] = committed felony outcomes for v
JAILED/EXITING suppresses repeated liveness felony for the same continuous fault
reporter_reward <= amount burned for the bound evidence outcome
```

Zero stored configuration currently means a compiled default, not literal zero.
Configuration validation must reject percentages above 100, unreachable thresholds
and incompatible epoch lengths before activation.

## Liveness escalation state machine

For each authenticated miss event:

```text
count := count + 1
already JAILED/EXITING -> record miss only
count % felony_threshold == 0 -> felony
else count % misdemeanor_threshold == 0 -> misdemeanor event
else -> count-only outcome
```

A felony atomically increments cumulative felony count, moves `ACTIVE -> JAILED`,
signals a reshare, burns the configured percentage of bonded plus unbonding stake,
and emits a typed result. Epoch transition resets only proposer/voter miss counters;
felony count and evidence guards persist.

Window voter/proposer commands bind the whole deterministic list to `fb_hash` and
write a single guard after every event succeeds. Duplicate entries in the canonical
missed-proposer list intentionally represent multiple skipped views; voter absentee
sets must be unique.

## Evidence-felony state machine and economics

```text
unseen evidence
  -> authenticate submitter
  -> canonical decode and cryptographic verification
  -> resolve historical signer to validator
  -> reserve evidence id
  -> jail validator
  -> slash stake/unbonding
  -> mint reporter reward = floor(slashed * reward_percent / 100)
  -> increment felony count and emit receipt
  -> processed
```

Consensus-detected Byzantine offenses use the same jail/slash core without a
reporter reward. Net supply reduction is `slashed - reporter_reward`; the reward is
new native balance after Staking burns the full slashed amount. This mint/burn policy
must be explicit in issuance accounting.

## Atomicity and side-effect ledger

Evidence guard, miss/felony counters, ValidatorSet transition, Staking burn, native
reward mint and EVM event are transactionally coupled and must share one
module-owned checkpoint or unforgeable outer transaction capability. Any error
restores the complete semantic pre-state, including the evidence guard.

Metrics and the wall-clock slashing journal are diagnostic external effects. They
currently occur before the transaction is known committed and cannot roll back;
they are not punishment receipts and must be reconciled/labeled as attempts.

The window guard is written after the loop, so its safety depends on the entire
system transaction rolling back if any element fails. This atomicity requirement
must be part of the command interface, not an assumption known only by the executor.

## Replay, retry and pruning

External duplicate evidence currently reverts. A stronger typed outcome may return
the original receipt, but must never repeat effects. Liveness window replay is a
no-op while its hash guard is retained. Evidence-family guards are not pruned in the
inspected schema; window guards are pruned after 64 finalized blocks.

Pruned window replay safety requires proof that no old `fb_hash` can re-enter either
slashing command. Consensus-originated `slash_byzantine(validator)` currently has no
offense id in its interface and therefore cannot provide exactly-once semantics.

## Determinism and bounds

Historical committee snapshots determine signature namespaces and signer identity.
Miss lists and absentee sets must be proof-bound, canonical and bounded by committee
size/view window. Evidence size/age/epoch lag and heavy gas bound cryptographic work.
Per-epoch reset is linear in its caller-supplied validator list.

All counter increments, percentage arithmetic and ring cursors need checked behavior.
Wall-clock time, logs and metrics never influence state. Evidence verifier schedules
and protocol versions must be selected only from on-chain/compiled activation state.

## Failure classification and progress

Malformed, invalid, duplicate or unauthorized external evidence is a deterministic
revert. Missing/corrupt historical snapshots, impossible canonical bindings,
dedup-key collision, counter overflow and partial cross-module effects are fatal
invariant failures in consensus paths. A valid but already punished validator needs
a named outcome: duplicate offense, distinct offense without further penalty, or
cumulative penalty; incidental status checks cannot decide economics.

Every evidence type has a terminal processed state. Every miss counter progresses
to epoch reset or threshold outcome. A permanently unverifiable historical offense
is rejected, not retained in an implicit retry queue.

## Security and compatibility

Evidence cryptography imports committee-bound namespaces and snapshot/hash formats
from consensus. Submitter ACL reduces ZeroFee DoS but does not replace gas/work caps.
Reporter self-reporting, reporter = offender and colluding evidence publication must
have explicit economics.

Evidence codecs/domains, failure classes, thresholds, percentage rounding, gas base,
age/epoch limits, guard retention and punishment order are consensus/hard-fork
surfaces. Storage changes require migration; defaults hidden behind zero slots must
remain stable until deliberately activated.

## Production-interface and architectural evidence

Inspected evidence includes `schema.rs`, `runtime.rs`, `evidence.rs`, VRF and seed
codecs/verifiers, `hooks.rs`, `precompile.rs`, Staking/ValidatorSet effects,
executor/window callers and dedicated evidence/signer-set tests. Coverage verifies
many cryptographic and dedup cases, but raw facade tests and public system functions
do not prove a closed authority seam or full rollback behavior.

The module has not passed architecture review. Closure requires a typed `OffenseId` and
`PunishmentReceipt`, one private punishment orchestrator, phase capabilities for
system offenses, module-owned checkpoint, typed dependency receipts, fail-closed
config/state validation and an independent reference model spanning epochs,
multiple validators and evidence replay.

## Consequences and rejected alternatives

Historical committee-bound verification prevents evidence replay across chains or
committees. Jailing before stake slash preserves the punished status when stake
drops below minimum. Immediate permanent removal was rejected because a live
threshold share remains accountable until reshare. Rewarding arbitrary submitters
was rejected for DoS reasons; active-validator submission is the current policy.

## Open questions and technical debt

- Close the raw mutation surface: public `slash_proposer`, `slash_voter`,
  `slash_byzantine`, reset and hook functions can be invoked by in-process callers
  without authenticated finalized evidence or a lifecycle capability.
- `slash_byzantine(validator)` has no offense id or dedup guard despite hook comments
  claiming idempotency through `evidence_processed`; repeated calls jail/slash and
  increment felony count again. Require a canonical consensus offense receipt.
- ValidatorSet's current `jail_validator` itself increments `slash_count` on replay;
  bind both modules to one offense id and one atomic punishment result.
- Liveness methods accept unregistered/arbitrary addresses and increment their
  counters until a threshold-time ValidatorSet failure. Validate membership and
  historical committee provenance before the first write.
- Window methods trust caller-supplied lists. Validate unique voter absentees and
  proof-bound missed-proposer occurrences inside the owning command.
- Replace direct raw writes/calls into ValidatorSet and Staking with typed commands
  and consumed receipts; one module must own each invariant.
- Wall-clock journal records and metrics are emitted before commit and survive
  rollback. Distinguish attempted from committed outcomes or deliver from an atomic
  on-chain outbox/event stream.
- Use checked increments for miss/felony counts and validate nonzero thresholds.
  Ordinary `+ 1` and modulo depend on implicit defaults/capacity.
- Validate slash/reward percentages at configuration time. Reward computation uses
  unchecked multiplication and its native mint is outside a documented global
  issuance-limit owner.
- Define whether distinct evidence against an already JAILED validator causes an
  additional slash, count-only outcome or duplicate. Liveness and evidence paths
  currently apply different incidental policies.
- Permanent evidence guard mappings grow without bound. Define retention that does
  not permit old evidence replay, or store a bounded authenticated offense journal.
- Prove the 64-block window-guard retention against every replay/restart path and add
  tests at 63/64/65; pruning an old guard must not re-enable punishment.
- Canonical pair evidence hash concatenates two variable byte strings without length
  prefixes. Although each current evidence has a fixed 144-byte header, make the
  encoding explicitly injective/domain-separated across evidence families.
- Evidence codecs use `Revert` for corrupt/missing historical consensus state;
  separate invalid user evidence from fatal local-state inconsistency.
- Bound/reset work and prove epoch ordering: rewards distribution and miss-based
  punishment must observe pre-reset counters, then both ValidatorSet and
  SlashIndicator resets must commit atomically.
- Add failure injection after guard reservation, jail, felony count, each Staking
  write/burn, reporter mint and event, comparing full cross-module pre-state.
- Add production-interface tests for same-key/different-intent, reversed evidence,
  submitter/offender identity cases, duplicate consensus offense, already-jailed
  distinct offense, threshold `T-1/T/T+1`, epoch reset and mixed-version schedules.
- Add an independent stateful reference model with labeled generated histories for
  misses, epochs, evidence families, duplicates, rollback, jail/unjail and pruning.
