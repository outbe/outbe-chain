@tee @min-validators-4
Feature: Encrypted tribute projection
  @pfs-001-01
  Scenario: A successful tribute is persisted by every validator
    Given a fresh localnet with a 6-block voting window
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    And every validator serves the same independently verified compressed tribute

  @pfs-001-02
  Scenario: An unknown tribute in an existing day has a verifiable absence proof
    Given a fresh localnet with a 6-block voting window
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    And every validator proves an unknown tribute absent from the existing collection

  @pfs-001-03
  Scenario: An unknown tribute day has a verifiable collection absence proof
    Given a fresh localnet with a 6-block voting window
    Then every validator proves an unknown tribute collection absent
    And no validator projects a tribute

  @pfs-001-05
  Scenario: A duplicate logical offer for the same owner and day is rejected
    Given a fresh localnet with a 6-block voting window
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    When the operator submits a duplicate logical tribute offer with different parameters for the same day
    Then the duplicate is rejected without changing tribute state or projections
