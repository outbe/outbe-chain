@tee @sgx-no-attest @sudo @min-validators-4 @encrypted-tribute-v2
Feature: Encrypted Tribute creation and repeated key derivation
  The creator owns a Tribute independently of the transaction caller. Public
  bodies, events and attributes retain ciphertext. Readers rederive amount keys.

  Scenario: A creator and every hardware enclave read the same encrypted Tribute after restart
    Given a fresh localnet with a bounded Tribute offering and a 6-block voting window
    When the operator offers a Tribute owned by a different creator
    Then the tribute transaction succeeds and supply becomes one
    And every validator stores the same Tribute and indexes in EVM state
    And every validator verifies the same Tribute against finalized EVM state
    And the creator and each enclave read encrypted Tribute values while public attributes retain ciphertext
    When the first validator and enclave restart with their existing sealed state
    Then every validator verifies the same Tribute against finalized EVM state
    And the creator and each enclave read encrypted Tribute values while public attributes retain ciphertext

  Scenario: Encrypted Tribute finalizes while one validator enclave is unavailable
    Given a fresh localnet with a bounded Tribute offering and a 6-block voting window
    When validator 3 enclave is stopped while an encrypted Tribute is submitted
    Then the surviving quorum finalizes and decrypts that encrypted Tribute
    When validator 3 enclave is restored from its sealed state
    Then every validator stores the same Tribute and indexes in EVM state
    And the creator and each enclave read encrypted Tribute values while public attributes retain ciphertext
