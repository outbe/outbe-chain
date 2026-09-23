// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// Registry of L2 networks. Registration is governance-only; chain id 0 is invalid.
///
/// Any caller may offer a Tribute with a valid root signature and ZK proof for
/// the selected chain. The caller need not match the registered `l1Address`.
interface IL2Registry {
    event L2NetworkRegistered(uint64 indexed chainId, address indexed l1Address, bytes publicKey);
    event L2NetworkRemoved(uint64 indexed chainId);
    event L2PublicKeyUpdated(uint64 indexed chainId, bytes publicKey);

    /// Removes the caller's registered L2 network. The caller must equal the
    /// `l1Address` stored for `chainId`.
    function removeNetwork(uint64 chainId) external;

    /// Replaces the registered network's root-signing key immediately.
    /// `msg.sender` must equal the stored `l1Address`, which may be a contract.
    /// `publicKey` must be a valid compressed BLS MinSig G2 key (96 bytes).
    /// Subsequent Tribute offers are verified against the replacement key.
    function updatePublicKey(uint64 chainId, bytes calldata publicKey) external;

    /// Returns the registration for `chainId`. Reverts when not registered.
    function getNetwork(uint64 chainId) external view returns (address l1Address, bytes memory publicKey);

    /// Returns the chain id registered for `l1Address`, or 0 if not registered.
    function chainIdByL1Address(address l1Address) external view returns (uint64);
}
