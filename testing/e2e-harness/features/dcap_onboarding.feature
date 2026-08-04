@tee @real-sgx @min-validators-4
Feature: DcapRequired one-time permanent offer-key onboarding
  P0 ends when an attested Validator or FullNode has durably ingested the
  permanent genesis offer key and can restart through its authenticated
  NodeHost identity. Validator-set activation is tested separately.

  Scenario: Validator and FullNode survive restart after production onboarding
    Given a fresh localnet with a 6-block voting window
    When a production validator joins and restarts with its permanent offer key
    Then the validator reopens the exact permanent key without re-registration
    When a production full node joins, starts, and restarts with its own enclave
    Then the full node reopens the exact permanent key before execution sync
    When finalized consensus time enters the renewal window
    Then automatic renewal finalizes for the Validator and FullNode without changing their offer key
