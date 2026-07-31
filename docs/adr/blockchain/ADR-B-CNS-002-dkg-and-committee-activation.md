# ADR-B-CNS-002: Height-periodic DKG produces one chain-bound committee activation boundary

- **Status:** Proposed (documents the observed current implementation)
- **Date:** 2026-07-17
- **Scope:** DKG/epoch code in `crates/blockchain/consensus` and `crates/blockchain/engine`; activation in `crates/system/validatorset` and `crates/blockchain/evm`
- **Depends on:** ADR-B-NOD-001, ADR-B-GEN-001, ADR-B-EVM-004
- **Related:** ADR-S-VAL-001 validator lifecycle, ADR-S-TEE-001 and ADR-S-TEE-002 TEE key handoff

## Context

Validator membership changes cannot immediately replace the committee that is
currently finalizing blocks. The incoming set needs threshold material, canonical
ordering, an activation height, persisted recovery state and a boundary artifact
that both proposal verification and deterministic execution can validate. A crash
or stale process must not rerun genesis DKG or activate a locally remembered result
that the parent chain did not commit.

Outbe therefore separates:

- on-chain validator lifecycle and target-set authority;
- off-chain DKG ceremony/gossip;
- consensus boundary proposal/verification;
- begin-zone atomic activation in EVM state;
- local key/material persistence and epoch-engine restart.

## Decision

### Cadence and target authority

DKG/reshare is height-periodic. At the configured preparation boundary, the target
validator set is frozen from canonical chain state. Joins/exits do not trigger an
immediate ad-hoc ceremony. The frozen set, freeze height, planned activation
height, DKG cycle and material identifiers are committed by one
`DkgBoundaryArtifact` (`primitives/src/consensus.rs:182-235`).

An `EXITING` validator remains in the outgoing consensus committee until the
boundary activation commits. Pending/new validators do not gain signing authority
merely by participating in gossip or possessing local files.

### Genesis versus existing-chain startup

Initial genesis DKG is permitted only when all are true
(`engine/src/stack.rs:875-934`):

- local execution height is zero;
- durable Marshal finalization height is zero;
- no chain-finalized DKG boundary exists;
- genesis formation is proven against the expected peers/genesis identity;
- the local key belongs to the genesis consensus set.

Otherwise the node must recover existing material or join the live chain. Local
execution height zero by itself never proves a fresh network. Testnet-only
`force-dkg` requires `trust-el-head`, is rejected on mainnet, and for an existing
chain still requires a recovered chain boundary (`stack.rs:2033-2044, 2462-2471`).

Genesis DKG requires all configured genesis dealer logs and fails fast when the
coordinated launch is incomplete. Live reshare completes at threshold and may
record revealed individual shares for unavailable participants.

### Persisted key material

A usable local DKG result is a validated triplet:

```text
private signing share + public polynomial + canonical DKG output
```

All three files must exist or all must be absent. Partial presence is fatal, and
the loaded triplet is cryptographically cross-validated
(`engine/src/stack.rs:6354-6395`). Pending and finalized triplets use distinct file
names. Key storage backends are plaintext (development), encrypted
AES-256-GCM/Argon2id, or OS keychain (`consensus/src/bls.rs:35-48`).

A local dealer persists a ceremony-bound random seed before publishing its first
bundle and journals cryptographically accepted player ACKs. Restart reconstructs
the byte-identical transcript and targets only players whose ACK is absent. Raw
secret files use owner-only temporary files, file sync, atomic rename and parent
directory sync for plaintext, encrypted envelopes and keychain markers. Keychain
markers name content-versioned entries, so the old marker remains valid until the
replacement marker commits. Dealer retry state is retained through the pending
boundary and removed only after canonical activation/promotion.

The keys directory is separate from consensus archives so routine data snapshots
do not overwrite per-validator key material. Migration from the legacy location
is process-owned startup behavior (ADR-B-NOD-001).

