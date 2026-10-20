// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IPromisLimit {
    function promisLimit() external view returns (uint256 promisLimitMinor);
}
