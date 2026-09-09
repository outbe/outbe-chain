// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;
import {Install} from "@zerodev/kernel/types/Structs.sol";

interface ISmartAccountFactory {
    function createAccount(address owner, uint256 salt) external returns (address);
    function getAccountAddress(address owner, uint256 salt) external view returns (address);
    function getBundleInstallPackages(address cca, address[] calldata tokens, address[] calldata senders)
        external
        view
        returns (Install[] memory);
    function openBundle(
        address account,
        address cca,
        address[] calldata tokens,
        address[] calldata senders,
        uint256 nonce,
        bytes calldata signature
    ) external;
    function kernelFactory() external view returns (address);
    function sudoPolicy() external view returns (address);
    function bundleModulePlugin() external view returns (address);
    function ecdsaSigner() external view returns (address);
    function bundleWithdrawHook() external view returns (address);
}
