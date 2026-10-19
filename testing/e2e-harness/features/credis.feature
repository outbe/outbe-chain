@tee @price-oracle @min-validators-4 @credis-lifecycle
Feature: Credis happy flow and payments
  Scenario: Credis issuance happy path and three payments by the owner
    Given a prepared Credis localnet with funded actors and vault liquidity
    Then the controlled COEN USD quote is finalized through the real price feeder
    And the pledge valuation window closes over that quote
    When the CCA reserves 300 stablecoins for the Credis owner
    Then the reservation holds the requested liquidity for those actors
    When the user pledges Gratis
    And the CCA issues Credis against the pledge and reservation
    Then the owner holds the issued Credis
    When the owner makes three daily payments
    Then the principal and interest are paid and all collateral is released
    And the committee nodes agree on the state root

  Scenario: A called Credis left unpaid past its notice is forfeited
    Given a prepared Credis localnet with funded actors and vault liquidity
    Then the controlled COEN USD quote is finalized through the real price feeder
    And the pledge valuation window closes over that quote
    When the CCA reserves 300 stablecoins for the Credis owner
    Then the reservation holds the requested liquidity for those actors
    When the user pledges Gratis
    And the CCA issues Credis against the pledge and reservation
    Then the owner holds the issued Credis
    When the USD rate holds above the Credis call price across its call window
    Then the Credis is called with a settlement notice
    When the owner pays part of the called Credis before its deadline
    And the settlement deadline passes unpaid
    Then the Credis reads Forfeited with its remainders written off
    When the forfeit sweep reaches the lapsed Credis
    Then the remaining pledge is burned into the Promis Limit pool once
    And the committee nodes agree on the state root

  Scenario: The owner forfeits an issued Credis
    Given a prepared Credis localnet with funded actors and vault liquidity
    Then the controlled COEN USD quote is finalized through the real price feeder
    And the pledge valuation window closes over that quote
    When the CCA reserves 300 stablecoins for the Credis owner
    Then the reservation holds the requested liquidity for those actors
    When the user pledges Gratis
    And the CCA issues Credis against the pledge and reservation
    Then the owner holds the issued Credis
    When the owner forfeits the Credis
    Then the whole pledge is burned into the Promis Limit pool and the principal written off
    And the committee nodes agree on the state root