### Ceremony and boundary ownership

The DKG mailbox owns transient ceremony state, dealer-log gossip, one pending
boundary, one execution-committed boundary receipt and a bounded parent-status
cache (`consensus/src/dkg_manager.rs:143-159`).

Ceremony completion records `pending_boundary` and invalidates any prior committed
receipt/cache (`dkg_manager.rs:456-464`). Recovery may restore the same pending
boundary. A pending artifact is only a local expectation: proposal/verification
must also derive whether emission is required or duplicate from canonical parent
ancestry (`dkg_manager.rs:475-480`).

After deterministic execution commits the matching boundary, the manager records
a commit receipt. `take_committed_boundary_artifact` consumes local pending state
only when the committed artifact is byte-equal (`dkg_manager.rs:490-500`). Local
memory cannot authorize activation.

### Activation

The boundary block's begin-zone handler atomically:

- validates outgoing and incoming committee snapshots;
- activates the frozen ValidatorSet result;
- stores historical snapshot/material required to verify both epochs;
- applies associated boundary facts governed by the active artifact version;
- emits the committed activation outcome.

The execution checkpoint is the rollback owner. Any critical sub-effect failure
invalidates the block; partial committee activation is forbidden.

`ApplicationEpochFence` prevents the outgoing Simplex epoch from submitting an
execution candidate above the planned activation height and rejects stale/future
epochs (`application/epoch_boundary.rs:118-174`). The first proposal of a new
epoch must resolve its continuity parent from the finalized anchor and Marshal;
parent mismatch is proposal invalidity, while missing/corrupt local infrastructure
is an error/abstention, not a false vote.

## DKG and activation FSM

| Current | Event | Guard | Effects | Next/error |
|---|---|---|---|---|
| No local material at genesis | start initial DKG | complete genesis-formation proof; local genesis member | all-member ceremony | Ceremony running or fatal startup |
| Existing chain, no usable local share | start/restart | chain history exists | sync/follow and live-join recovery | Non-signing join required |
| Idle outgoing epoch | preparation height | canonical target frozen; no conflicting cycle | start threshold ceremony and gossip | Ceremony running |
| Ceremony running | valid dealer/player material | canonical participant/round/proof checks | accumulate deterministic output | Running |
| Ceremony running | threshold/all-genesis completion | output validates | persist pending triplet; create boundary artifact | Boundary pending |
| Ceremony running | timeout/insufficient live threshold before VRF deadline | frozen target unchanged | keep outgoing committee live | Retry same frozen target |
| Ceremony running | timeout at or after VRF deadline | finalized height reached deadline | mark VRF expired and terminate consensus stack | Fatal/operator recovery |
| Boundary pending | proposal before activation | parent ancestry says boundary not due | do not emit | Pending |
| Boundary pending | activation proposal | exact artifact and parent-chain due status | carry artifact; arm epoch fence | Await execution commit |
| Await execution commit | begin-zone succeeds | all activation guards/sub-effects succeed | atomically activate/store snapshots/events | Boundary committed |
| Await execution commit | any critical failure | none | rollback entire block | Pending/retry or fatal invariant |
| Boundary committed | local commit receipt observed | byte-equal pending artifact | promote pending files/material; restart epoch providers | New epoch active |
| Any | stale/different artifact replay | epoch/hash/ancestry mismatch | reject | Unchanged |

No nonterminal state may silently wait forever. Genesis is fail-fast/operator
restart. A live reshare keeps the outgoing committee live and retries the same
frozen target while its VRF window remains valid. If the ceremony times out at
or after the published VRF deadline, the supervisor reads the authoritative
finalized height and terminates the consensus stack instead of starting another
retry. This is bounded fail-closed behavior, not a forfeiture transition: the
protocol does not currently remove an unavailable frozen-target participant or
compute a replacement target automatically.

## Persistent state and invariants

