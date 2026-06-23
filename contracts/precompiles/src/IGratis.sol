// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IGratis {
    // ERC-20 events (declared for ABI completeness; never emitted because
    // gratis is non-transferable).
    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    // Gratis runtime events.
    event GratisMined(address indexed account, uint256 amount, uint256 newTotalSupply);
    event GratisBurned(address indexed account, uint256 amount, uint256 remainingSupply);
    event GratisPledged(address indexed account, uint256 amount, uint256 totalPledged);
    event GratisUnpledged(address indexed account, uint256 amount, uint256 remainingPledged);

    // ERC-20 metadata
    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function decimals() external view returns (uint8);
    function totalSupply() external view returns (uint256);
    function pledgedTotalSupply() external view returns (uint256);
    function balanceOf(address account) external view returns (uint256);

    // ERC-20 transfer surface — gratis is non-transferable.
    // `allowance` returns 0; the others revert.
    function allowance(address owner, address spender) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function transfer(address to, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);

    // gratis-specific
    function pledgedOf(address account) external view returns (uint256);

    // ERC-165
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
