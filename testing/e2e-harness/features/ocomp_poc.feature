@ocomp @tee @sgx-no-attest @sudo @min-validators-4
Feature: Off-chain computation PoC closure
  @ocomp-e2e @ocomp-e2e-001
  # OCOMP-TEST-ID: OCM-E2E-001
  # OCOMP-TEST-ID: OCM-TRC-001
  Scenario: Final public Tribute flows through four independent domains to certified Nod
    Given the canonical four-validator OCOMP Final devnet
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    And every validator serves the same independently verified compressed tribute
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the production OCOMP domains process that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And all four OCOMP domains run their node-facing production roles
    And each OCOMP domain retains isolated deterministic worker artifacts for that JobIntent
    When a late follower replays the finalized OCOMP request and quorum blocks
    Then runtime traces prove proposal import and historical replay without on-chain calculation

  @ocomp-e2e @ocomp-e2e-007
  # OCOMP-TEST-ID: OCM-E2E-007
  Scenario: An incompatible Supervisor cannot affect consensus or the other validator domains
    Given the canonical four-validator OCOMP Final devnet
    When validator 0 OCOMP supervisor is replaced by an incompatible peer
    And an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the production OCOMP domains process that finalized JobIntent
    Then three compatible validator domains atomically apply Lysis and create the Nod
    And the incompatible supervisor remains outside OCOMP while consensus finality advances
