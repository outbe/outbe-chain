@tee @min-validators-4
Feature: Encrypted tribute projection
  Scenario: A successful tribute is persisted by every validator
    Given a fresh localnet with a 6-block voting window
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
