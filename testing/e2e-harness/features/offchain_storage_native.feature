@tee @mock-native @price-oracle @min-validators-4 @offchain-storage-native-e2e
Feature: EVM state persists through confidential Lysis and restart
  Scenario: Native Tribute and Nod state survives validator restart
    Given a fresh four-validator native Lysis localnet
    Then the controlled COEN USD quote is finalized through the real price feeder
    When an L2 network is registered through governance with an inbox contract and no pinned key
    And a user of the registered L2 submits an encrypted Tribute with ZKP and WAA and SRA beneficiaries
    Then the tribute transaction succeeds and supply becomes one
    And every validator stores the same Tribute and indexes in EVM state
    And every validator verifies the same Tribute against finalized EVM state
    When the committee logical clock reaches the native Lysis processing time
    Then Metadosis completes confidential Lysis and stores ready Nods on every validator
    And the encrypted NOD matches its canonical body, public views and native issuance event
    When all validator nodes restart with preserved EVM state
    Then the encrypted NOD matches its canonical body, public views and native issuance event
