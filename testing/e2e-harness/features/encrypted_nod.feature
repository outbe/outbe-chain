@encrypted-nod @ocomp @tee @sgx-no-attest @sudo @min-validators-4 @price-oracle
Feature: Encrypted NOD entitlement and private Gratis exercise
  Scenario: A relayed offer creates a self-contained encrypted NOD and only its owner receives Gratis
    Given a fresh four-validator OCOMP public capacity localnet
    Then the controlled COEN USD quote is finalized through the real price feeder
    When an operator offers a Tribute for a different validator as the encrypted NOD owner
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    When the committee logical clock reaches the public capacity processing time
    Then Metadosis creates one finalized JobIntent from that public Tribute
    When the production OCOMP domains process that finalized JobIntent
    Then three matching validator domains atomically apply Lysis and create the Nod
    And the encrypted NOD matches its protected calldata, canonical body, public views and events
    When the encrypted NOD is paid and exercised through a relayer across restarts
    Then the removed Gratis supply selector rejects calls on every validator
