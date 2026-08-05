# PFS-006: Validator joins, operates and leaves or is punished

- **Status:** Draft
- **Actors:** validator operator, ValidatorSet, Staking, consensus/DKG, Rewards,
  SlashIndicator, Cycle and claimant
- **Trigger:** operator registers a validator identity and self-stakes, or an active
  validator accumulates an exit/punishment condition
- **Topology/services:** multi-validator network with DKG, finalized-parent Phase 1,
  native fee escrow and configured validator predeploys
- **Referenced ADRs:** ADR-B-NOD-001, ADR-B-GEN-001, ADR-B-CNS-001, ADR-B-CNS-002, ADR-B-CNS-003,
  ADR-S-CYC-001, ADR-S-VAL-001, ADR-S-STK-001, ADR-S-RWD-001,
  ADR-S-SLS-001, ADR-S-KEY-001, ADR-S-ACC-001, ADR-S-EMI-001
- **Supersedes:** The deleted pre-space validator lifecycle aggregate narrative

## Outcome

A validator moves through one unambiguous economic and consensus lifecycle. It
cannot vote before syncing and receiving a DKG share; its finalized participation
is compensated once; voluntary exit or a unique offense removes it at a reshare;
and bonded value becomes claimable only after the correct delay and slash effects.

## Acceptance contract

- **Source:** Validator operator, consensus accounting or authenticated offense reporter.
- **Trigger:** An operator registers and self-stakes, or an active validator requests exit or accumulates canonical punishment evidence.
- **Environment:** Multi-validator finalizing network with DKG, verified parent accounting, fee escrow and configured validator predeploys.
- **Canonical inputs:** EOA/BLS identity and proof, optional versioned P2P information, stake/claims, readiness/finalized head, DKG target/artifact, committee/accounting snapshots, participation/fee metadata and unique offense identity.
- **System under test:** ValidatorSet, Staking, consensus/DKG, Rewards, SlashIndicator, Cycle and claim settlement.
- **Expected response:** Validator/staking statuses, committee/share snapshots, participation/reward receipts, unbonding payouts, or jail/slash/reporter-reward records.
- **Response measures:** Only ready validators with valid shares become active; participation and offenses settle once; exit/punishment removes the validator at the next committee transition; bonded value, claims, fees and slash/reward deltas conserve.
- **Failure guarantee:** Failed DKG or replay leaves no partial committee, membership, share, reward, claim, jail or slash effect; restart resumes solely from committed state.

## Preconditions and canonical inputs

- The validator controls its EOA and BLS key. P2P information defaults to unset;
  when present, its version and payload decode atomically.
- Pre-registration stake is rejected; stake-bearing lifecycle states always have a
  registered identity and consensus key.
- ValidatorSet capacity and permissionless registration cap permit admission.
- Staking/ValidatorSet configuration, DKG schedule and protocol versions agree on
  every node.
- Certified-parent metadata and committee snapshots are verified before accounting.
- Punishment evidence has a canonical unique offense identity.

## Effective typed states and ABI compatibility

The Rust lifecycle uses richer states than the persisted Solidity-compatible
`uint8 status`. Two effective states may therefore share one raw ABI tag:

| Effective Rust state | ABI status | Canonical coupled state |
|---|---|---|
| `Absent` | no persisted record | no dense registry entry and no validator-owned residue |
| `WaitingForStake` | `REGISTERED = 0` | registered identity; stake below minimum; no share |
| `WaitingForReadiness` | `PENDING = 1` | minimum stake; readiness false; no share |
| `Joining` | `PENDING = 1` | readiness true; eligible for the next target; no share |
| `Active` | `ACTIVE = 2` | member of the current committee with a live share |
| `Exiting` | `EXITING = 3` | current member retaining its live share until exclusion |
| `Unbonding` | `UNBONDING = 4` | outside consensus while stake and claims settle |
| `Inactive` | `INACTIVE = 5` | no bonded stake, live claim or share; retained through cooldown |
| `JailRetained` | `JAILED = 6` | jailed current member retaining its live share |
| `Jail` | `JAILED = 6` | jailed and excluded from the current committee; no share |

An address without a dense registry entry is represented by the derived `Absent`
state and has no persisted status byte. P2P information is optional metadata on
registered states, not a lifecycle gate.
`ACTIVE` without a share, `PENDING` with a share and `EXITING` without a share are
invalid coupled states and fail closed rather than becoming recovery variants.

