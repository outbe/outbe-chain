@tee @min-validators-4
Feature: FullNode synchronization, failover, promotion, and replay
  FullNodes verify finalized history without voting. Promotion is covered only
  where it changes the node role; ValidatorSet/OCOMP membership is owned by the
  validator-lifecycle suite.

  @pfs-008-01 @pfs-008-02 @pfs-008-03 @pfs-008-04 @pfs-008-05
  Scenario: Chained FullNodes stop on upstream loss and recover through a healthy upstream
    Given a fresh localnet with a short epoch
    When the committee drives past a reshare
    And a cold follower syncs from the committee
    Then the follower reaches the committee finalized checkpoint with matching hash and state root
    When a second follower chains off the first
    Then the chained follower reaches lockstep with the committee
    When the follower loses its only upstream while the committee advances
    Then the disconnected follower makes no unverified finalized progress
    When the follower switches to a healthy upstream and restarts from its durable datadir
    Then the follower reaches the committee finalized checkpoint with matching hash and state root

  @pfs-008-06 @pfs-008-07 @pfs-008-08
  Scenario: Warm promotion survives duplicate readiness and boundary restarts
    Given a fresh localnet with a short epoch
    When the committee drives past a reshare
    And a cold follower syncs from the committee
    Then the follower reaches the committee finalized checkpoint with matching hash and state root
    When the first follower is promoted to a validator with its warm datadir
    And readiness is resubmitted before the warm promotion restart
    And the warm-promoted node and an active validator restart around the activation boundary
    Then promotion activates only at its planned boundary with sealed state and exact network parity

  @pfs-008-09 @follower-off-grid-restart
  Scenario: A FullNode follows and restarts across a delayed off-grid DKG boundary
    Given a fresh localnet for an off-grid follower boundary
    When a production FullNode with its own enclave syncs from the committee
    Then the follower reaches the committee finalized checkpoint with matching hash and state root
    When a staked joiner freezes a 4-to-5 reshare target
    And the reshare loses quorum before it can complete
    Then the old committee crosses the planned activation height without partial activation
    When the downed validator is restored
    Then the delayed reshare activates off-grid and the active set reaches 5
    And the follower reaches the committee finalized checkpoint with matching hash and state root
    When the follower restarts from its durable datadir against the same upstream
    Then the follower reaches the committee finalized checkpoint with matching hash and state root
