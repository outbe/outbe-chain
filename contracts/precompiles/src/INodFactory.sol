// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface INodFactory {
    event NodIssued(
        address indexed owner,
        bytes nodId,
        uint256 worldwideDay,
        uint256 leagueId,
        uint256 floorPriceMinor,
        uint256 gratisLoadMinor,
        uint256 entryPriceMinor,
        uint256 costAmountMinor
    );

    event NodBurned(address indexed owner, bytes nodId, uint256 gratisLoadMinor);

    function mineGratis(bytes calldata nodId, uint256 nonce, address asset) external returns (uint256);
}
