// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// Reward distribution surface. A claim issues the reward as a Gem. The `#[contract_dispatch]`
/// macro pilot synthesizes the Rust dispatch at compile time from `#[contract_public(...)]`
/// annotations in `crates/core/agentreward/src/precompile.rs`. The drift test in that crate
/// keeps the two in sync. Every `promisLoadMinor` is a PROMIS load in protocol units
/// (6 decimals); pools are 0 = WAA, 1 = SRA, 2 = CCA, 3 = Validator.
interface IAgentReward {
    error DistributeDailyOverflow();
    error DistributionOverflow();
    error ClaimableTotalOverflow();
    error PoolTotalMismatch();
    error ClaimableRewardsOverflow();
    error NoClaimableBalance();
    error InsufficientClaimableBalance();
    error MissingPreviousDayPrice(uint16 value);
    error UnknownRewardPool(uint8 value);
    error AccrualJobMissing(uint64 value);

    event RewardAccrued(address indexed account, uint8 indexed pool, uint32 indexed utcDay, uint256 promisLoadMinor);
    event RewardsClaimed(address indexed account, uint8 indexed pool, uint256 promisLoadMinor, uint256 gemId);

    function getClaimableBalance(address account) external view returns (uint256 promisLoadMinor);
    function getPoolClaimableBalance(address account, uint8 pool) external view returns (uint256 promisLoadMinor);
    /// @notice The pool's claimable balances, summed over all accounts.
    function getPoolClaimableTotal(uint8 pool) external view returns (uint256 promisLoadMinor);
    /// @notice The part of queued WAA or SRA day pools not yet credited; zero for CCA and validator.
    function getPoolPendingTotal(uint8 pool) external view returns (uint256 promisLoadMinor);
    /// @notice Issues `promisLoadMinor` of the caller's balance in `pool`
    /// as a Gem in the caller's `issuanceCurrency` and `referenceCurrency` (ISO 4217 numeric).
    /// `referenceCurrency` must be an Oracle reference currency. The entry price is the
    /// previous UTC day's COEN VWAP in it less the agent discount; the call price marks
    /// up the full VWAP. `issuanceCurrency` is any code in 1..=999.
    /// A `promisLoadMinor` of zero claims the whole pool balance. What is left
    /// keeps accruing and cannot be forfeited. The caller controls its own risk
    /// through the size of the claim.
    /// @return gemId the Gem issued for the caller.
    function claimReward(uint8 pool, uint256 promisLoadMinor, uint16 issuanceCurrency, uint16 referenceCurrency)
        external
        returns (uint256 gemId);
}
