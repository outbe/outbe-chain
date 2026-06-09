// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @dev Minimal interface for KernelFactory
interface IKernelFactory {
    function createAccount(bytes calldata data, bytes32 salt) external payable returns (address);
    function getAddress(bytes calldata data, bytes32 salt) external view returns (address);
}
