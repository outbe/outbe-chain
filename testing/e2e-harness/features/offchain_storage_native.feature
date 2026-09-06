@ocomp @tee @price-oracle @min-validators-4 @offchain-storage-native-e2e
Feature: Off-chain storage through Lysis with the native mock enclave
  Four validators exercise the real storage and OCOMP processes. Enclave
  emulation does not cover SGX or the separately tested fifth-node onboarding.

  Scenario: RocksDB supports public Tribute, Lysis, and durable restart
    Given a fresh four-validator Metadosis capacity localnet at FORMING
    Then the fresh capacity day is created in FORMING by finalized block 1
    And the controlled COEN USD quote is finalized through the real price feeder
    And every OCOMP transaction signer is distinct and scoped only to the OCOMP role
    When the committee logical clock reaches the fresh capacity OFFERING window
    Then the same fresh capacity day advances through LOOKBACK to OFFERING
    When an operator submits one encrypted tribute offer with WAA and SRA beneficiaries
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    And every validator serves the same independently verified compressed tribute
    When the committee logical clock reaches the fresh capacity processing time
    Then the same fresh capacity day advances through WAITING and READY
    And Metadosis creates one finalized JobIntent from that public Tribute
    When the production OCOMP domains process that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And all four OCOMP domains run their node-facing production roles
    And each OCOMP domain retains isolated deterministic worker artifacts for that JobIntent
    When validator 0 SnapshotExporter restarts from a prepared-only crash state
    And all validator nodes and OCOMP node-facing processes restart with preserved data
    Then the completed generation and exact vote replay remain identical
