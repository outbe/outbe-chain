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

    /// @notice Burn the caller-owned Nod and mint its gratis load to the caller.
    ///         Authorized by the caller's Gratis modify key: `mac =
    ///         HMAC(modifyKey, op-preimage)` where `opNonce` MUST equal the
    ///         caller's current on-chain gratis op-nonce. The Nod owner is the
    ///         gratis recipient, so they can always supply this authorization.
    function mineGratis(uint256 nodId, uint256 nonce, address asset, bytes32 mac, uint64 opNonce)
        external
        returns (uint256);
}
