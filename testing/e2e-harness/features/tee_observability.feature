@tee @min-validators-4
Feature: TEE enclave observability and session identity
  # The enclave sidecar is a synchronous protocol component: every validator
  # decrypts tribute offers through it inside block execution. This feature
  # owns the OBSERVABILITY and SESSION-IDENTITY contract around that channel
  # (2026-08-22 testnet incident follow-up):
  #   - every enclave answers the node's canary decrypt and reports health
  #     through `outbe_consensusStatus.enclave`;
  #   - every served request leaves a telemetry line in the enclave log;
  #   - an enclave-sidecar restart that preserves the sealed identity is
  #     survivable WITHOUT restarting the validator (session reconnect with
  #     identity re-validation);
  #   - an enclave that comes back with DIFFERENT keys is refused permanently
  #     (fail-closed revocation), visible in the canary state — never silently
  #     adopted.
  # Membership/onboarding stays with validator_lifecycle; the permanent
  # attested-key restart contract stays with tee_onboarding.

  Scenario: A restarted enclave reconnects, and a re-keyed one is refused
    Given a fresh localnet with a 6-block voting window
    And the committee has reached a usable height
    Then every validator reports a ready enclave canary
    And every enclave log shows per-request telemetry
    When validator-1's enclave sidecar restarts with its sealed identity
    Then validator-1's enclave session reconnects without a node restart
    When validator-1's enclave restarts with a fresh identity
    Then validator-1 reports a refused enclave session while the rest stay ready
