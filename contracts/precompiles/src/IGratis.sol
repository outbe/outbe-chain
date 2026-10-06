// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IGratis {
    // ERC-20 events, declared for ABI completeness. They are never emitted because
    // gratis is non-transferable.
    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    // Gratis runtime events.
    event GratisMinted(address indexed account, uint256 amount, uint256 newTotalSupply);
    event GratisBurned(address indexed account, uint256 amount, uint256 remainingSupply);

    // ERC-20 metadata
    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function decimals() external view returns (uint8);
    function totalSupply() external view returns (uint256);
    function pledgedTotalSupply() external view returns (uint256);
    // Confidential balance: returns the account's ciphertext blob, a fixed
    // 56 bytes = version(8, big-endian) || ChaCha20Poly1305 ct (32-byte U256
    // amount + 16-byte tag). The length is constant regardless of the balance,
    // so it never leaks magnitude. A never-written account returns empty bytes.
    // Decrypt off-chain with the account's view key.
    function balanceOf(address account) external view returns (bytes memory);

    // ERC-20 transfer surface. Gratis is non-transferable.
    // `allowance` returns 0. The others revert.
    function allowance(address owner, address spender) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function transfer(address to, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);

    // Current modify-auth replay counter for `account`. It is the value a write's
    // authorization (`mac`) must bind and that must be passed as `opNonce`.
    // Public: it is a per-account write counter, not a balance.
    function opNonceOf(address account) external view returns (uint64);

    // ERC-165
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
