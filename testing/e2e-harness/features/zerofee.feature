@min-validators-4
Feature: EIP-7702 ZeroFee sponsorship, replay, and daily quota
  The main scenario owns the complete successful quota lifecycle and its
  restart/replay guarantees. Invalid authorization and day rollover are
  separate state transitions.

  @pfs-007-01 @pfs-007-02 @pfs-007-03 @pfs-007-04 @pfs-007-05 @pfs-007-06 @pfs-007-07 @pfs-007-08 @tee
  Scenario: Delegated quota survives replay and restart before paid fallback
    Given a fresh localnet with a 20-block voting window
    And the committee has reached a usable height
    Then Pectra and the ZeroFee views are ready
    When an account with one atomic COEN bootstraps its ZeroFee delegation
    Then the exact ZeroFee delegation designator is installed
    When the exact included ZeroFee bootstrap transaction is replayed
    Then the bootstrap replay is rejected without changing account or quota state
    When the account submits eight eligible sponsored reward calls
    Then all eight calls succeed without fees and consume the full quota
    When the exact included sponsored ZeroFee transaction is replayed
    Then the replay is rejected without changing delegation or quota
    When validator "validator-3" restarts after quota exhaustion
    Then the exhausted ZeroFee state is identical on every validator
    When the entire committee restarts after quota exhaustion
    Then the exhausted ZeroFee state is identical on every validator
    When the exact included ZeroFee bootstrap transaction is replayed
    Then the bootstrap replay is rejected without changing account or quota state
    When the exact included sponsored ZeroFee transaction is replayed
    Then the replay is rejected without changing delegation or quota
    When the account submits a ninth eligible sponsored reward call
    Then the ninth call is mined as ZeroFee soft failure 110 without a fee
    When the quota-exhausted account submits the same call with a priority fee
    Then the paid call succeeds, charges a fee, and does not change the quota
    And the product CLI emits a canonical ZeroFee authorization

  @pfs-007-09 @pfs-007-10 @pfs-007-11
  Scenario: Invalid, wrong-target, and conflicting authorizations cannot obtain sponsorship
    Given a fresh localnet with a 20-block voting window
    And the committee has reached a usable height
    When a funded account submits an EIP-7702 authorization for the wrong chain
    Then the invalid authorization leaves delegation and ZeroFee quota unset
    When the account delegates to a non-ZeroFee target and submits a sponsored-shaped call
    Then the wrong-target call receives no sponsorship and leaves ZeroFee quota unchanged
    When a stale conflicting authorization attempts to replace the wrong target
    Then the conflicting authorization leaves the prior delegation and ZeroFee quota unchanged

  @pfs-007-12
  Scenario: Exhausted quota resets lazily across the worldwide-day boundary
    Given a fresh localnet near the next UTC worldwide-day boundary
    And the committee has reached a usable height
    Then the controlled COEN USD quote is finalized through the real price feeder
    When an account with one atomic COEN bootstraps its ZeroFee delegation
    Then the exact ZeroFee delegation designator is installed
    When the exact included ZeroFee bootstrap transaction is replayed
    Then the bootstrap replay is rejected without changing account or quota state
    When the account submits eight eligible sponsored reward calls
    Then all eight calls succeed without fees and consume the full quota
    When the chain crosses into the next worldwide day
    Then ZeroFee quota resets lazily and the first new-day sponsored call succeeds on every validator
