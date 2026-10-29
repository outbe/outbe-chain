// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IPromis {
    error InvalidOpNonce(uint64 expected, uint64 provided);
    error Rejected(string value);

    /// @notice `promisMinor` PROMIS (6 decimals) was minted; `totalSupplyMinor` is the supply after it.
    event PromisMinted(address indexed account, uint256 promisMinor, uint256 totalSupplyMinor);
    /// @notice `promisMinor` PROMIS (6 decimals) was burned; `totalSupplyMinor` is the supply after it.
    event PromisBurned(address indexed account, uint256 promisMinor, uint256 totalSupplyMinor);

    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function decimals() external view returns (uint8);
    function totalSupply() external view returns (uint256);
    // Confidential balance: returns the account's ciphertext blob, a fixed
    // 92 bytes = version(8, big-endian) || "PRO2"(4) || binding(32) ||
    // ChaCha20Poly1305 ct (32-byte U256 amount + 16-byte tag). The length is
    // constant regardless of the balance, so it never leaks magnitude. A
    // never-written account returns empty bytes. Decrypt off-chain with the
    // account's Promis view key (outbe_deriveKeys).
    function balanceOf(address account) external view returns (bytes memory);

    // Current modify-auth replay counter for `account` - the value a write's
    // authorization (`mac`) must bind and that must be passed as `opNonce`.
    // Public: it is a per-account write counter, not a balance.
    function opNonceOf(address account) external view returns (uint64);

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
