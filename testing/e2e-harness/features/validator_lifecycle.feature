@tee @min-validators-4
Feature: Validator admission, membership, recovery, and removal
  Every scenario owns a distinct ValidatorSet or DKG transition. OCOMP appears
  only where changing ACTIVE membership must preserve historical job snapshots.

  @pfs-006-01 @pfs-006-03
  Scenario: A FullNode becomes ACTIVE, preserves an in-flight offer, and exits cleanly
    Given a fresh lifecycle localnet with a 6-block voting window
    When operator "validator-0" submits a tribute offer
    Then the committee processes and projects the offer
    When a full node joins and syncs to the committee tip
    Then the full node matches committee supply and state root and is not a participant
    When the full node stakes and confirms readiness
    Then it is promoted to an active participant and the in-flight offer lands once
    When the promoted validator deactivates
    Then it exits, the committee reshares down, and the node demotes to a follower
    And its unbonded stake can be claimed with exact accounting

  @pfs-006-09
  Scenario: Restarted ACTIVE validator resumes signing from its persisted share
    Given a fresh localnet with a 6-block voting window
    When a joiner reaches active with a persisted share
    And the node is killed and restarted with the same keys
    Then it resumes signing from the persisted share without a new ceremony

  @pfs-006-09
  Scenario: Entire committee recovers after every enclave restarts
    Given a fresh localnet with a 6-block voting window
    When the entire committee and its enclaves are stopped and restarted
    Then all validators recover sealed TEE state and resume finalization

  @pfs-006-09
  Scenario: Completed DKG survives a joining node restart before activation
    Given a fresh localnet with a restartable DKG window
    When a joiner completes DKG and waits below the activation boundary
    And the joining node and enclave restart before activation
    Then the recovered pending DKG activates once and consensus continues

  @pfs-006-09
  Scenario: In-flight DKG survives a joining node and enclave restart
    Given a fresh localnet with a restartable DKG window
    When a joining validator is restarted during its DKG ceremony
    Then the old committee stays live and a later DKG activates the joiner once

  @pfs-006-09
  Scenario: Registration survives a joining node and enclave restart
    Given a fresh localnet with a 6-block voting window
    When a registered joining node and enclave restart before staking
    Then registration survives and the join can activate once

  @pfs-006-09
  Scenario: An ACTIVE validator and enclave restart during reshare
    Given a fresh localnet with a restartable DKG window
    When an active validator and enclave restart during a joining reshare
    Then the frozen reshare activates once with the restarted validator in lockstep

  @pfs-006-04
  Scenario: Stalled reshare keeps the old set live and recovers when quorum returns
    Given a fresh localnet with a wide DKG activation grace
    When a staked joiner freezes a 4-to-5 reshare target
    And the reshare loses quorum before it can complete
    Then the old committee keeps finalizing through the stalled reshare
    When the downed validator is restored
    Then the reshare completes and the active set reaches 5

  @pfs-006-04
  Scenario: Permanently unavailable frozen target fails closed at VRF expiry
    Given a fresh localnet with a short DKG activation grace
    When a staked joiner freezes a 4-to-5 reshare target
    And the frozen-target joiner and one validator remain offline
    Then the old committee finalizes without partial activation until VRF expiry
    And the surviving validators exit with the frozen-target expiry error

  @pfs-006-06
  Scenario: A downed ACTIVE validator is slashed once while the quorum stays live
    Given a fresh localnet with a 6-block voting window
    And the slashing config is readable
    And validator "validator-3" starts active
    When validator "validator-3" is killed
    Then the committee keeps finalizing until the validator is slashed exactly once
    And continued downtime does not slash the validator twice

  @ocomp @ocomp-dynamic-admission @ocomp-dynamic-overlap
  Scenario: New ACTIVE membership affects only new OCOMP jobs
    Given a fresh four-validator OCOMP dynamic-membership localnet with two scheduled jobs
    When one public Tribute is submitted for each scheduled OCOMP job
    And validators 2 and 3 OCOMP workers are stopped before the job
    Then job A opens with four members and quorum three while job B remains scheduled
    When a fifth node syncs as a non-voting FullNode
    Then the fifth node has canonical state parity without OCOMP vote capability
    And the FullNode independently materializes job A without voting
    When the synced node completes OCOMP-ready validator admission
    Then the certified boundary adds exactly one fifth OCOMP validator domain
    And job B opens with five members and quorum four while job A remains four of three
    When validator 2 OCOMP worker restarts and completes both pinned quorums
    Then the FullNode result for job A matches the canonical quorum result
    And both deadlines record validator 3 missing and keep the chain live after jailing it

  @pfs-006-02
  Scenario: Unconfirmed joiner remains PENDING until confirm-ready
    Given a fresh localnet with a 6-block voting window
    When a staked joiner has not confirmed readiness
    Then the unconfirmed joiner stays pending across a full reshare cycle
    When the joiner confirms readiness
    Then the confirmed joiner activates on the next reshare
