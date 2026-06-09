// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.10;

// TODO replace this copy-paste with reference from AA
/*
 * @dev copied from Credis for integration
 */
interface ITokenBundle {
    function topUp(address sender, address token, uint256 amount) external;
    function balanceOf(address owner, address token) external view returns (uint256);
}
