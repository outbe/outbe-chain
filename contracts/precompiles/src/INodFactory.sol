// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface INodFactory {
    event NodIssued(
        address indexed owner,
        uint256 nodId,
        uint256 worldwideDay,
        uint256 leagueId,
        uint256 floorPriceMinor,
        uint256 gratisLoadMinor,
        uint256 entryPriceMinor,
        uint256 costAmountMinor
    );

    event NodBurned(address indexed owner, uint256 nodId, uint256 gratisLoadMinor);

    /// @dev The reserve vault provider is the canonical `VAULT_PROVIDER_ADDRESS`
    ///      precompile; it is no longer passed in by the caller.
    function mineGratis(uint256 nodId, uint256 nonce, address asset) external returns (uint256);
}
