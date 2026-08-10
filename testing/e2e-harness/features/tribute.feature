@tee @gramine-direct @min-validators-4
Feature: Tribute admission, projection, and proofs
  One canonical Tribute flow owns successful ingress, projection, presence and
  absence proofs, and duplicate protection. ZK policy branches stay separate
  because they exercise different admissibility rules.

  @pfs-001-01 @pfs-001-02 @pfs-001-03 @pfs-001-05
  Scenario: One public Tribute has complete projection and duplicate protection
    Given a fresh localnet with a 6-block voting window
    Then every validator proves an unknown tribute collection absent
    And no validator projects a tribute
    When an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one
    And every validator projects the same tribute and indexes
    And every validator serves the same independently verified compressed tribute
    And every validator proves an unknown tribute absent from the existing collection
    When the operator submits a duplicate logical tribute offer with different parameters for the same day
    Then the duplicate is rejected without changing tribute state or projections

  @pfs-001-10
  Scenario: An unsigned ZK-gated offer is rejected and disabling the gate restores ingress
    Given a fresh localnet with a 6-block voting window
    When an L2 network is registered for the operator with zk enabled
    And the operator submits an encrypted tribute offer without an L2 signature
    Then the offer is rejected and tribute supply stays zero
    When zk verification is disabled for the registered L2 network
    And an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one

  @pfs-001-11
  Scenario: A registered L2 network admits a correctly signed FullProof
    Given a fresh localnet with a 6-block voting window
    When an L2 network is registered for the operator with zk enabled
    And the operator submits an encrypted tribute offer with a valid ZK proof and L2 signature
    Then the tribute transaction succeeds and supply becomes one
