@ocomp @tee @min-validators-4
Feature: Off-chain computation PoC closure
  @ocomp-e2e @ocomp-e2e-001
  # OCOMP-TEST-ID: OCM-E2E-001
  Scenario: Final public Tribute flows through four independent domains to certified Nod
    Given the canonical four-validator OCOMP Final devnet
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    And every validator serves the same independently verified compressed tribute
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the validator supervisors submit results directly for that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And all four OCOMP domains run their node-facing production roles
    And each OCOMP domain owns one authenticated production worker
