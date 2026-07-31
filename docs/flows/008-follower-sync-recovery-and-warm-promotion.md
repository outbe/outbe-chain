# PFS-008: Followers synchronize, recover and warm-promote safely

- **Status:** Draft
- **Actors:** Committee validator, cold follower, chained follower and validator operator
- **Trigger:** Operator starts a follower from an upstream node or restarts/promotes a synchronized node
- **Topology/services:** Four-validator TEE localnet plus two non-voting follower slots
- **Referenced ADRs:** ADR-B-NOD-001, ADR-B-CNS-001, ADR-B-CNS-002, ADR-B-CNS-003, ADR-B-OPS-001, ADR-S-VAL-001, ADR-S-STK-001
- **Supersedes:** None

## Outcome

Non-voting nodes catch up through direct or chained upstreams, a restarted
validator re-locksteps without corrupting consensus state, and a synchronized
follower can reuse its durable chain data when promoted through the normal
stake/readiness/DKG path.

## Acceptance contract

- **Source:** Node/validator operator.
- **Trigger:** Launch, restart or warm-promote a node using an explicit upstream and durable data directory.
- **Environment:** Four validators finalizing through a reshare, mock TEE available, follower and joiner ports isolated.
- **Canonical inputs:** Upstream endpoint, finalized head, node data directory, joiner identity/key/stake and readiness confirmation.
- **System under test:** Node follower mode, upstream synchronization, consensus catch-up, durable database reuse, ValidatorSet/Staking and DKG activation.
- **Expected response:** Followers and restarted validator reach lockstep; warm-promoted node becomes a consensus participant and remains caught up.
- **Response measures:** Each tested node is within four blocks of committee head; follower never votes before activation; promoted node appears as participant and stays in lockstep.
- **Failure guarantee:** Sync/restart/promotion never rewinds committee state, duplicates identity, activates stale data or makes an unconfirmed follower vote.

## Preconditions and canonical inputs

- Committee has crossed a reshare and exposes a non-zero VRF material version.
- Upstreams publish finalized progress and accept follower connections.
- Warm promotion uses the follower's stopped, synchronized datadir and a distinct registered joiner identity.

## Success sequence

| Step | Owner                    | Command/effect                             | Durable evidence         |
| ---: | ------------------------ | ------------------------------------------ | ------------------------ |
|    1 | operator                 | start cold follower from committee         | follower finalized head  |
|    2 | operator                 | start second follower from first           | chained follower head    |
|    3 | operator                 | kill/restart validator mid-epoch           | validator catches up     |
|    4 | operator                 | stop follower and reuse datadir for joiner | durable database move    |
|    5 | Staking/ValidatorSet/DKG | stake, confirm and reshare                 | participant status/share |

## Boundaries and conservation

Follower synchronization changes only local node data. Stake/readiness and DKG
activation are separate on-chain/consensus boundaries. Warm promotion may reuse
chain data but never a validator identity or threshold share.

## Observable completion contract

RPC heads and consensus status prove lockstep/non-participation/participation.
Promotion completes only after active-set membership, not after process launch.

## Replay, retry, restart and failure

Restart repeats synchronization idempotently. Reusing a stale or live datadir must
fail safely. Repeated readiness/stake follows their owning idempotency rules; DKG
failure retains the old committee as specified by PFS-006.

## E2E scenario matrix

| Id         | Scenario                                  | Given / canonical inputs                                | When / trigger                                                                        | Then / outputs and postconditions                                                                     | Verification                     |
| ---------- | ----------------------------------------- | ------------------------------------------------------- | ------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- | -------------------------------- |
| PFS-008-01 | cold follower sync                        | reshared committee and empty follower data              | launch from committee upstream                                                        | follower reaches ≤4-block lockstep and remains non-voting                                             | live `follower_upstream.feature` |
| PFS-008-02 | chained follower sync                     | first follower in lockstep                              | launch second with first as upstream                                                  | second reaches committee lockstep                                                                     | same live composite scenario     |
| PFS-008-03 | validator restart catch-up                | active validator mid-epoch                              | kill, wait and restart                                                                | validator returns to lockstep without committee rewind                                                | same live composite scenario     |
| PFS-008-04 | warm promotion                            | stopped synchronized follower and fresh joiner identity | reuse datadir, stake, launch, confirm                                                 | joiner activates through DKG and stays in lockstep                                                    | same live composite scenario     |
| PFS-008-05 | upstream loss and switch                  | synchronized follower with one upstream                 | disconnect upstream while committee advances, then restart against a healthy upstream | no unverified finalized progress while isolated; durable catch-up restores exact hash/root parity     | `@pfs-008-05` live-node          |
| PFS-008-06 | restart during warm promotion             | synchronized follower promoted from its durable datadir | restart promoted node and enclave around the planned activation boundary              | no premature participation; activation occurs only at the planned boundary with sealed-state recovery | `@pfs-008-06` live-node          |
| PFS-008-07 | active-validator restart during promotion | promotion/DKG in progress                               | restart one active validator around the same boundary                                 | old committee remains authoritative until activation; final network state converges                   | `@pfs-008-07` live-node          |
| PFS-008-08 | duplicate readiness/promotion intent      | warm candidate already submitted readiness              | resubmit readiness before restart/activation                                          | command is idempotent; exactly one activation and canonical committee result                          | `@pfs-008-08` live-node          |

## Open questions and technical debt

- Split or tag the composite feature so all four implemented examples retain stable traceability without multiplying expensive setup unnecessarily.
- Check explicit finalized-hash/state-root equality and retain the four-block height tolerance.
- Define upstream failover, retention floor and datadir compatibility policy.
