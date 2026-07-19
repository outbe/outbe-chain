@min-validators-4
Feature: EIP-7702 ZeroFee sponsorship and paid fallback
  # One live scenario keeps the expensive localnet setup shared while each step
  # exposes a distinct protocol postcondition. It uses native alloy signing and
  # RPC through the Rust World; Foundry cast and the legacy shell runner are not
  # involved.

  @pfs-007-01 @pfs-007-02 @pfs-007-03 @pfs-007-04 @pfs-007-05 @pfs-007-06
  Scenario: A delegated account consumes its free quota and can still pay
    Given a fresh localnet with a 20-block voting window
    And the committee has reached a usable height
    Then Pectra and the ZeroFee views are ready
    When a funded fresh account delegates to ZeroFee with EIP-7702
    Then the exact ZeroFee delegation designator is installed
    When the account submits eight eligible sponsored reward calls
    Then all eight calls succeed without fees and consume the full quota
    When the account submits a ninth eligible sponsored reward call
    Then the ninth call is mined as ZeroFee soft failure 110 without a fee
    When the quota-exhausted account submits the same call with a priority fee
    Then the paid call succeeds, charges a fee, and does not change the quota
    And the product CLI emits a canonical ZeroFee authorization

  @pfs-007-07 @pfs-007-08 @tee
  Scenario: Exact replay is rejected and exhausted quota survives validator and committee restarts
    Given a fresh localnet with a 20-block voting window
    And the committee has reached a usable height
    When a funded fresh account delegates to ZeroFee with EIP-7702
    Then the exact ZeroFee delegation designator is installed
    When the account submits eight eligible sponsored reward calls
    Then all eight calls succeed without fees and consume the full quota
    When the exact included ZeroFee delegation transaction is replayed
    Then the replay is rejected without changing delegation or quota
    When validator "validator-3" restarts after quota exhaustion
    Then the exhausted ZeroFee state is identical on every validator
    When the entire committee restarts after quota exhaustion
    Then the exhausted ZeroFee state is identical on every validator
    When the exact included ZeroFee delegation transaction is replayed
    Then the replay is rejected without changing delegation or quota
    And the exhausted ZeroFee state is identical on every validator
    When the quota-exhausted account submits the same call with a priority fee
    Then the paid call succeeds, charges a fee, and does not change the quota
