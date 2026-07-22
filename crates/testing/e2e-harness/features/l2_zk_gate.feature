@tee @min-validators-4
Feature: L2Registry zk signature gate on tribute offers
  @pfs-001-10
  Scenario: An unsigned offer from a zk-enabled L2 operator is rejected and the toggle restores it
    Given a fresh localnet with a 6-block voting window
    When an L2 network is registered for the operator with zk enabled
    And the operator submits an encrypted tribute offer without an L2 signature
    Then the offer is rejected and tribute supply stays zero
    When zk verification is disabled for the registered L2 network
    And an operator submits one encrypted tribute offer
    Then the tribute transaction succeeds and supply becomes one

  @pfs-001-11
  Scenario: A zkMerkleRoot signed with the registered network key passes the gate
    Given a fresh localnet with a 6-block voting window
    When an L2 network is registered for the operator with zk enabled
    And the operator submits an encrypted tribute offer with a valid L2 signature
    Then the tribute transaction succeeds and supply becomes one
