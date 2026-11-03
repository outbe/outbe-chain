// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IPromisLimit {
    error Overflow();
    error Underflow();
    error UnauthorizedBurnCaller(address caller, uint8 source);

    event NativeBurnedToPromisLimit(uint8 indexed source, uint256 burnedUnit);

    function promisLimit() external view returns (uint256 promisLimitMinor);

    function restMinor() external view returns (uint256 restMinor);
}