## Success sequence: join and operation

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | ValidatorSet | self-register with BLS proof and optional P2P information | `WaitingForStake` (`REGISTERED`), identity indexes |
| 2 | Staking | receive self-stake and reach minimum | bonded ledger; `WaitingForReadiness` (`PENDING`) |
| 3 | node/operator | sync to finalized head and confirm readiness | `Joining` (`PENDING`) and set-change signal |
| 4 | consensus/DKG | freeze canonical reshare target and complete ceremony | validated DKG artifact |
| 5 | ValidatorSet boundary | atomically snapshot committees and activate share | `Active` (`ACTIVE`), share, set hash |
| 6 | consensus/Rewards | record verified finalized participation and escrow fees | fingerprint/participation/escrow |
| 7 | Rewards window close | pay fee shares, burn/dispatch residue once | native deltas and settled guard |
| 8 | Cycle/Rewards | allocate daily emission top-up as Gems | Gem receipts and day guards |

## Success sequence: voluntary exit

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | validator/Staking | unstake below minimum or request deactivation | claim; `Exiting` (`EXITING`) retaining its share; set-change signal |
| 2 | consensus/DKG | form next committee without exiting validator | reshare artifact |
| 3 | ValidatorSet boundary | atomically clear share and move `Exiting -> Unbonding` | committee snapshots/status |
| 4 | Staking lifecycle | move residual bonded value into delayed claim | zero bonded; claim maturity |
| 5 | validator | claim every matured entry | native transfer; consumed claims |
| 6 | Staking/ValidatorSet | when no bonded/live claim remains, mark `INACTIVE` | terminal status |

## Success sequence: punishment and recovery

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | SlashIndicator | authenticate unique evidence or threshold miss | offense id/counter |
| 2 | ValidatorSet | jail while retaining current live-share accountability | `JailRetained` (`JAILED`), jailed height |
| 3 | Staking | burn exact configured bonded/unbonding fraction | slash amount and conservation |
| 4 | SlashIndicator | optionally mint bounded reporter reward | punishment receipt |
| 5 | consensus/DKG | atomically exclude jailed validator and clear share | `Jail` (`JAILED`), new committee snapshot |
| 6a | validator | top up, wait cooldown, unjail and re-confirm | `WaitingForReadiness -> Joining`, then new DKG activation |
| 6b | validator | after exclusion, fully unstake instead | `Jail -> Unbonding -> Inactive` |

`JailRetained` cannot leave the accountable committee in the middle of an epoch.
Only the boundary may turn it into `Jail`. A fully unstaked `Jail` moves directly
to `Unbonding`; a partial jailed unstake leaves it jailed.

Both jailed states retain the complete stored history shape. This is necessary
because finalized-parent accounting may record a historical missed vote after the
validator has already been excluded. The exclusion boundary clears the old
committee's missed-block/vote slate; unjail clears those two counters again so any
late historical accounting cannot become miss debt in the validator's rejoin cycle.
Joined/deactivated heights, slash count and proposal history are preserved.

## Boundaries and conservation

Registration, stake, readiness, unstake, claim, unjail and external evidence are
separate user transactions. DKG boundary activation, certified-parent accounting,
late settlement, epoch reset and Cycle daily dispatch are ordered system
transactions. Each row's multi-module effects share one explicit checkpoint.
The finalized boundary is an internal consensus/system command; an owner or manual
`activateResharedSet` call is not an independent membership authority.

```text
Staking native balance = bonded total + live unbonding claims
one offense id          = at most one punishment receipt
one finalized hash      = one economic fingerprint and one miss window
ACTIVE voter            => canonical live DKG share
EXITING validator       => canonical live DKG share until boundary exclusion
no live share           => cannot sign/vote as current participant
fee escrow              = native payouts + burned residue
```

## Observable completion contract

ABI reads show correct status/stake/P2P identity; consensus status includes the
validator only after boundary activation; committee snapshots and active-set hash
agree; receipts and finalized metadata show participation; Rewards settlement and
Gem state reconcile; exit/punishment clears the validator from the next committee;
claim transfers exact matured value; duplicate evidence/replay changes nothing.

