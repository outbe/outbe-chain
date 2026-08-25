@settlement @tee @sgx-no-attest @sudo @min-validators-4
Feature: Protocol positions redeem through the reserve into COEN
  Settlement evidence uses a six-decimal USD asset and an ownerless reserve
  vault deployed by the scenario, while every state transition runs through
  the production precompiles and enclave-backed confidential ledgers.

  @gem-settlement
  Scenario: A validator redeems its protocol reward Gem into COEN
    Given a fresh four-validator OCOMP public measurement localnet
    Then the controlled COEN USD quote is finalized through the real price feeder
    Then validator 0 receives a protocol reward Gem from same-block RewardsGemDelivery
    And validator 0 settles that Gem and redeems its exact Promis into COEN

  @reward-gem-delivery-recovery
  Scenario: A stale Oracle rate defers validator Gem delivery and later recovers
    Given a fresh localnet near the next UTC worldwide-day boundary
    And the committee has reached a usable finalized height
    When the chain crosses into the next worldwide day without a price feeder
    Then the stale boundary finalizes with one pending reward Gem batch and no new Gem
    When the committee restarts while the reward Gem batch is pending
    Then the controlled COEN USD quote is finalized through the real price feeder
    Then the first canonical fresh tally delivers the saved reward Gem batch exactly once
    And every validator observes the same delivered reward Gem and continued finality
