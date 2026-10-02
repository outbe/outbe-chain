// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IPromisLimit {
    event CertifiedCarryOverCredited(
        bytes32 indexed activationCallId,
        uint32 indexed sourceWorldwideDay,
        uint256 promisLimitBeforeMinor,
        uint256 unusedLysisLimitMinor,
        uint256 promisLimitAfterMinor,
        bytes32 stateEventDigest
    );

    function totalUnallocated() external view returns (uint256 promisLimitMinor);
}
