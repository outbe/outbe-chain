@tee @min-validators-4
Feature: Byzantine committee pre-announces
  A leader proposing a forged CommitteePreAnnounce after the successor DKG
  completes but before its activation boundary must be rejected by honest
  validators, so finalized history only carries the genuine committee. Run
  with OUTBE_TEST_BYZANTINE_PREANNOUNCE=1 and the E2E node feature set.

  @byzantine-preannounce
  Scenario: Forged committee pre-announces from byzantine leaders never finalize
    Given every validator is armed to forge committee pre-announces
    And a fresh localnet with a short epoch
    When the committee drives past a reshare
    Then finalized history authenticates from genesis through two committee handoffs
    And forged pre-announces were proposed and every one was rejected
