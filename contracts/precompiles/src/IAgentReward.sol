// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// Reward surface. Each pool and emission day has one rate. An agent claims its days, and the
/// claim issues the reward as a Gem. The `#[contract_dispatch]` macro synthesizes the Rust dispatch
/// at compile time from `#[contract_public(...)]` annotations in
/// `crates/core/agentreward/src/precompile.rs`. The drift test in that crate keeps the two in sync.
/// Every `promisLoadMinor` is a PROMIS load in protocol units (6 decimals); a rate has 18 decimal
/// places; pools are 0 = WAA, 1 = SRA, 2 = CCA, 3 = Validator.
interface IAgentReward {
    error OfferCounterOverflow();
    error AgentRewardRateAlreadySet(uint8 pool, uint32 emissionDay);
    error AgentRewardRateMissing(uint8 pool, uint32 emissionDay);
    error InvalidRewardDays(uint256 count);
    error NoAgentRewardWeight(uint32 emissionDay);
    error NoAgentReward();
    error AgentRewardDeadlinePassed(uint64 deadline);
    error AgentRewardNotExpired(uint64 deadline);
    error RewardDeadlineOverflow();
    error RedeemPriceUnavailable(uint16 isoCode);
    error EntryPriceOverflow();
    error UnknownRewardPool(uint8 value);

    event AgentRewardRateSet(
        uint32 indexed emissionDay, uint8 indexed pool, uint256 rate, uint256 allocatedMinor, uint64 deadline
    );
    event AgentRewardRateFailed(uint32 indexed emissionDay, uint8 indexed pool, uint256 amountMinor);
    event AgentRewardRedeemed(address indexed account, uint8 indexed pool, uint256 promisLoadMinor, uint256 gemId);
    event AgentRewardExpired(address indexed account, uint8 indexed pool, uint32 emissionDay, uint256 promisLoadMinor);
    event AgentRewardLimitReturned(uint32 indexed emissionDay, uint8 indexed pool, uint256 amountMinor);

    /// @notice The rate of `pool` in `emissionDay`, its deadline and the part of the allocated amount
    /// that no claim or expiry took yet. All zero when the day has no rate or no weight left.
    function getAgentRewardRate(uint8 pool, uint32 emissionDay)
        external
        view
        returns (uint256 rate, uint64 deadline, uint256 unclaimedMinor);
    /// @notice The weight of `account` in `pool` and `emissionDay` and its reward at the rate of the day.
    function getAgentReward(address account, uint8 pool, uint32 emissionDay)
        external
        view
        returns (uint256 weight, uint256 promisLoadMinor);
    /// @notice Anyone may send the rewards of `account` in `pool` for `emissionDays`, at or after
    /// their deadline, to the Promis Limit. At most 64 days. The caller receives no reward.
    function expireAgentReward(address account, uint8 pool, uint32[] calldata emissionDays)
        external
        returns (uint256 promisLoadMinor);
    /// @notice Claims the caller's rewards in `pool` for `emissionDays` (1 to 64 days, each before
    /// its deadline) as one Gem in the caller's `issuanceCurrency` and `referenceCurrency`
    /// (ISO 4217 numeric). `referenceCurrency` must be an Oracle reference currency. The entry price
    /// is the 24-hour COEN VWAP of the redemption hour in it less the agent discount; the call price
    /// marks up the full VWAP. The Gem joins the sub-series of the redemption hour. An agent gets one
    /// reward Gem of a sub-series in each block, so a second redemption in the same sub-series and
    /// block reverts `AlreadyExists`.
    /// @return gemId the Gem issued for the caller.
    function redeemAgentReward(
        uint8 pool,
        uint32[] calldata emissionDays,
        uint16 issuanceCurrency,
        uint16 referenceCurrency
    ) external returns (uint256 gemId);
}
