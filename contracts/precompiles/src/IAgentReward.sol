// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// Agent-reward distribution surface. A claim mints the reward as a Gem:
/// the WAA pool mints a Wallet Gem, the SRA pool an Sra Gem. The Rust dispatch is
/// synthesized at compile time from `#[contract_public(...)]` annotations
/// in `crates/core/agentreward/src/precompile.rs` (the `#[contract_dispatch]`
/// macro pilot). The drift test in that crate keeps the two in sync.
interface IAgentReward {
    function getClaimableBalance(address account) external view returns (uint256);
    function getPoolClaimableBalance(address account, uint8 pool) external view returns (uint256);
    /// @notice Mints the caller's whole balance in `pool` (0 = WAA, 1 = SRA) as a Gem.
    /// @return gemId the Gem minted for the caller.
    function claimReward(uint8 pool) external returns (uint256 gemId);
}
