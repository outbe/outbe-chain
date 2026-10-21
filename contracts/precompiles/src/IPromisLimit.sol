// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IPromisLimit {
    error Overflow();
    error Underflow();

    function promisLimit() external view returns (uint256 promisLimitMinor);
}
