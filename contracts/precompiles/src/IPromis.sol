// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IPromis {
    event PromisMinted(address indexed account, uint256 amount, uint256 newTotalSupply);
    event PromisBurned(address indexed account, uint256 amount, uint256 remainingSupply);

    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function decimals() external view returns (uint8);
    function totalSupply() external view returns (uint256);
    function balanceOf(address account) external view returns (uint256);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
