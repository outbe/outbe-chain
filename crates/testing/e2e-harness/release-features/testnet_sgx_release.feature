@hardware-sgx @release
Feature: Published testnet SGX release
  The exact image promoted for testnet must contain the protected-job signature,
  execute on SGX hardware, preserve same-signer sealed identity across restart,
  and fail closed when artifacts or signer identity change.

  Scenario: Published signed enclave image is immutable and restart-safe
    Given an exact signed testnet SGX bundle and published image
    Then the signed bundle and immutable runtime layout verify
    When the published image probes hardware SGX
    Then the hardware report matches the signed enclave measurements
    When the published image starts twice with one sealed identity directory
    Then the second start restores the same-signer sealed identity
    When an artifact in the signed bundle is substituted
    Then release verification rejects the substituted artifact
    When the rendered manifest is signed by a different test key
    Then the prior sealed identity is not silently restored by the different signer
    And canonical hardware release evidence is written
