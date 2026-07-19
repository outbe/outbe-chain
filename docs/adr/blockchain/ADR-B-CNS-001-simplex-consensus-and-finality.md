# ADR-B-CNS-001: Simplex finality uses chain-bound hybrid BLS evidence and deterministic leader election

- **Status:** Proposed (documents the observed current implementation)
- **Date:** 2026-07-17
- **Scope:** `crates/blockchain/consensus`, consensus construction in `crates/blockchain/engine`, consensus-facing primitives
- **Depends on:** ADR governance, ADR-B-NOD-001
- **Related:** ADR-B-CNS-002 committee/DKG, ADR-B-CNS-003 execution bridge, ADR-B-CRY-001 cryptographic pinning

## Context

Outbe uses Commonware Simplex for BFT finality, but the production interface is
not Commonware's default configuration. Validator identity, vote attribution,
VRF material, leader selection, namespace derivation, finalized block persistence,
execution delivery and missed-proposer accounting are joined by Outbe-specific
adapters. Those choices are consensus-visible and must be one normative contract.

The production root is `outbe_engine::run_consensus_stack`
(`crates/blockchain/engine/src/stack.rs:1994`). Certified followers use a separate
stack and do not instantiate the voting engine.

## Decision

### Membership and identity

Canonical runtime membership comes only from the on-chain ValidatorSet at the
latest canonical execution state (`stack.rs:2055-2060`). Static peers are
discovery hints; they cannot add voting authority. The ordered validator set maps
the same participant index to BLS public key, EVM address and P2P address
(`primitives/src/validators.rs:12-30`). Invalid on-chain P2P data is distinguished
from missing data so a static address cannot silently replace a malformed
authoritative entry.

Every self-registered MinPk voting key requires proof of possession. Genesis- or
owner-supplied keys are an explicit trust exception documented by the root README;
an owner registration without PoP is bootstrap-only debt, not equivalent
cryptographic evidence.

### Hybrid certificate

The certificate scheme combines:

- individual MinPk same-message signatures for signer attribution and BFT voting;
- a MinSig threshold signature produced from epoch DKG material for VRF seed
  generation;
- the committee snapshot/material version needed to verify the evidence at the
  correct epoch.

The individual aggregate remains authoritative for safety. Threshold VRF material
drives leader election/fairness and does not replace signer attribution.

### Namespace and replay isolation

Production installs the chain ID once before signing or verification. The base
namespace is `b"outbe" || chain_id_be`
(`consensus/src/proof/constants.rs:34-64`). Individual vote namespaces append a
versioned commitment to the canonical Commonware participant set
(`constants.rs:111-122`), so a signature from another chain or another committee
cannot verify as the same vote.

Seed paths are chain-bound and threshold-polynomial-bound. All signing and
verification paths must call the shared namespace derivation; local reimplementation
of byte concatenation is forbidden.

### Leader election

For round `R`, election uses this ordered policy
(`consensus/src/hybrid/election.rs:101-194`):

1. verify the certificate VRF threshold proof against its actual seed round;
2. for epoch view 1, use a verified/bootstrap seed when available;
3. when a certificate exists but its VRF proof is absent/unusable, derive
   `bootstrap_seed || encoded(R)` and emit the degraded-selection metric;
4. if no usable seed exists, choose round-robin
   `(epoch + view) mod committee_size`.

Certificate seed-round recovery probes at most `u8::MAX` descending views. For a
missed-proposer recomputation using one older anchor certificate, the verified
seed is additionally bound to the elected round. The live immediately preceding
certificate path keeps its existing raw threshold-signature seed.

Election is deterministic for identical certificate, round, committee and
epoch-scoped material. Local wall-clock time and container iteration order do not
select the leader.

### Finalization persistence and delivery

The consensus stack initializes Commonware Marshal immutable archives for blocks
and finalizations before deciding whether a node may perform genesis DKG
(`engine/src/stack.rs:2295-2438`). Marshal's durable finalized height participates
in crash/genesis-formation decisions; execution height alone is insufficient.

Finalized execution delivery, Marshal acknowledgement, CE commit barriers and
projection readiness have separate owners:

