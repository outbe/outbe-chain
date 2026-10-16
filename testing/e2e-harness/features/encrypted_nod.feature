@encrypted-nod @lysis @tee @sgx-no-attest @sudo @min-validators-4 @price-oracle
Feature: Encrypted NOD entitlement and private Gratis exercise
  Scenario: A relayed offer creates a self-contained encrypted NOD and only its owner receives Gratis
    Given a fresh four-validator native Lysis localnet
    Then the controlled COEN USD quote is finalized through the real price feeder
    When an operator offers a Tribute for a different validator as the encrypted NOD owner
    Then the tribute transaction succeeds and supply becomes one
    And every validator stores the same Tribute and indexes in EVM state
    When the committee logical clock reaches the native Lysis processing time
    Then Metadosis completes confidential Lysis and stores ready Nods on every validator
    And the encrypted NOD matches its canonical body, public views and native issuance event
    When the encrypted NOD is paid and exercised through a relayer across restarts
    Then the removed Gratis supply selector rejects calls on every validator
