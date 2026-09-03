@price-oracle @validators-4
Feature: Price Oracle publishes through independent validator feeders
  The production feeder path must reach the on-chain count quorum without
  giving any active validator more than one quorum vote.

  Scenario: Per-pair quorum survives a sub-quorum cross intersection
    Given a fresh price oracle localnet with a 8-block voting window
    And the committee has reached a usable height
    Then independent validator feeders finalize overlapping pair quorums
    And the committee nodes agree on the state root
