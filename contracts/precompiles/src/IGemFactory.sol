// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IGemFactory {
    function settleGem(uint256 gemId) external;
    function mineGemPromis(uint256 gemId, uint256 nonce) external returns (uint256);
    function getStatistics()
        external
        view
        returns (uint256 totalGemsIssued, uint256 totalIntexParked);
}
