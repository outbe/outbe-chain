@tee @min-validators-4
Feature: Upstream followers, validator catch-up, and warm promotion
  # A cold --upstream follower syncs a
  # reshared chain to lockstep; a second follower chains off the first; a
  # validator killed mid-epoch restarts and re-locksteps; finally follower1's
  # synced datadir is warm-promoted into an ACTIVE validator.

  @pfs-008-01 @pfs-008-02 @pfs-008-03 @pfs-008-04
  Scenario: Followers sync, a validator recovers, and a follower is warm-promoted
    Given a fresh localnet with a short epoch
    When the committee drives past a reshare
    And a cold follower syncs from the committee
    Then the follower reaches lockstep with the committee
    When a second follower chains off the first
    Then the chained follower reaches lockstep with the committee
    When a validator is killed and restarted mid-epoch
    Then the restarted validator catches up to lockstep
    When the first follower is promoted to a validator with its warm datadir
    Then the promoted validator activates and stays in lockstep

  @pfs-008-05 @tee
  Scenario: A follower stalls safely without its upstream and catches up after switching upstream
    Given a fresh localnet with a short epoch
    When the committee drives past a reshare
    And a cold follower syncs from the committee
    Then the follower reaches the committee finalized checkpoint with matching hash and state root
    When the follower loses its only upstream while the committee advances
    Then the disconnected follower makes no unverified finalized progress
    When the follower switches to a healthy upstream and restarts from its durable datadir
    Then the follower reaches the committee finalized checkpoint with matching hash and state root

  @pfs-008-06 @pfs-008-07 @pfs-008-08 @tee
  Scenario: Warm promotion survives duplicate readiness and node, enclave, and validator restarts
    Given a fresh localnet with a short epoch
    When the committee drives past a reshare
    And a cold follower syncs from the committee
    Then the follower reaches the committee finalized checkpoint with matching hash and state root
    When the first follower is promoted to a validator with its warm datadir
    And readiness is resubmitted before the warm promotion restart
    And the warm-promoted node and an active validator restart around the activation boundary
    Then promotion activates only at its planned boundary with sealed state and committee parity
