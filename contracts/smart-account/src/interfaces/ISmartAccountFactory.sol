// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @notice Factory for fresh accounts with delayed owner execution and per-token CCA permissions.
interface ISmartAccountFactory {
    function createAccount(address owner, address cca, address[] calldata withdrawalTokens, uint256 salt)
        external
        returns (address);
    function getAccountAddress(address owner, address cca, address[] calldata withdrawalTokens, uint256 salt)
        external
        view
        returns (address);
    function kernelFactory() external view returns (address);
    function executionDelayPolicy() external view returns (address);
    function withdrawalLimitPolicy() external view returns (address);
    function ecdsaSigner() external view returns (address);
    function DAILY_LIMIT() external view returns (uint256);
    function LIMIT_INTERVAL() external view returns (uint48);
}
