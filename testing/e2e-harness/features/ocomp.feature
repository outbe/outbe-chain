@ocomp @min-validators-4
Feature: Off-chain computation process topology
  @ocomp-dynamic-admission
  Scenario: A non-voting FullNode becomes the fifth OCOMP validator only after activation
    Given a fresh four-validator OCOMP measurement localnet
    When a fifth node syncs as a non-voting FullNode
    Then the fifth node has canonical state parity without OCOMP vote capability
    When the synced node completes OCOMP-ready validator admission
    Then the certified boundary adds exactly one fifth OCOMP validator domain

  @ocomp-dynamic-overlap
  Scenario: A live four-validator job keeps its snapshot while the fifth validator joins a new job
    Given a fresh four-validator OCOMP dynamic-membership localnet with two scheduled jobs
    When one public Tribute is submitted for each scheduled OCOMP job
    And validators 2 and 3 OCOMP supervisors are stopped before the job
    Then job A opens with four members and quorum three while job B remains scheduled
    When a fifth node syncs as a non-voting FullNode
    Then the fifth node has canonical state parity without OCOMP vote capability
    And the FullNode independently materializes job A without voting
    When the synced node completes OCOMP-ready validator admission
    Then the certified boundary adds exactly one fifth OCOMP validator domain
    And job B opens with five members and quorum four while job A remains four of three
    When validator 2 OCOMP supervisor restarts and completes both pinned quorums
    Then the FullNode result for job A matches the canonical quorum result
    And both deadlines record validator 3 missing and keep the chain live after jailing it

  @ocomp-int-024
  Scenario: A supervisor failure is isolated from consensus
    Given a fresh four-validator OCOMP measurement localnet
    Then all four OCOMP domains run their node-facing production roles
    And each OCOMP domain owns one authenticated production worker
    When validator 0 OCOMP supervisor is stopped through the typed fault control
    Then consensus finality advances while only that supervisor remains stopped
    And validator 0 OCOMP supervisor restarts through the typed topology

  @ocomp-fork-restart
  Scenario: Validator recovery preserves the fork across every height boundary
    Given the canonical four-validator OCOMP Final devnet before H
    When validator 0 restarts before, across, and after the OCOMP fork height
    Then the OCOMP evidence records successful H-1, H, and H+1 recovery
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute

  @ocomp-fork-mismatch
  Scenario: A distinct immutable fork install cannot join the canonical committee
    Given the canonical four-validator OCOMP Final devnet before H
    When validator 0 restarts with a different valid immutable OCOMP fork install
    Then the canonical committee finalizes through H while the mismatched validator stays before H
    When validator 0 returns to the canonical OCOMP fork install
    And an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    Then Metadosis creates one finalized JobIntent from that public Tribute