- ADR-B-CNS-003 owns execution/new-payload/fork-choice delivery and acknowledgements;
- ADR-B-OCD-008 through ADR-B-OCD-013 own CE exact-parent/finalized commit;
- ADR-B-OCD-004 and ADR-B-OCD-005 own asynchronous Mongo projection and execution-read readiness;
- Marshal owns consensus block/finalization persistence.

No component may advance its durable acknowledgement past the commit boundary its
interface promises.

## Consensus FSM

The complete Simplex protocol FSM remains supplied by the pinned Commonware
version; Outbe owns the following externally observable adapter transitions:

| Current | Event | Guard | Effects | Next/error |
|---|---|---|---|---|
| Epoch starting | load committee/material | non-empty canonical set; material matches epoch | register providers/subchannels | Voting epoch |
| Voting epoch | propose | elected local leader; application resolves valid candidate | broadcast proposal | Await votes |
| Voting epoch | verify proposal | valid namespace, committee, parent, metadata and execution result | true vote | Continue round |
| Voting epoch | invalid proposal | deterministic protocol invalidity | false vote/evidence path | Continue round |
| Voting epoch | local timeout/unavailable dependency | cannot decide before local budget | no false vote; response dropped/abstain | Retry/new view |
| Voting epoch | quorum certificate/finalization | hybrid certificate verifies | persist/report/deliver finalized block | Next view or epoch fence |
| Voting epoch | leader timeout | timing from genesis/pinned defaults | advance view, record activity | Next view |
| Any active epoch | fatal actor/storage/verification invariant | classified fatal | propagate stack error | Node shutdown via ADR-B-NOD-001 |

The distinction between an invalid proposal and a local inability to decide is
normative. For example, epoch-boundary parent mismatch is invalid, while missing
local anchor/Marshal data is infrastructure error and must not become a false vote
(`application/epoch_boundary.rs:56-63`).

## Side-effect ledger

| Effect | Owner | Atomicity domain | Receipt/error | Retry/replay |
|---|---|---|---|---|
| Sign/broadcast vote | Hybrid signer + network actor | one signed consensus message | feedback/error and protocol evidence | duplicate governed by Simplex/message identity |
| Persist block/finalization | Marshal archive | Commonware storage journal/archive | durable Marshal acknowledgement | replayed from archive on restart |
| Deliver finalized block to execution | consensus/execution bridge | ADR-B-CNS-003 delivery protocol | Engine response/ACK | exact identity required |
| Publish finalization/activity | reporter/finalization actors | process-local mailboxes plus on-chain artifact path | typed messages | stale/duplicate rules owned by actor |
| Select leader | elector | pure deterministic computation | `Participant` | recomputation must be identical |
| Metrics/logging | consensus adapters | diagnostic | non-transactional | may repeat |

## Persistent state and invariants

- Marshal archives are the durable consensus-finalization authority.
- Canonical execution state is the membership authority.
- Epoch-scoped providers must contain exactly the committee, hybrid scheme and
  elector material for that epoch before messages are admitted.
- A certificate is interpreted only with the committee and namespace committed to
  its epoch; current-committee verification of historical evidence is forbidden.
- Participant ordering is canonical and shared by signing, aggregation,
  attribution and committee commitments.
- Finalized height/hash cannot regress or fork within one durable Marshal history.

Process-local caches/providers may accelerate verification but cannot manufacture
or replace missing durable evidence.

## Determinism and bounded work

- Network channels have explicit quotas/backlogs; message sizes are bounded by
  consensus configuration (`engine/src/stack.rs:2087-2123`).
- Epoch muxers isolate messages by epoch and are registered before activation to
  avoid routing races (`stack.rs:2185-2215`).
- VRF seed-round recovery is bounded to 255 views.
- Timing parameters that affect consensus live in genesis or pinned defaults, not
  per-validator CLI overrides (`engine/src/args.rs:113-117`).
- Cryptographic batch verification randomness is local verification machinery and
  cannot change the accepted result.

Any bounded truncation that changes missed-proposer accounting or evidence must be
defined by the owning economic/slashing ADR, not inferred from an implementation cap.

## Replay, concurrency and reentrancy

