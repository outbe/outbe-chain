@min-validators-4
Feature: Stablecoin Factory V1 product lifecycle
  @pfs-010-01 @pfs-010-02 @pfs-010-03 @pfs-010-04
  Scenario: A governed stablecoin survives full ledger use and committee restart
    Given a fresh stablecoin localnet with a 20-block voting window
    And the committee has reached a usable height
    When a funded issuer creates a shared policy and validators approve its bonded stablecoin proposal
    Then the token is registered with one HookEvents creation receipt and the bond is refunded
    When the issuer exercises mint transfer permit memo pause freeze and forced transfer
    Then the stablecoin ledger has the expected final state on every validator
    When a second issuer proposes the same global ticker
    Then the duplicate ticker is rejected without a proposal or bond debit
    When the entire stablecoin committee restarts
    Then stablecoin historical and current reads remain identical on every validator
