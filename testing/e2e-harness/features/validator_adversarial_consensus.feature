@tee @min-validators-4 @adversarial @risk-d-04
Feature: Honest validators reject a malicious committee boundary
  # The adversarial binary is built from the exact source revision in a detached
  # worktree. Its test-only patch changes only the selected proposer's boundary
  # artifact; it must attempt to commit bad committee state on honest nodes, not
  # merely crash or isolate the malicious process.

  Scenario: A malicious leader cannot commit a boundary that omits an active validator
    Given a fresh localnet has completed a valid pending reshare boundary
    And the next view-one leader is selected from the committed VRF seed
    When that leader restarts with the omit-active-boundary adversarial binary
    And it proposes a self-consistent boundary that omits one active validator
    Then every honest validator rejects the malicious boundary without changing committee state
    And a later honest leader commits the original expected boundary
    And honest committee membership, shares, snapshots, and state roots converge
    When the malicious validator restarts with the normal binary
    Then it catches up to the honest finalized state
    And exactly one adversarial boundary injection is recorded
