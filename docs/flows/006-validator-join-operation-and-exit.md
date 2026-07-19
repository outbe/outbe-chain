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
- **Canonical inputs:** EOA/BLS/P2P identity and proof, stake/claims, readiness/finalized head, DKG target/artifact, committee/accounting snapshots, participation/fee metadata and unique offense identity.
- **System under test:** ValidatorSet, Staking, consensus/DKG, Rewards, SlashIndicator, Cycle and claim settlement.
- **Expected response:** Validator/staking statuses, committee/share snapshots, participation/reward receipts, unbonding payouts, or jail/slash/reporter-reward records.
- **Response measures:** Only ready validators with valid shares become active; participation and offenses settle once; exit/punishment removes the validator at the next committee transition; bonded value, claims, fees and slash/reward deltas conserve.
- **Failure guarantee:** Failed DKG or replay leaves no partial committee, membership, share, reward, claim, jail or slash effect; restart resumes solely from committed state.

## Preconditions and canonical inputs

- The validator controls its EOA, BLS key and valid versioned P2P address.
- ValidatorSet capacity and permissionless registration cap permit admission.
- Staking/ValidatorSet configuration, DKG schedule and protocol versions agree on
  every node.
- Certified-parent metadata and committee snapshots are verified before accounting.
- Punishment evidence has a canonical unique offense identity.

## Success sequence: join and operation

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | ValidatorSet | self-register with BLS proof and P2P identity | `REGISTERED`, identity indexes |
| 2 | Staking | receive self-stake and reach minimum | bonded ledger; `PENDING` |
| 3 | node/operator | sync to finalized head and confirm readiness | readiness flag and set-change signal |
| 4 | consensus/DKG | freeze canonical reshare target and complete ceremony | validated DKG artifact |
| 5 | ValidatorSet boundary | atomically snapshot committees and activate share | `ACTIVE`, share, set hash |
| 6 | consensus/Rewards | record verified finalized participation and escrow fees | fingerprint/participation/escrow |
| 7 | Rewards window close | pay fee shares, burn/dispatch residue once | native deltas and settled guard |
| 8 | Cycle/Rewards | allocate daily emission top-up as Gems | Gem receipts and day guards |

## Success sequence: voluntary exit

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | validator/Staking | unstake below minimum or request deactivation | claim; `EXITING`; set-change signal |
| 2 | consensus/DKG | form next committee without exiting validator | reshare artifact |
| 3 | ValidatorSet boundary | clear share and move `EXITING -> UNBONDING` | committee snapshots/status |
| 4 | Staking lifecycle | move residual bonded value into delayed claim | zero bonded; claim maturity |
| 5 | validator | claim every matured entry | native transfer; consumed claims |
| 6 | Staking/ValidatorSet | when no bonded/live claim remains, mark `INACTIVE` | terminal status |

## Success sequence: punishment and recovery

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | SlashIndicator | authenticate unique evidence or threshold miss | offense id/counter |
| 2 | ValidatorSet | jail while retaining current live-share accountability | `JAILED`, jailed height |
| 3 | Staking | burn exact configured bonded/unbonding fraction | slash amount and conservation |
| 4 | SlashIndicator | optionally mint bounded reporter reward | punishment receipt |
| 5 | consensus/DKG | exclude jailed validator and clear share | new committee snapshot |
| 6a | validator | top up, wait cooldown, unjail and re-confirm | `PENDING`, then new DKG activation |
| 6b | validator | fully unstake instead | `EXITING -> UNBONDING -> INACTIVE` |

## Boundaries and conservation

Registration, stake, readiness, unstake, claim, unjail and external evidence are
separate user transactions. DKG boundary activation, certified-parent accounting,
late settlement, epoch reset and Cycle daily dispatch are ordered system
transactions. Each row's multi-module effects share one explicit checkpoint.

```text
Staking native balance = bonded total + live unbonding claims
one offense id          = at most one punishment receipt
one finalized hash      = one economic fingerprint and one miss window
ACTIVE voter            => canonical live DKG share
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

Registration duplicates reject except explicit inactive re-registration. DKG
artifact retry either commits the same boundary once or restores all snapshots and
membership. Metadata replay is fingerprint-idempotent. Fee/day settlement and
offense processing use intent-bound guards. Restart reconstructs pending set change,
DKG ceremony, unsettled escrows, epoch counters and unbonding claims solely from
committed state. A partial cross-module result is never accepted.

## E2E scenario matrix

| Id | Scenario | Given / canonical inputs | When / trigger | Then / outputs and postconditions | Verification |
|---|---|---|---|---|---|
| PFS-006-01 | join and activate | 4 validators plus registered/staked/synced joiner | confirm readiness and complete reshare | ACTIVE with canonical share/set hash; committee agrees | `@pfs-006-01` live-node |
| PFS-006-02 | stale join guard | staked joiner not readiness-confirmed | reshare boundary passes, then confirm and retry | stays PENDING/no share first; activates only on later reshare | `@pfs-006-02` live-node |
| PFS-006-03 | voluntary exit and claim | active validator with bonded stake | deactivate, reshare, mature and claim | excluded, UNBONDING→INACTIVE; exact value claimed once | `@pfs-006-03` covers exclusion only; claim/value gap |
| PFS-006-04 | DKG failure/recovery | frozen 4→5 target with ceremony quorum removed | stall then restore validator | old committee remains live; no partial activation; retry reaches 5 | `@pfs-006-04` live-node |
| PFS-006-05 | fee and late-voter settlement | finalized participation/escrow with delayed vote evidence | close settlement window | payouts plus burned residue equal escrow exactly once | documentation-only: fee-enabled genesis/metadata control absent |
| PFS-006-06 | downtime felony | active validator crosses configured miss threshold | kill validator and process offense | one jail/slash and next committee exclusion while chain remains live | `@pfs-006-06` covers liveness only; slash disabled |
| PFS-006-07 | duplicate evidence | one authenticated offense already processed | resubmit same canonical evidence | no second punishment/reporter reward | documentation-only: evidence construction/submission absent |
| PFS-006-08 | unjail and rejoin | jailed validator topped up and cooldown elapsed | unjail, confirm and reshare | PENDING then ACTIVE with fresh share; no stale share reuse | documentation-only: slashing/time control absent |
| PFS-006-09 | crash boundaries | operation poised at each registration/DKG/reward/exit checkpoint | crash and restart | semantic pre-state or complete outcome at every boundary | `@pfs-006-09` covers active-share restart and full-committee sealed TEE recovery; other checkpoints remain gaps |
| PFS-006-10 | cleanup and re-registration | inactive validator with no bonded/live claims | clean indexes then register identity again | no stale pubkey/cooldown/index; exactly one live record | documentation-only: maturity/cleanup fixture absent |

## Open questions and technical debt

- Replace direct raw cross-module writes with typed command/receipt seams before
  treating this flow as Accepted.
- Define one durable intent identity for DKG activation and every punishment.
- Define exact restart ownership for in-flight DKG and overdue Rewards/unbonding work.
- Add narrower scenarios for the claim, slash, committee-exclusion and crash-boundary
  assertions that existing tagged composite features cover only partially.
- Implement the missing voluntary exit/value conservation, Rewards settlement,
  duplicate evidence, unjail, fault-injection and re-registration scenarios.
- Add a mixed-version topology proving storage/evidence/committee-format activation.
- Reconcile external diagnostic journal entries with committed on-chain receipts.
