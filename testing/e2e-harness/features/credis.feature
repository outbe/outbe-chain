@ocomp @tee @price-oracle @min-validators-4 @credis-lifecycle
Feature: Credis happy flow and payments
  Scenario: Credis issuance happy path and three payments by user
    Given a prepared Credis localnet with funded actors and vault liquidity
    Then the controlled COEN USD quote is finalized through the real price feeder
    When the CCA reserves 300 stablecoins for the user's smart account
    Then the reservation holds the requested liquidity for those actors
    When the user pledges Gratis
    And the CCA issues Credis against the pledge and reservation
    Then the smart account owns the open Credis position
    When the user makes three daily payments through the smart account
    Then the principal and interest are paid and all collateral is released
    And the committee nodes agree on the state root