- Chain ValidatorSet is the membership authority; DKG output cannot invent members.
- Frozen target ordering, target hash and committee snapshot ordering are identical.
- `planned_activation_height > freeze_height` and cycle/epoch identifiers are
  monotonic under the configured cadence.
- Pending/finalized key triplets are complete and internally consistent.
- At most one locally pending boundary and one matching committed receipt exist.
- Incoming committee activation and both outgoing/incoming snapshot writes share
  one execution checkpoint.
- Historical evidence is verified with its epoch snapshot, never the current set.
- Old epoch cannot execute above the armed boundary; new epoch cannot execute
  before activation and continuity-parent resolution.
- Revealed-share records are canonical evidence and require operator key rotation;
  they are not silently erased by later local recovery.

## Side-effect ledger

| Effect | Owner | Atomicity domain | Receipt/error | Retry/idempotency |
|---|---|---|---|---|
| Dealer/player gossip | ceremony/network actors | externally asynchronous | validated message/feedback | duplicate gossip canonicalized by ceremony |
| Persist pending/final DKG files | engine key persistence | local filesystem triplet protocol | propagated error + validated reload | incomplete triplet fails closed |
| Publish boundary artifact | consensus application | block proposal/certificate | artifact hash and finalized chain evidence | duplicate classified from parent ancestry |
| Activate validator set/snapshots | ValidatorSet begin-zone hook | EVM execution checkpoint | committed receipt/events or block error | block replay deterministic |
| Promote local pending share | engine DKG manager | filesystem + matching execution receipt | exact artifact equality | same committed artifact consumed once locally |
| Register epoch providers/routes | engine/consensus stack | process-local epoch transition | actor/provider construction result | rebuilt from durable chain/material on restart |
| Revealed-share warning/metric | DKG/reporting | diagnostic plus canonical artifact evidence | log/metric | may repeat; artifact is authority |

Filesystem key persistence and on-chain activation are different atomicity domains.
Safety comes from refusing to use local material as authority and validating it
against the chain boundary, not from pretending they commit in one transaction.

## Determinism and bounded execution

- Target selection and ordering derive from canonical on-chain state.
- DKG channel is muxed by cycle/round, and next-epoch consensus subchannels are
  pre-registered before activation to close early-message races.
- Boundary artifact encoding/hash and committee commitment are canonical and
  versioned.
- Boundary-status cache is bounded to 1024 parent entries; cache eviction cannot
  change the recomputed result.
- Consensus-critical cadence/timeouts originate from genesis/pinned configuration,
  not per-node business choices.

## Replay, concurrency and crash recovery

Ceremony actors, application verification, EVM execution and filesystem
persistence overlap. Their linearization points are intentionally separate:

- ceremony result becomes locally pending when output is validated/persisted;
- network authority changes only when the boundary block commits;
- local signing authority changes only after the matching committed boundary is
  observed and material is installed for the new epoch.

Crash recovery inspects execution height/hash, Marshal finalized height, chain
boundary evidence and complete key triplets. It must reject partial files,
different artifacts, stale epoch material and “fresh genesis” inference from an
empty local execution DB connected to a live network.

## Security and trust assumptions

- DKG cryptographic security depends on the pinned Commonware scheme and threshold.
- Genesis all-member liveness is an explicit coordinated-launch assumption.
- Live reshare tolerates unavailable members only up to threshold; revealed shares
  degrade that member's VRF secrecy and require rotation.
- Local key backend/filesystem confidentiality is an operator responsibility;
  plaintext is development-only.
- Testnet disaster-recovery flags are not a production recovery protocol.
- TEE offer-key DKG/handoff shares network cadence but has distinct protocol and
  registry contracts in ADR-S-TEE-001 and ADR-S-TEE-002.

## Verification evidence

Current evidence includes:

- DKG manager ceremony, duplicate, cache and boundary verification tests;
- persisted triplet completeness/validation and pending-boundary recovery tests in
  engine stack tests;
