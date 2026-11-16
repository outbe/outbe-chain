// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IPromisLimit {
    error Underflow();
    error UnauthorizedBurnCaller(address caller, uint8 source);

    event NativeBurnedToPromisLimit(uint8 indexed source, uint256 burnedUnit);
    event PromisLimitAdded(uint8 indexed source, uint256 amountMinor);
    event PromisLimitSubtracted(uint8 indexed target, uint256 amountMinor);

    function promisLimit() external view returns (uint256 promisLimitMinor);

    function restMinor() external view returns (uint256 restMinor);
}
