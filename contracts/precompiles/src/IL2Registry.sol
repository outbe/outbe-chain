// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// Registry of L2 networks. Registration is governance-only; chain id 0 is invalid.
///
/// Any caller may offer a Tribute with a valid root signature and ZK proof for
/// the selected chain. The caller need not match the registered `l1Address`.
///
/// Public keys use canonical EIP-2537 G2 encoding (256 bytes):
/// x.c0 || x.c1 || y.c0 || y.c1, each component padded to 64 bytes.
/// Registering an empty key or 256 zero bytes selects live resolution from
/// `IDaInbox(l1Address).groupPubKey()`. Explicit registry keys take precedence.
/// Compressed inputs are rejected; inbox keys and updates must be nonidentity.
interface IL2Registry {
    event L2NetworkRegistered(uint64 indexed chainId, address indexed l1Address, bytes publicKey);
    event L2NetworkRemoved(uint64 indexed chainId);
    event L2PublicKeyUpdated(uint64 indexed chainId, bytes publicKey);

    /// Removes the caller's registered L2 network. The caller must equal the
    /// `l1Address` stored for `chainId`.
    function removeNetwork(uint64 chainId) external;

    /// Replaces the registered network's root-signing key immediately.
    /// `msg.sender` must equal the stored `l1Address`, which may be a contract.
    /// `publicKey` must be a valid, nonidentity EIP-2537 G2 key (256 bytes).
    /// Subsequent Tribute offers are verified against the replacement key.
    function updatePublicKey(uint64 chainId, bytes calldata publicKey) external;

    /// Returns the operator and effective EIP-2537 G2 key (256 bytes).
    /// When the stored key is unset, resolves the inbox key via STATICCALL on
    /// every read, without caching. Reverts for an unregistered chain, a failed
    /// inbox call, or an invalid inbox key.
    /// Inbox lookup charges a prepaid 100,000 gas and forwards that budget.
    function getNetwork(uint64 chainId) external view returns (address l1Address, bytes memory publicKey);

    /// Returns the chain id registered for `l1Address`, or 0 if not registered.
    function chainIdByL1Address(address l1Address) external view returns (uint64);
}
