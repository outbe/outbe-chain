@tee @sgx-no-attest @sudo @min-validators-4 @enclave-upgrade-hardware
Feature: Independently signed hardware enclave replacement
  Operators replace real release binaries, retain the permanent network key,
  and recover validator authority through finalized proofs and durable state.

  Scenario: Four hardware validators update their nodes and replace their enclaves twice
    Given a fresh localnet with a 20-block voting window
    And the committee has reached a usable height
    When the operators install node release version "0.3" before enclave governance
    And the four validators upgrade their enclaves to version "0.2" using "target/e2e-upgrades/enclave-0.2/outbe-tee-enclave" in round 1
    Then the committee nodes agree on the state root
    When the four validators upgrade their enclaves to version "0.3" using "target/e2e-upgrades/enclave-0.3/outbe-tee-enclave" in round 2
    Then the committee nodes agree on the state root
    And the committee continues producing finalized blocks
