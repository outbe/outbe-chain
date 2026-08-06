@min-validators-4
Feature: Local target chain
  # Cross-chain work needs a second chain to send to. Before any contract or
  # message is involved, prove the harness can own that chain next to the
  # committee: both answer on their own endpoints, neither disturbs the other,
  # and the extra process is torn down with the scenario.

  Scenario: A local target chain runs alongside the committee
    Given a fresh localnet with a 20-block voting window
    And the committee has reached a usable height
    When a local target chain is started
    Then the target chain answers with its own chain id
    And the committee is still producing blocks
