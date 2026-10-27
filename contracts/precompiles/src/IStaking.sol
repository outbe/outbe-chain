// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IStaking
/// @notice Staking precompile at 0x000000000000000000000000000000000000EE02
/// @dev Stake and payout amounts are COEN base units.
interface IStaking {
    /// Caller-facing domain revert.
    error AmountZero();
    error ThirdPartyStakingNotSupported();
    error StakeExceedsMaxStakePercent();
    error InsufficientStakedAmount();
    error UnjailRequiresLiveEnclaveBinding();
    error UnjailInsufficientStake(uint256 have, uint256 need);
    error InvalidSlashPercent();
    error MsgValueMismatch();
    error UnbondingCompletionTimestampOverflow();
    error SlashedWithdrawalDelayOverflow();
    error StakeAccountingUnderflow(address validator);
    error StakeAccountingOverflow(address validator);
    error TotalStakeAccountingOverflow();
    error SlashAmountOverflow();
    error MissingValidatorHistory();
    error ValidatorRegistryExceedsConsensusBound(uint32 count, uint32 bound);
    error SweepCountOverflow(string value);
    error TooManyUnbondingEntries(address validator, uint32 maxEntries);
    error UnbondingIndexOverflow();
    error ClaimableAmountOverflow();

    /// `amount` is the deposit. `bonded` is that validator's stake after the credit.
    event Staked(address indexed validator, uint256 amount, uint256 bonded);
    /// `index` is the existing queue slot and can be reused after that life ends.
    /// `completeTime` is the unix completion time in seconds. `automatic` is true
    /// when the protocol queues the remaining stake of an already-Unbonding validator.
    event UnbondingStarted(
        address indexed validator, uint32 index, uint256 amount, uint64 completeTime, bool automatic
    );
    /// Remaining amount and effective completion time after a penalty change.
    /// A money change or a deadline change emits this, including a zero-rounded amount.
    event UnbondingUpdated(address indexed validator, uint32 index, uint256 remaining, uint64 completeTime);
    /// Paid amount for one queue entry. A zero payout does not emit this event.
    event UnbondedClaimed(address indexed validator, uint32 index, uint256 paid);
    /// Amounts actually deducted from bonded stake and from the unbonding queue.
    event Slashed(address indexed validator, uint256 bondedSlashed, uint256 unbondingSlashed);

    /// Register `validatorAddress` (or re-register an `INACTIVE` validator) and bond
    /// `amount` in a single atomic transaction. The caller must be the validator.
    /// The call must carry `msg.value == amount` and `amount >= getMinStake()`.
    function stake(
        address validatorAddress,
        uint256 amount,
        bytes calldata consensusPubkey,
        bytes32 radicleNodeId,
        bytes calldata blsRegistrationSignature,
        uint8 p2pVersion,
        bytes calldata p2pEncoded
    ) external payable;
    /// Add `amount` to an already-registered validator's bonded stake.
    /// The caller must be `validatorAddress` and `msg.value == amount`.
    function topup(address validatorAddress, uint256 amount) external payable;
    function unstake(uint256 amount) external;
    function claimUnbonded() external;
    /// Unjail a JAILED validator. The caller must be that validator, and the
    /// caller's bonded stake must be at least the minimum stake. The transition
    /// is JAILED to WaitingForReadiness. `IValidatorSet.confirmValidatorReady()`
    /// then moves it to Joining. A later validated committee boundary can
    /// promote it to Active.
    function unjailValidator() external;
    function getStake(address validator) external view returns (uint256);
    function getTotalStaked() external view returns (uint256);
    /// Minimum bonded stake, in COEN base units.
    function getMinStake() external view returns (uint256);
    /// Unbonding period in seconds.
    function getUnbondingPeriod() external view returns (uint64);
    /// Integer percent of total stake. A stored value outside (0, 100) disables the cap.
    function getMaxStakePercent() external view returns (uint64);
    /// Effective delay in seconds before a slashed unbonding entry can be claimed.
    /// A stored delay of zero uses `2 * unbondingPeriod`. Overflow reverts with
    /// `SlashedWithdrawalDelayOverflow`.
    function getSlashedWithdrawalDelay() external view returns (uint64);
}
