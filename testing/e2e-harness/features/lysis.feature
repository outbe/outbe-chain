@lysis @tee @sgx-no-attest @sudo @min-validators-4 @price-oracle
Feature: Confidential Lysis called by Metadosis
  @nod-settlement
  Scenario: A public Tribute becomes an encrypted Nod and its owner receives COEN
    Given a fresh four-validator native Lysis localnet
    Then the controlled COEN USD quote is finalized through the real price feeder
    When an L2 network is registered through governance with an inbox contract and no pinned key
    And a user of the registered L2 submits an encrypted Tribute with ZKP and WAA and SRA beneficiaries
    Then the tribute transaction succeeds and supply becomes one
    And the WAA and SRA beneficiaries have no reward before their execution UTC day settles
    And every validator stores the same Tribute and indexes in EVM state
    And every validator verifies the same Tribute against finalized EVM state
    When a production FullNode with its own enclave syncs from the committee
    Then the follower reaches the committee finalized checkpoint with matching hash and state root
    When the committee logical clock reaches the native Lysis processing time
    Then Metadosis completes confidential Lysis and stores ready Nods on every validator
    And the encrypted NOD matches its canonical body, public views and native issuance event
    And the FullNode verifies the same encrypted Nod against finalized EVM state
    And the follower reaches the committee finalized checkpoint with matching hash and state root
    And the public Tribute owner settles its Nod and redeems its exact Gratis into COEN
    When the offer execution UTC day reaches its next ProtocolCycle settlement
    Then every validator observes the same nonzero WAA and SRA AgentReward
    When both beneficiaries claim their complete AgentReward as Gems with paid transactions, WAA issued in 458 against 840 and SRA issued in 978 against 840
    Then the paid Gem claims clear both claimables and move no AgentReward native COEN
    And validator 0 settles its protocol reward Gem and redeems its exact Promis into COEN
