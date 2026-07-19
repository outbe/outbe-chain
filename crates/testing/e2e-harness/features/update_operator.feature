@min-validators-4
Feature: Operator protocol-version update via governance vote
  # An operator proposes a protocol
  # update, three validators approve it, and after the voting window + activation
  # height the new version goes live with state-root parity across the committee.
  # Needs a >=4-validator committee; runs with or without TEE (--tee any).
  #
  # The voting window is measured in blocks (~1s each — consensus enforces a 1000ms
  # minimum). It must outlast the wall-clock cost of casting the votes over
  # sequential RPC round-trips, or the proposal expires before it can be approved.
  # The vote step fires the ballots without blocking and the tally is polled, so a
  # 20-block (~20s) window leaves room.

  @pfs-005-01
  Scenario: An update is proposed, approved, scheduled, and activated
    Given a fresh localnet with a 20-block voting window
    And the committee has reached a usable height
    When operator "validator-0" proposes an update to the next protocol version
    Then proposal 1 is pending, targets the update module, and carries the activation height
    When validators "validator-0,validator-1,validator-2" cast yes votes
    Then proposal 1 is still pending with 3 yes votes
    When the committee passes the vote deadline
    Then proposal 1 is approved and the scheduled update matches the proposal
    When the committee passes the activation height
    Then the active protocol version equals the proposed version
    And the scheduled update is marked activated
    And the committee nodes agree on the state root

  @pfs-005-restart-recovery
  Scenario: Governance and update state survive voting and activation-boundary restarts
    Given a fresh localnet with a 20-block voting window
    And the committee has reached a usable height
    When operator "validator-0" proposes an update to the next protocol version
    Then proposal 1 is pending, targets the update module, and carries the activation height
    When validator "validator-3" restarts during the voting window
    Then proposal 1 and its votes are identical on every validator
    When validators "validator-0,validator-1,validator-2" cast yes votes
    Then proposal 1 is still pending with 3 yes votes
    And proposal 1 and its votes are identical on every validator
    When the committee passes the vote deadline
    Then proposal 1 is approved and the scheduled update matches the proposal
    When the entire committee restarts after update scheduling
    Then the approved proposal and waiting schedule are identical on every validator
    When the committee approaches the activation height
    And the entire committee restarts at the activation boundary
    Then the update activation converges on every validator after the boundary restart
    When the committee passes the activation height
    Then the activated update state is identical on every validator
    And the committee continues producing finalized blocks

  @pfs-005-duplicate-vote
  Scenario: A duplicate ballot reverts without changing the approved update
    Given a fresh localnet with a 20-block voting window
    And the committee has reached a usable height
    When operator "validator-0" proposes an update to the next protocol version
    Then proposal 1 is pending, targets the update module, and carries the activation height
    When validators "validator-0,validator-1,validator-2" cast yes votes
    Then proposal 1 is still pending with 3 yes votes
    When validator "validator-0" repeats the yes vote on proposal 1
    Then the duplicate vote reverts and proposal 1 still has 3 yes votes on every validator
    When the committee passes the vote deadline
    Then proposal 1 is approved and the scheduled update matches the proposal

  @pfs-005-expired
  Scenario: A below-quorum update expires without scheduling or version mutation
    Given a fresh localnet with a 20-block voting window
    And the committee has reached a usable height
    When operator "validator-0" proposes an update to the next protocol version
    Then proposal 1 is pending, targets the update module, and carries the activation height
    When validators "validator-0,validator-1" cast yes votes
    Then proposal 1 is still pending with 2 yes votes
    When the committee passes the vote deadline
    Then proposal 1 is expired without an update schedule on every validator
    And the active protocol version remains baseline on every validator
    And the committee continues producing finalized blocks

  # Oversized U256 pagination args used to panic inside Vote::clamp_page via
  # U256::to::<u64>() and could take down the RPC node. After the saturating
  # conversion fix the call must stay non-fatal.
  Scenario: Oversized listProposals pagination must not kill the RPC node
    Given a fresh localnet with a 20-block voting window
    And the committee has reached a usable height
    When operator "validator-0" proposes an update to the next protocol version
    And validator "validator-0" receives listProposals with index 2^256-1 and count 1
    Then validator "validator-0" node process is still running
    And listProposals with index 2^256-1 and count 1 on "validator-0" returns an empty page

  # A scheduled version above the binary PROTOCOL_VERSION is allowed through
  # propose/vote/schedule, but activation returns PrecompileError::Fatal and
  # aborts the activation-height block — the committee stalls below that height.
  @pfs-005-09
  Scenario: Activating a version above the binary fatally stalls the chain
    Given a fresh localnet with a 20-block voting window
    And the committee has reached a usable height
    When operator "validator-0" proposes an update to an unsupported protocol version
    Then proposal 1 is pending, targets the update module, and carries the activation height
    When validators "validator-0,validator-1,validator-2" cast yes votes
    Then proposal 1 is still pending with 3 yes votes
    When the committee passes the vote deadline
    Then proposal 1 is approved and the scheduled update matches the proposal
    When the committee approaches the activation height
    Then the committee does not advance past the activation height
    And the active protocol version is unchanged
    And the scheduled update is still waiting for activation
    And validator "validator-0" logs report the unsupported activation as fatal
    When the entire committee restarts after the unsupported activation failure
    Then every validator RPC recovers below the unsupported activation height
    And the unsupported proposal and waiting schedule are identical on every validator
    And the committee remains stalled below the unsupported activation height
    And validator "validator-0" logs report the unsupported activation as fatal
