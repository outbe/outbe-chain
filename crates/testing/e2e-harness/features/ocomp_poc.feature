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

  @ocomp-e2e @ocomp-e2e-005
  # OCOMP-TEST-ID: OCM-E2E-005
  Scenario: A tentative request orphan is released before it can obtain attestation
    Given the canonical four-validator OCOMP Final devnet
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    When the next request proposer is isolated before the JobIntent request block
    Then the isolated proposer durably pins one tentative JobIntent candidate
    When the isolated proposer stops and the other three validators finalize a competing request block
    Then the isolated proposer rejoins and releases the exact orphaned tentative pin
    When the validator supervisors submit results directly for that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And the orphaned candidate cannot obtain node attestation

  @ocomp-e2e @ocomp-e2e-006
  # OCOMP-TEST-ID: OCM-E2E-006
  Scenario: A q-forming owner failure rolls back the vote slot and every activation effect
    Given the canonical four-validator OCOMP Final devnet with the Nod owner failpoint
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the validator supervisors submit results directly for that finalized JobIntent
    Then the named Nod owner failure leaves two vote slots and no activation effect

  @ocomp-e2e @ocomp-e2e-007
  # OCOMP-TEST-ID: OCM-E2E-007
  Scenario: An incompatible Supervisor cannot affect consensus or the other validator domains
    Given the canonical four-validator OCOMP Final devnet
    When validator 0 OCOMP supervisor is replaced by an incompatible peer
    And an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the validator supervisors submit results directly for that finalized JobIntent
    Then three compatible validator domains atomically apply Lysis and create the Nod
    And the incompatible supervisor remains outside OCOMP while consensus finality advances

  @ocomp-e2e @ocomp-e2e-008
  # OCOMP-TEST-ID: OCM-E2E-008
  Scenario: A completed generation survives node and compute-process restart and replay
    Given the canonical four-validator OCOMP Final devnet
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the validator supervisors submit results directly for that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    When all validator nodes and OCOMP node-facing processes restart with preserved data
    Then the completed generation and exact vote replay remain identical

  @ocomp-e2e @ocomp-trace
  # OCOMP-TEST-ID: OCM-TRC-001
  Scenario: Proposal import and historical replay execute only OCOMP boundaries
    Given the canonical four-validator OCOMP Final devnet
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the validator supervisors submit results directly for that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    When a late follower replays the finalized OCOMP request and quorum blocks
    Then runtime traces prove proposal import and historical replay without on-chain calculation
