// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// Agent-reward distribution surface. A claim issues the reward as a Gem:
/// the WAA pool issues a Wallet Gem, SRA an Sra Gem, and CCA a Cca Gem. The `#[contract_dispatch]`
/// macro pilot synthesizes the Rust dispatch at compile time from `#[contract_public(...)]`
/// annotations in `crates/core/agentreward/src/precompile.rs`. The drift test in that crate
/// keeps the two in sync.
interface IAgentReward {
    event RewardAccrued(address indexed cca, uint32 indexed utcDay, uint256 amount);
    event RewardsClaimed(address indexed cca, uint256 amount);

    function getClaimableBalance(address account) external view returns (uint256);
    function getPoolClaimableBalance(address account, uint8 pool) external view returns (uint256);
    /// @notice Issues `amount` of the caller's balance in `pool` (0 = WAA, 1 = SRA, 2 = CCA)
    /// as a Gem. An `amount` of zero claims the whole pool balance. What is left
    /// keeps accruing and cannot be forfeited. The caller controls its own risk
    /// through the size of the claim.
    /// @return gemId the Gem issued for the caller.
    function claimReward(uint8 pool, uint256 amount) external returns (uint256 gemId);
}
