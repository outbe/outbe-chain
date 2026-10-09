@min-validators-4
Feature: Consensus and Node Resilience (B8, B20, R7, R11, R13, S14)
  # Network-level resilience scenarios for outbe-chain with explicit scope boundaries:
  #   - B8:  post-epoch restart and finalization progress smoke
  #   - B20: PREVRANDAO binds the exact parent proof across restart, epoch transition, and follower replay
  #   - R7:  follower recovery across upstream loss/stall; dedup/retry/cancellation checked at the Commonware adapter boundary
  #   - R11: finalize vote ingress across epoch boundaries without consensus progress stall
  #   - R13: downtime recovery with valid empty V2 missed-proposers metadata
  #   - S14: txpool regression verifying nonce sequence preservation and execution order across proposals

  @b8-cert-retention @b20-prev-randao
  Scenario: Committee finalization recovers after post-epoch validator restart
    Given a fresh localnet with a short epoch and 6-block voting window
    And the committee reaches a finalized height past the epoch transition
    When an active validator is stopped and restarted after the epoch transition
    Then the restarted validator catches up and resumes finalization
    And the committee continues producing and finalizing blocks in lockstep
    And finalized headers derive PREVRANDAO from their exact parent proofs

  @r7-follower-cancel @b20-prev-randao @tee @sgx-no-attest @sudo
  Scenario: Follower sync recovers through a healthy upstream after upstream stall
    Given a fresh localnet with a 6-block voting window
    And the committee has reached a usable height
    When a production FullNode syncs from the committee with bounded follower resolution
    And the follower upstream is stopped while committee consensus advances
    Then the disconnected follower remains responsive while its upstream is offline
    When the follower is restarted in place and switched to an active healthy upstream
    Then the follower catches up to the committee finalized checkpoint with matching hash and state root
    And finalized headers derive PREVRANDAO from their exact parent proofs

  @r11-finalize-epoch-progress
  Scenario: Finalize vote ingress across epoch boundary preserves consensus progress
    Given a fresh localnet with a short epoch and 6-block voting window
    And the committee advances towards an epoch transition
    When active validators process ingress finalize votes across epoch boundaries
    Then the committee advances across the epoch boundary without progress stall
    And all committee validators finalize blocks in lockstep past the transition

  @r13-view-gap-recovery
  Scenario: Consensus recovers across non-proposing validator downtime with empty V2 missed-proposers metadata
    Given a fresh localnet with a 6-block voting window
    And the committee has reached a usable height
    When an active validator experiences downtime while consensus advances
    And the successor block contains empty V2 missed-proposers metadata in phase 1 accounting
    And the committee continues producing and finalizing blocks without progress stall

  @s14-nonce-pool-regression
  Scenario: Ordered nonce transactions from an account are preserved in pool and mined in sequence
    Given a fresh txpool-eviction localnet with a 6-block voting window
    And the committee has reached a usable height
    When an operator submits a sequence of ordered nonce transactions from one account
    And an independent healthy transaction is submitted from another sender
    Then the independent healthy transaction is successfully mined
    And the dependent nonce sequence remains present in the transaction pool or receipt pipeline
    When the next block proposal executes
    Then the nonce transactions are mined in canonical order
