@settlement @tee @sgx-no-attest @sudo @min-validators-4
Feature: Protocol positions redeem through the reserve into COEN
  Settlement evidence uses a six-decimal USD asset and an ownerless reserve
  vault deployed by the scenario, while every state transition runs through
  the production precompiles and enclave-backed confidential ledgers.

  @gem-settlement
  Scenario: A validator redeems its protocol reward Gem into COEN
    Given a fresh four-validator OCOMP public measurement localnet
    Then the controlled COEN USD quote is finalized through the real price feeder
    Then validator 0 receives a protocol reward Gem
    And validator 0 settles that Gem and redeems its exact Promis into COEN
