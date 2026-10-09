@ocomp @tee @price-oracle @min-validators-4 @credis-lifecycle
Feature: Credis happy flow and payments
  Scenario: Credis issuance happy path and three payments by user
    Given a prepared Credis localnet with funded actors and vault liquidity
    Then the controlled COEN USD quote is finalized through the real price feeder
    And the pledge valuation window closes over that quote
    When the CCA reserves 300 stablecoins for the user's smart account
    Then the reservation holds the requested liquidity for those actors
    When the user pledges Gratis
    And the CCA issues Credis against the pledge and reservation
    Then the smart account owns the open Credis
    When the user makes three daily payments through the smart account
    Then the principal and interest are paid and all collateral is released
    And the committee nodes agree on the state root

  Scenario: A called Credis left unpaid past its notice is forfeited
    Given a prepared Credis localnet with funded actors and vault liquidity
    Then the controlled COEN USD quote is finalized through the real price feeder
    And the pledge valuation window closes over that quote
    When the CCA reserves 300 stablecoins for the user's smart account
    Then the reservation holds the requested liquidity for those actors
    When the user pledges Gratis
    And the CCA issues Credis against the pledge and reservation
    Then the smart account owns the open Credis
    When the USD rate holds above the Credis call price across its call window
    Then the Credis is called with a settlement notice
    When the user pays part of the called Credis before its deadline
    And the settlement deadline passes unpaid
    Then the Credis reads Forfeited with its remainders written off
    When the forfeit sweep reaches the lapsed Credis
    Then the remaining pledge is burned into the Promis Limit pool once
    And the committee nodes agree on the state root
