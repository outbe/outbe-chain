@min-validators-4
Feature: Validator lifecycle state remains internally consistent
  # These scenarios exercise only public/runtime-reachable transitions. Genesis
  # profiles may shorten positive protocol windows or select valid owners and
  # capacities, but never seed malformed storage. Every scenario is currently
  # enforced by the main suite; there are no target-invariant exclusions.

  @risk-d-01a
  Scenario: A registration proof cannot be replayed on another valid chain
    Given a funded validator registration fixture on a valid localnet with chain id 54322345
    And the fixture is accepted without changing its identity material
    When the localnet is re-bootstrapped with valid chain id 424242
    And the same validator address, BLS public key, and proof are submitted
    Then the cross-chain proof replay is rejected without changing the registry identity bundle

  @risk-d-01b
  Scenario: ValidatorSet owner cannot register a validator without possession proof
    Given a fresh localnet whose configured ValidatorSet owner is "validator-0"
    And a funded unregistered validator identity
    When the owner submits its registration with an empty possession proof
    Then owner registration is rejected without changing the registry identity bundle

  @risk-d-02 @tee
  Scenario: A set-change hint waits for the next periodic DKG window
    Given a fresh localnet with a short valid DKG schedule
    And a staked joiner confirms readiness immediately after the target freeze
    When the joiner set-change flag becomes observable
    Then no new DKG ceremony starts before the next periodic window
    And the flag remains set until a matching boundary activates the joiner
    And the joiner becomes active no later than that scheduled DKG window

  @risk-d-04
  Scenario: An owner cannot omit an active validator from the committee
    Given a fresh localnet whose four active validators have BLS shares
    When the configured owner attempts to activate a canonical reshared set omitting "validator-3"
    Then the owner omission is rejected with all four validators still ACTIVE with shares
    And committee membership and state roots converge on every validator

  @risk-d-05
  Scenario: One canonical felony evidence can punish a validator only once
    Given valid conflicting-notarize evidence for an active validator and its canonical reverse
    When a reporter submits the evidence and then submits every replay
    Then exactly one punishment changes status, stake, slash count, burn, reporter reward, and events
    And every replay is rejected without another punishment state change

  @risk-d-06 @tee
  Scenario: Inactive re-registration resets coupled validator state
    Given an exiting validator with an old BLS key, P2P address, and observable lifecycle residue
    When its final unbonding claim and same-EOA registration with a new BLS key execute in one block
    Then re-registration reuses exactly one dense index and resets all lifecycle residue
    And the old BLS key can be registered by another candidate exactly once
    And a duplicate registration of the new BLS key is rejected without partial registry writes
    And staking without a new readiness confirmation cannot activate the re-registered validator

  @risk-d-07
  Scenario: Partial jailed unstake does not begin validator exit
    Given an active validator is jailed by valid felony evidence
    When it unstakes below minimum while leaving a nonzero bonded amount
    Then it remains JAILED with the remaining stake and no exit transition

  @risk-d-10
  Scenario: Early unjail never leaves a pending validator with a live share
    Given an active validator is jailed by valid felony evidence
    When it requests unjail before an exclusion boundary
    Then early unjail is rejected and preserves the retained jailed committee member
    And the validator cannot participate until readiness is reconfirmed and a fresh reshare commits

  @risk-s-01 @tee
  Scenario: Leaving pending through unstake clears readiness
    Given a staked PENDING joiner has confirmed readiness
    When it unstakes below minimum before activation
    Then it is REGISTERED with readiness cleared
    When it stakes again without another readiness confirmation
    Then it remains PENDING and excluded after the next scheduled reshare

  @risk-s-02
  Scenario: A direct owner activation cannot bypass boundary orchestration
    Given a fresh localnet whose configured ValidatorSet owner is "validator-0"
    And the public validator state bundle is snapshotted
    When the owner directly activates a registered unconfirmed member with an arbitrary group hash
    Then direct activation is rejected with the validator state bundle unchanged

  @risk-s-03
  Scenario Outline: A malformed reshared set is rejected atomically
    Given a fresh localnet whose configured ValidatorSet owner is "validator-0"
    And the public validator state bundle is snapshotted
    When the owner submits a reshared set with "<mutation>"
    Then malformed reshared-set activation is rejected with the validator state bundle unchanged

    Examples:
      | mutation                   |
      | a duplicate active member |
      | non-canonical member order |
      | a mismatched active hash   |

  @risk-s-06
  Scenario: Stake mirrors and native value remain conserved through the lifecycle
    Given isolated validators for successful and reverting lifecycle actions
    When the harness finalizes stake, rejected stake, slash, exit, and claim actions
    Then after every action the Staking and ValidatorSet stake mirrors agree on every node
    And the Staking native balance equals bonded stake plus all live claims

  @risk-s-08 @tee
  Scenario: A validator stays unbonding until every live claim is consumed
    Given an active validator has staggered partial-unstake claims before exit
    When an exclusion boundary moves it to UNBONDING
    And its matured claims are consumed one at a time
    Then it remains UNBONDING while bonded stake or any live claim remains
    And only the final claim moves it to INACTIVE with zero stake, zero unbonding end, and no share

  @risk-s-09 @tee
  Scenario: P2P version and payload change and clear as one pair
    Given a registered validator has a valid P2P version and payload
    When invalid-version, malformed-payload, and unauthorized P2P updates are submitted
    Then every update is rejected and the original P2P pair remains intact on every node
    When the validator completes cleanup and re-registration
    Then both P2P fields are cleared together on every node

  @risk-s-12
  Scenario: Evicted epoch evidence cannot use a colliding snapshot-ring entry
    Given valid conflicting-notarize evidence is retained for the current epoch
    When the unchanged committee advances beyond the snapshot-ring retention window
    And the old evidence is submitted after its epoch slot has been reused
    Then the evicted evidence is rejected without punishment or accounting changes

  @risk-s-14
  Scenario: Rejected registrations never leave partial identity or index state
    Given a valid localnet with capacity for exactly one additional validator
    When invalid-PoP, duplicate-key, and over-capacity registrations are attempted
    Then every rejected registration leaves count, indexes, key ownership, and candidate records unchanged
