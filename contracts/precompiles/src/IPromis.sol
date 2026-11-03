// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IPromis {
    error TransferNotAllowed();
    error InvalidOpNonce(uint64 expected, uint64 provided);
    error Rejected(string value);

    // ERC-20 events, declared for ABI completeness. They are never emitted because
    // Promis is non-transferable.
    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    event PromisMinedFromGem(uint256 indexed gemId, address indexed account, bytes encryptedAmountMinor);
    event PromisMinedFromIntex(uint256 indexed intexId, address indexed account, bytes encryptedAmountMinor);
    event PromisBurned(address indexed account, bytes encryptedAmountMinor);

    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function decimals() external view returns (uint8);
    // Confidential balance: returns the account's ciphertext blob, a fixed
    // 92 bytes = version(8, big-endian) || "PRO2"(4) || binding(32) ||
    // ChaCha20Poly1305 ct (32-byte U256 amount + 16-byte tag). The length is
    // constant regardless of the balance, so it never leaks magnitude. A
    // never-written account returns empty bytes. Decrypt off-chain with the
    // account's Promis view key (outbe_deriveKeys).
    function balanceOf(address account) external view returns (bytes memory);

    // ERC-20 transfer surface. Promis is non-transferable.
    // `allowance` returns 0. The others revert.
    function allowance(address owner, address spender) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function transfer(address to, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);

    // Current modify-auth replay counter for `account` - the value a write's
    // authorization (`mac`) must bind and that must be passed as `opNonce`.
    // Public: it is a per-account write counter, not a balance.
    function opNonceOf(address account) external view returns (uint64);

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
