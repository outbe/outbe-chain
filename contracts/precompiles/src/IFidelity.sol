// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IFidelity {
    function getFidelityIndex(address account) external view returns (uint64);
}
