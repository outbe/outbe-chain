// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/// @dev The upgrade function of an OpenZeppelin `UUPSUpgradeable` proxy. Every multi-chain proxy
///      of Outbe has it. The Update precompile reads this file to encode an approved
///      `upgradeContracts` proposal (crates/system/update/src/sol_ext.rs).
interface IUUPSUpgradeable {
    function upgradeToAndCall(address newImplementation, bytes calldata data) external payable;
}
