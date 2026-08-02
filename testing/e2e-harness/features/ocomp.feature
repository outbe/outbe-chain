@ocomp @min-validators-4
Feature: Off-chain computation process topology
  @ocomp-int-024
  Scenario: A supervisor failure is isolated from consensus
    Given a fresh four-validator OCOMP measurement localnet
    Then all four OCOMP domains run their node-facing production roles
    And each OCOMP domain owns one authenticated production worker
    When validator 0 OCOMP supervisor is stopped through the typed fault control
    Then consensus finality advances while only that supervisor remains stopped
    And validator 0 OCOMP supervisor restarts through the typed topology

  @ocomp-fork-restart
  Scenario: Validator recovery preserves the fork across every height boundary
    Given the canonical four-validator OCOMP Final devnet before H
    When validator 0 restarts before, across, and after the OCOMP fork height
    Then the OCOMP evidence records successful H-1, H, and H+1 recovery

  @ocomp-fork-mismatch
  Scenario: A distinct immutable fork install cannot join the canonical committee
    Given the canonical four-validator OCOMP Final devnet before H
    When validator 0 restarts with a different valid immutable OCOMP fork install
    Then the canonical committee finalizes through H while the mismatched validator stays before H
