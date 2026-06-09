// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ITokenBundle {
    function topUp(address sender, address token, uint256 amount) external;
    function balanceOf(address owner, address token) external view returns (uint256);
}
