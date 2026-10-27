// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IPromisLimit {
    error Overflow();
    error Underflow();

    event NativeBurnedToPromisLimit(uint8 indexed source, uint256 burnedUnit);

    function promisLimit() external view returns (uint256 promisLimitMinor);

    function nativeRest() external view returns (uint256 nativeRestUnit);
}
