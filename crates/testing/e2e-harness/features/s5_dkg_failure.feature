@tee @min-validators-4
Feature: DKG reshare failure keeps the old committee live
  # A frozen 4->5 reshare is starved below
  # player_threshold (joiner + one committee validator offline); the existing
  # committee keeps finalizing on its 3-of-4 quorum with no hard-halt. Restoring
  # the downed validator lets a later retry complete and the set reaches 5.

  @pfs-006-04
  Scenario: Stalled reshare does not halt the chain, and recovers when restored
    Given a fresh localnet with a wide DKG activation grace
    When a staked joiner freezes a 4-to-5 reshare target
    And the reshare loses quorum before it can complete
    Then the old committee keeps finalizing through the stalled reshare
    When the downed validator is restored
    Then the reshare completes and the active set reaches 5

  # The current protocol has no finalized forfeiture transition for a frozen
  # target. If one of its required players never returns, the old committee may
  # continue only through the bounded VRF grace window; it must never partially
  # activate the target and must fail closed once that window expires.
  @pfs-006-04
  Scenario: Permanently unavailable frozen-target player fails closed at VRF expiry
    Given a fresh localnet with a short DKG activation grace
    When a staked joiner freezes a 4-to-5 reshare target
    And the frozen-target joiner and one validator remain offline
    Then the old committee finalizes without partial activation until VRF expiry
    And the surviving validators exit with the frozen-target expiry error
