@tee @sgx-no-attest @sudo @min-validators-4 @ocomp @encrypted-tribute-v2
Feature: Encrypted Tribute creation and repeated key derivation
  The creator owns a Tribute independently of the transaction caller. Public
  bodies, events and attributes retain ciphertext. Readers rederive amount keys.

  Scenario: A creator and every hardware enclave read the same encrypted Tribute after restart
    Given a fresh localnet with a bounded Tribute offering and a 6-block voting window
    When the operator offers a Tribute owned by a different creator
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    And every validator serves the same independently verified compressed tribute
    And the creator and each enclave read encrypted Tribute values while public attributes retain ciphertext
    When the first validator and enclave restart with their existing sealed state
    Then every validator serves the same independently verified compressed tribute
    And the creator and each enclave read encrypted Tribute values while public attributes retain ciphertext