- ValidatorSet atomic boundary/snapshot rollback tests;
- epoch fence boundary/stale/future tests;
- Rust e2e scenarios for join/activation, restart with persisted share, stalled
  reshare liveness/recovery, permanent frozen-target loss through VRF-expiry,
  exit/reshare-down and stale-join readiness.

The full 2026-07-17 Rust e2e run passed 12 scenarios/83 steps, including these
paths. That does not prove every crash point or byzantine interleaving.
The focused 2026-07-20 permanent-loss regression passed once with mock SGX and
three consecutive times with hardware SGX. It asserts continued finalization by
the outgoing four-member committee, no partial 4-to-5 activation, arrival at the
published expiry height and process termination with the frozen-target expiry
error on every surviving validator.

## Consequences

- Validator lifecycle changes have bounded activation points and cannot mutate the
  live committee mid-epoch.
- Ceremony failure does not immediately halt the existing committee.
- Local key loss removes one validator's ability to sign but does not authorize a
  new network DKG.
- Historical committee snapshots/material become protocol state required for proof
  verification and slashing/accounting.

## Rejected alternatives

### On-demand reshare after every join/exit

Rejected because it makes membership activation timing dependent on transaction
arrival and overlapping ceremonies.

### Activate from local DKG completion

Rejected because validators can complete/observe ceremonies at different times;
only a finalized boundary can change network authority.

### Treat any empty datadir as fresh genesis

Rejected because a new node joining a live network could start a conflicting DKG.

### Store only a private share

Rejected because the share, polynomial and canonical output must be validated as
one material set for signing and verification parity.

## Open questions and technical debt

- Ceremony timeout and frozen-target retry remain spread across engine control
  flow/config rather than expressed as one closed typed FSM. The VRF-expiry
  fail-closed boundary is tested, but the protocol-level forfeiture/replacement
  transition for a permanently unavailable frozen-target participant is not
  defined.
- Multi-file DKG triplet replacement still needs one manifest/generation commit
  point and fault injection across every file boundary. Each individual secret
  file is atomically replaced, but several files do not form one filesystem
  transaction.
- OS-keychain replacement can leave an unreachable content-versioned orphan after
  a crash. This is safe for startup but needs operator garbage collection and a
  backend integration test on supported Secret Service implementations.
- Pending and committed boundary state is process-local `Mutex` state; recovery
  reconstructs it from files/chain, but a complete crash matrix for every
  pre/post-activation write boundary is missing.
- Boundary manager methods are publicly callable within the crate graph and accept
  rich artifacts; a module audit must confirm no caller can forge commit status or
  skip parent-ancestry classification.
- Cache correctness assumes recomputation after eviction; add property tests
  comparing cached/uncached boundary classification over reorg-like parent sets.
- Revealed-share rotation is an operator warning/metric, not an enforced on-chain
  lifecycle transition. Decide whether a revealed validator must become pending,
  jailed or key-rotation-required.
- Testnet `trust-el-head/force-dkg` is disaster recovery without a production
  equivalent. ADR-B-OCD-008 must define snapshots/bootstrap and explicitly delete or
  retain these flags before mainnet.
- Key backend migration and passphrase/keychain failure behavior are not covered by
  end-to-end restart tests.
- TEE re-registration fields exist in the boundary artifact, while the root README
  says production generation remains dormant. ADR-S-TEE-001 and ADR-S-TEE-002 must reconcile carried
  schema, validation gates and actual producer reachability.
- Epoch provider registration/removal and already-buffered messages need a
  generated concurrency history or bounded model beyond unit fence tests.
- The application epoch fence recovers poisoned mutex state by taking the inner
  value. Prove poisoning cannot coincide with a partially applied logical update,
  or make updates panic-free by construction.
- There is no single stateful reference-model test spanning validator lifecycle,
  frozen target, DKG result, boundary block, rollback, epoch restart and duplicate
  replay.
- This ADR requires human acceptance before its `Proposed` status changes.
