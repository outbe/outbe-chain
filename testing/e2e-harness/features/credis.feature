@ocomp @tee @price-oracle @min-validators-4 @credis-lifecycle
Feature: Credis borrowing and repayment
  # The account deliberately gives the user and CCA equal call access. Account
  # policy is tested in its own repository; this flow exercises chain accounting.
  # Gratis is acquired through the enclave, not injected as plaintext storage.
  Scenario: A user repays a CCA-issued position in three payments
    Given a prepared Credis localnet with funded actors and vault liquidity
    Then the controlled COEN USD quote is finalized through the real price feeder
    When the CCA reserves 300 stablecoins for the user's smart account
    Then the reservation holds the requested liquidity for those actors
    When the user pledges Gratis for that credit
    And the CCA issues Credis against the pledge and reservation
    Then the smart account owns the open Credis position
    When the user makes three daily payments through the smart account
    Then the principal and interest are paid and all collateral is released
    And the committee nodes agree on the state root
