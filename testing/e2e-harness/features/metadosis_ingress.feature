@min-validators-4
Feature: Metadosis public ingress before OCOMP activation
  # The submitLysisResult selector is reachable from arbitrary public calldata.
  # Before the OCOMP lifecycle activates it must revert like any failed
  # transaction and must never abort payload building: a Fatal here is the
  # 2026-05-15 network-wide proposer-stall incident class.

  @metadosis-inactive-lysis-ingress
  Scenario: A Lysis result vote before OCOMP activation reverts and the chain advances
    Given a fresh localnet with a 20-block voting window
    And the committee has reached a usable height
    When an operator submits a Lysis result vote before OCOMP activation
    Then the vote transaction reverts with the lifecycle-inactive code and finalization continues
