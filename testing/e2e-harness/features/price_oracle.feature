@price-oracle @min-validators-4
Feature: Price Oracle publishes through independent validator feeders
  The production feeder path must reach the on-chain count quorum without
  treating stake as extra quorum votes.

  Scenario: The minimum two-thirds committee publishes one controlled COEN USD quote
    Given a fresh localnet with a 8-block voting window
    And the committee has reached a usable height
    Then the controlled COEN USD quote is finalized through the real price feeder
    And the committee nodes agree on the state root
