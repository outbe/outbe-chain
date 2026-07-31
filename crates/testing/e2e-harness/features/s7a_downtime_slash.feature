@tee @min-validators-4
Feature: Liveness survives a downed validator
  # Killing one committee validator drops the set to 3-of-4. The surviving
  # quorum keeps finalizing, the absent voter crosses the dev felony threshold,
  # and the resulting burn is checked against every affected accounting surface.

  @pfs-006-06
  Scenario: Chain stays live after a validator is killed
    Given a fresh localnet with a 6-block voting window
    And the slashing config is readable
    And validator "validator-3" starts active
    When validator "validator-3" is killed
    Then the committee keeps finalizing until the validator is slashed exactly once
    And continued downtime does not slash the validator twice
