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
    And each OCOMP domain retains isolated deterministic worker artifacts for that JobIntent

  @ocomp-e2e @ocomp-e2e-002
  # OCOMP-TEST-ID: OCM-E2E-002
  Scenario: An empty Tribute day uses the terminal compatibility branch
    Given the canonical four-validator OCOMP Final devnet
    Then the empty Tribute day completes without a JobIntent or Nod and records its direct remainder
    And no validator projects a tribute

  @ocomp-e2e @ocomp-e2e-004
  # OCOMP-TEST-ID: OCM-E2E-004
  Scenario: A rejected duplicate Tribute never enters the later Lysis generation
    Given the canonical four-validator OCOMP Final devnet
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    When the operator submits a duplicate logical tribute offer with different parameters for the same day
    Then the duplicate is rejected without changing tribute state or projections
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the validator supervisors submit results directly for that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And the certified Lysis generation contains only the original Tribute
