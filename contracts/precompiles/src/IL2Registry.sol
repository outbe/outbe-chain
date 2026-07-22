// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IL2Registry {
    event L2NetworkRegistered(uint64 indexed chainId, address indexed l1Address, bytes publicKey);
    event L2NetworkZkSet(uint64 indexed chainId, bool enabled);
    event L2NetworkRemoved(uint64 indexed chainId);

    /// Registers an L2 network. `publicKey` is the network's BLS MinPk public
    /// key (48 bytes, the same variant used for validator consensus keys).
    /// `l1Address` is the L1 account that submits on behalf of the network
    /// (e.g. calls `TributeFactory.offerTribute`); it must be unique across
    /// registered networks. Permissionless: any caller may register.
    function registerNetwork(uint64 chainId, address l1Address, bytes calldata publicKey) external;

    /// Enables or disables ZK verification for a registered network.
    /// Permissionless: any caller may toggle.
    function setZkEnabled(uint64 chainId, bool enabled) external;

    /// Removes a registered L2 network. Permissionless: any caller may remove.
    function removeNetwork(uint64 chainId) external;

    /// Returns the registration for `chainId`. Reverts when not registered.
    function getNetwork(uint64 chainId)
        external
        view
        returns (address l1Address, bytes memory publicKey, bool zkEnabled);

    /// Returns the chain id registered for `l1Address`, or 0 when none.
    function chainIdByL1Address(address l1Address) external view returns (uint64);
}
