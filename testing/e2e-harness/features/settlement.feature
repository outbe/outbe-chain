@settlement @tee @sgx-no-attest @sudo @min-validators-4
Feature: Protocol positions redeem through the reserve into COEN
  Settlement evidence uses a six-decimal USD asset and an ownerless reserve
  vault deployed by the scenario, while every state transition runs through
  the production precompiles and enclave-backed confidential ledgers.

  @gem-settlement
  Scenario: A zero-balance validator redeems its reward Gem through ZeroFee
    Given a fresh four-validator native Lysis localnet
    Then the controlled COEN USD quote is finalized through the real price feeder
    Then validator 0 claims its validator reward as a Gem
    And validator 0 with zero COEN uses ZeroFee to redeem that Gem into exact COEN