Consensus actors execute concurrently and communicate through bounded or
explicitly configured mailboxes. Protocol safety relies on Commonware's actor FSM
plus Outbe's epoch/committee admission gates; it is not structurally single-threaded.

Epoch muxing prevents stale messages from a prior engine instance entering a new
epoch route. Duplicate signed intent must retain one canonical message identity;
same signer/round with conflicting intent is slashable evidence, not idempotent
success. Finalization redelivery must be idempotent at downstream commit barriers.

## Security and trust assumptions

- BFT safety assumes the configured Simplex fault threshold and possession-verified
  individual voting keys.
- Threshold VRF secrecy/fairness tolerates at most the DKG threshold exposure;
  individual revealed shares reduce fairness for that validator but do not replace
  MinPk voting safety.
- Chain ID initialization and committee-bound namespaces are trusted to occur
  before any signature operation.
- Commonware's pinned protocol/storage implementation is an imported normative
  dependency; upgrades require compatibility review and ADR-B-CRY-001 evidence.

## Verification evidence

Current repository evidence includes:

- hybrid scheme, proof verifier, fingerprint and cluster tests under
  `crates/blockchain/consensus/tests`;
- timestamp/header validation tests in `crates/blockchain/node/src/consensus.rs`;
- deterministic simulated-network consensus harness;
- e2e liveness, follower, validator restart, DKG failure and governance scenarios.

This is not yet a complete G9 matrix. In particular, the relationship between
Commonware's internal FSM and every Outbe adapter failure boundary has not been
captured by one independent stateful reference model.

## Consequences

- Votes and certificates are not portable across chains or committee snapshots.
- Leader election remains deterministic when VRF material is temporarily unusable,
  preserving liveness while surfacing degraded fairness.
- Consensus participation depends on exact local execution/projection/CE readiness;
  local unavailability causes abstention rather than a dishonest negative vote.
- Historical certificate verification requires retaining/reconstructing the
  correct epoch committee and material.

## Rejected alternatives

### Threshold signature as the sole vote certificate

Rejected because it loses individual signer attribution required for activity and
slashing evidence.

### Per-node timeout CLI overrides

Rejected because heterogeneous consensus-critical timing can split behavior and
make liveness nondeterministic across validators.

### Trust static peer configuration as membership

Rejected because network discovery configuration is not canonical chain state.

### Return `false` on every local verification error

Rejected because database/network lag is not proof that a proposal is invalid and
could let local outages manufacture negative consensus votes.

## Open questions and technical debt

- `CONSENSUS_CHAIN_ID` falls back to `0` when not initialized
  (`proof/constants.rs:34-56`). Production claims initialization always precedes
  use, but the type/interface does not make an uninitialized signer impossible.
- `OnceLock` uses first-value-wins semantics. Add a production startup assertion
  that repeated initialization with a different chain ID is fatal, not silently
  accepted.
- Owner-supplied voting keys without proof of possession remain a documented
  rogue-key trust exception and need removal or a network-activation policy.
- The degraded VRF path is claimed not to be adversarially reachable in the root
  README, but this audit has not found a bounded adversarial model proving the
  claim across missing, malformed and delayed certificates.
- The 255-view seed recovery/reporting cap needs an explicit economic outcome for
  larger gaps and `cap-1/cap/cap+1` production-interface evidence.
- Commonware protocol upgrades have no checked compatibility manifest tying crate
  revision, storage codec and network activation.
- Several CodeGraph paths report no direct covering tests for production reporter,
  follower resolver and VRF material providers; private tests elsewhere must not
  be assumed to cover their effective interfaces.
- The follower resolver spawns one child task per fetch and uses an unbounded
  mailbox; cancellation/retention is currently a no-op. Resource caps and
  starvation behavior require ADR-B-TXP-001 and ADR-B-OCD-010 closure.
- No formal or independent reference model covers epoch/view changes, degraded
  election, duplicate/conflicting votes, actor restarts and downstream ACKs in one
  generated history.
- Exact linearization points for provider registration/removal at epoch change
  belong to ADR-B-CNS-001 and must be cross-checked against messages already in muxers.
- This ADR requires human acceptance before its `Proposed` status changes.
