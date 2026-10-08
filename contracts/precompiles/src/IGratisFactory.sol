// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IGratisFactory {
    struct ModifyAuth {
        bytes32 mac;
        uint64 opNonce;
    }
    event GratisPledged(uint256 indexed reservationId, address indexed source, uint256 gratisMinor);
    event PledgeSentToCredis(uint256 indexed reservationId, uint256 indexed positionId);
    event PledgeCancelled(uint256 indexed reservationId, address indexed source, uint256 gratisMinor);
    event CoenMined(address indexed sender, uint256 coenMinor);
    /// Pledge the reservation's Gratis from the caller's liquid balance. The caller
    /// must be the reservation source. `auth` binds `Pledge` and the reserved amount.
    function pledgeGratis(uint256 reservationId, ModifyAuth calldata auth) external;
    /// Return an unused pledge to the caller's liquid balance. Only its source may cancel.
    function cancelPledge(uint256 reservationId) external;
    /// The unused pledge for `reservationId`. Zeros when there is none.
    function pledgeOf(uint256 reservationId) external view returns (address source, uint256 gratisMinor);
    /// Gratis still backing Credis `positionId`. Zeros once it has been fully returned or burned.
    function collateralOf(uint256 positionId) external view returns (address source, uint256 remainingMinor);
    function mineCoen(uint256 gratisMinor, bytes32 mac, uint64 opNonce) external returns (uint256 coenMinor);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
