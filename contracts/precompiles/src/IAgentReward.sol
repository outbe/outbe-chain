// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// Reward distribution surface. A redemption issues the reward as a Gem. The `#[contract_dispatch]`
/// macro pilot synthesizes the Rust dispatch at compile time from `#[contract_public(...)]`
/// annotations in `crates/core/agentreward/src/precompile.rs`. The drift test in that crate
/// keeps the two in sync. Every `promisLoadMinor` is a PROMIS load in protocol units
/// (6 decimals); pools are 0 = WAA, 1 = SRA, 2 = CCA, 3 = Validator.
interface IAgentReward {
    error OfferCounterOverflow();
    error DistributeDailyOverflow();
    error DistributionOverflow();
    error AgentRewardTotalOverflow();
    error PoolTotalMismatch();
    error AgentRewardOverflow();
    error NoAgentReward();
    error InsufficientAgentReward();
    error AgentRewardDeadlinePassed(uint64 deadline);
    error AgentRewardNotExpired(uint64 deadline);
    error RewardDeadlineOverflow();
    error RewardDeadlineMissing();
    error RedeemPriceUnavailable(uint16 isoCode);
    error EntryPriceOverflow();
    error UnknownRewardPool(uint8 value);
    error AccrualJobMissing(uint64 value);

    event AgentRewardAdded(
        address indexed account, uint8 indexed pool, uint32 indexed emissionDay, uint256 promisLoadMinor
    );
    event AgentRewardRedeemed(address indexed account, uint8 indexed pool, uint256 promisLoadMinor, uint256 gemId);
    event AgentRewardExpired(address indexed account, uint8 indexed pool, uint256 promisLoadMinor);
    event AgentRewardLimitReturned(uint32 indexed emissionDay, uint8 indexed pool, uint256 amountMinor);

    /// @notice Includes expired balances until they are returned to the Promis Limit.
    function agentRewardTotal(address account) external view returns (uint256 promisLoadMinor);
    /// @notice Includes expired balances until they are returned to the Promis Limit.
    function getAgentReward(address account, uint8 pool) external view returns (uint256 promisLoadMinor);
    /// @notice Expiration timestamp in seconds for this account and pool; zero for an empty balance.
    function agentRewardDeadline(address account, uint8 pool) external view returns (uint64 deadline);
    /// @notice The agent rewards of the pool, summed over all accounts.
    function poolAgentRewardTotal(uint8 pool) external view returns (uint256 promisLoadMinor);
    /// @notice The part of queued WAA or SRA day pools not yet credited; zero for CCA and validator.
    function poolPendingAgentRewardTotal(uint8 pool) external view returns (uint256 promisLoadMinor);
    /// @notice Anyone may return the whole expired balance to the Promis Limit at or after its deadline.
    /// An empty balance returns zero without an event. The caller receives no reward.
    function expireAgentReward(address account, uint8 pool) external returns (uint256 promisLoadMinor);
    /// @notice Issues `promisLoadMinor` of the caller's balance in `pool`
    /// as a Gem in the caller's `issuanceCurrency` and `referenceCurrency` (ISO 4217 numeric).
    /// `referenceCurrency` must be an Oracle reference currency. The entry price is the
    /// 24-hour COEN VWAP of the redemption hour in it less the agent discount; the call price
    /// marks up the full VWAP. The Gem joins the sub-series of the redemption hour. `issuanceCurrency` is any code in 1..=999.
    /// A `promisLoadMinor` of zero redeems the whole pool balance. A redemption must precede
    /// the balance deadline. A successful partial redemption renews the remaining balance's
    /// deadline for 365 days; a full redemption clears it. The first positive credit starts
    /// a 365-day lifetime, while later credits preserve its deadline, even after expiration.
    /// An agent gets one reward Gem of a sub-series in each block, so a second redemption in the
    /// same sub-series and block reverts `AlreadyExists`. A merchant Gem never blocks it.
    /// @return gemId the Gem issued for the caller.
    function redeemAgentReward(uint8 pool, uint256 promisLoadMinor, uint16 issuanceCurrency, uint16 referenceCurrency)
        external
        returns (uint256 gemId);
}