Submitted transaction hashes are not completion evidence. Every assertion must
distinguish executed, finalized and observed committee/economic state.

## Replay, retry, restart and partial failure

Registration duplicates reject except explicit inactive re-registration after the
same cooldown required for cleanup. Cleanup cannot erase an inactive record early
and thereby bypass that cooldown. DKG
artifact retry either commits the same boundary once or restores all snapshots and
membership. Metadata replay is fingerprint-idempotent. Fee/day settlement and
offense processing use intent-bound guards. Restart reconstructs pending set change,
DKG ceremony, unsettled escrows, epoch counters and unbonding claims solely from
committed state. A partial cross-module result is never accepted.

## E2E scenario matrix

| Id | Scenario | Given / canonical inputs | When / trigger | Then / outputs and postconditions | Verification |
|---|---|---|---|---|---|
| PFS-006-01 | join and activate | 4 validators plus registered/staked/synced joiner | move `WaitingForReadiness -> Joining` and complete reshare | `Active` (`ACTIVE`) with canonical share/set hash; committee agrees | `@pfs-006-01` live-node |
| PFS-006-02 | stale join guard | joiner in `WaitingForReadiness` | reshare boundary passes, then confirm and retry | stays `WaitingForReadiness`/no share first; becomes `Joining` and activates only on a later reshare | `@pfs-006-02` live-node |
| PFS-006-03 | voluntary exit and claim | active validator with bonded stake | deactivate, reshare, mature and claim | excluded, UNBONDING→INACTIVE; exact value claimed once; unauthorized exit/claim rejected | `@pfs-006-03` live-node with exact claim/value accounting and caller isolation |
| PFS-006-04 | DKG failure/recovery and bounded permanent loss | frozen 4→5 target with ceremony quorum removed | either restore the validator before expiry or leave the required players offline | recovery path keeps the old committee live and retry reaches 5; permanent-loss path never partially activates, finalizes through the published VRF deadline, then all surviving validators terminate fail-closed | two `@pfs-006-04` live-node scenarios; permanent-loss path passed mock once and hardware SGX 3/3 on 2026-07-20 |
| PFS-006-05 | fee and late-voter settlement | finalized participation/escrow with delayed vote evidence | close settlement window | payouts plus burned residue equal escrow exactly once | documentation-only: fee-enabled genesis/metadata control absent |
| PFS-006-06 | downtime felony | active validator crosses configured miss threshold | kill validator and process offense | one jail/slash with exact bonded/burn/supply deltas; continued downtime cannot punish twice; chain remains live | `@pfs-006-06` live-node |
| PFS-006-07 | duplicate evidence | one authenticated offense already processed | resubmit same canonical evidence | no second punishment/reporter reward | documentation-only: evidence construction/submission absent |
| PFS-006-08 | unjail and rejoin | validator is `Jail` after exclusion, topped up and cooldown elapsed | unjail, confirm and reshare | `WaitingForReadiness -> Joining -> Active` with a fresh share; no stale share reuse | documentation-only: slashing/time control absent |
| PFS-006-09 | crash boundaries | operation poised at registration, in-flight DKG, completed-DKG/pre-activation, active-share and reshare checkpoints | crash node/enclave or full committee and restart | committed state is recovered; no premature/duplicate activation; sealed state and finalization survive | six `@pfs-006-09` live-node scenarios |
| PFS-006-10 | cleanup and re-registration | cooldown-expired inactive validator with no bonded/live claims | re-register in place, or clean indexes and then register again | neither path bypasses cooldown; no stale pubkey/index; exactly one live record | documentation-only: maturity/cleanup fixture absent |

## Open questions and technical debt

- Replace direct raw cross-module writes with typed command/receipt seams before
  treating this flow as Accepted.
- Define a chain-authoritative forfeiture/replacement transition if the network
  must recover automatically from a permanently unavailable member of an
  already-frozen DKG target. Current behavior intentionally stops at VRF expiry.
- Define one durable intent identity for DKG activation and every punishment.
- Define exact restart ownership for in-flight DKG and overdue Rewards/unbonding work.
- Implement the remaining Rewards settlement, externally submitted duplicate
  evidence, unjail and re-registration scenarios.
- Add a mixed-version topology proving storage/evidence/committee-format activation.
- Reconcile external diagnostic journal entries with committed on-chain receipts.
