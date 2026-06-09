// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// Validator agent-reward distribution surface. The Rust dispatch is
/// synthesized at compile time from `#[contract_public(...)]` annotations
/// in `crates/core/agentreward/src/precompile.rs` (the `#[contract_dispatch]`
/// macro pilot). The drift test in that crate keeps the two in sync.
interface IAgentReward {
    function getClaimableBalance(address account) external view returns (uint256);
    function claimReward(uint256 amount) external returns (uint256);
}
