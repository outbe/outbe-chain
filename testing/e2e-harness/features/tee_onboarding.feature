@tee @real-sgx @dcap-only @min-validators-4
Feature: Permanent TEE offer-key onboarding
  Validator-set activation is tested separately. This scenario owns only the
  permanent attested key and authenticated NodeHost restart contract.

  Scenario: Validator and FullNode survive restart after production onboarding
    Given a fresh localnet with a 6-block voting window
    When a production validator joins and restarts with its permanent offer key
    Then the validator reopens the exact permanent key without re-registration
    When the validator DCAP sealed state is offered to an SGX no-attestation runtime
    Then the downgrade runtime cannot reopen or expose the permanent key
    When a production full node joins, starts, and restarts with its own enclave
    Then the full node reopens the exact permanent key before execution sync
    When finalized consensus time enters the renewal window
    Then manual renewal finalizes for the Validator and FullNode without changing their offer key
