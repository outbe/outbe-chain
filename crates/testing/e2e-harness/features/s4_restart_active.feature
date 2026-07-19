@tee @min-validators-4
Feature: Active validator restarts without a new DKG ceremony
  # An ACTIVE validator's BLS share is
  # persisted to its keys-dir on disk (not the enclave). Killing only the node —
  # the enclave container stays up — and restarting it with the same keys-dir must
  # resume signing from the recovered share with NO fresh DKG ceremony.

  @pfs-006-09
  Scenario: Restarted active node resumes signing from its persisted share
    Given a fresh localnet with a 6-block voting window
    When a joiner reaches active with a persisted share
    And the node is killed and restarted with the same keys
    Then it resumes signing from the persisted share without a new ceremony

  @pfs-006-09
  Scenario: Entire committee recovers after all enclaves restart
    Given a fresh localnet with a 6-block voting window
    When the entire committee and its enclaves are stopped and restarted
    Then all validators recover sealed TEE state and resume finalization

  @pfs-006-09
  Scenario: Completed DKG survives a joining node restart before activation
    Given a fresh localnet with a 6-block voting window
    When a joiner completes DKG and waits below the activation boundary
    And the joining node and enclave restart before activation
    Then the recovered pending DKG activates once and consensus continues

  @pfs-006-09
  Scenario: In-flight DKG survives a joining node and enclave restart
    Given a fresh localnet with a 6-block voting window
    When a joining validator is restarted during its DKG ceremony
    Then the old committee stays live and a later DKG activates the joiner once
