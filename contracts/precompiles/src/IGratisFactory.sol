// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IGratisFactory {
    error FidelityNotEligible();
    error ReservationNotFound();
    error ReservationExpired();
    error NotReservationSource();
    error PledgeExists();
    error PledgeNotFound();
    error PledgeMismatch();
    error NativeCoenOverflow();
    error Exists();
    error NotFound();
    error Exceeded();
    error InvalidCoenAmount();

    struct ModifyAuth {
        bytes32 mac;
        uint64 opNonce;
    }
    event GratisPledgeCreated(uint256 indexed reservationId, address indexed source, uint256 gratisMinor);
    event GratisPledgeSentToCredis(uint256 indexed reservationId, uint256 indexed credisId);
    event GratisPledgeCanceled(uint256 indexed reservationId, address indexed source, uint256 gratisMinor);
    event GratisPledgeReleased(uint256 indexed credisId, address indexed source, uint256 gratisMinor);
    event GratisPledgeBurned(uint256 indexed credisId, address indexed source, uint256 gratisMinor);
    event CoenMinedFromGratis(address indexed account, uint256 amountMinor);
    event GratisMinedFromCoen(address indexed account, uint256 amountMinor);
    /// Pledge the reservation's Gratis from the caller's liquid balance. The caller
    /// must be the reservation source. `auth` binds `Pledge` and the reserved amount.
    function pledgeGratis(uint256 reservationId, ModifyAuth calldata auth) external;
    /// Return an unused pledge to the caller's liquid balance. Only its source may cancel.
    function cancelPledge(uint256 reservationId) external;
    /// The unused pledge for `reservationId`. Zeros when there is none.
    function pledgeOf(uint256 reservationId) external view returns (address source, uint256 gratisMinor);
    /// Gratis still backing Credis `credisId`. Zeros once it has been fully returned or burned.
    function collateralOf(uint256 credisId) external view returns (address source, uint256 remainingMinor);
    function mineCoen(uint256 gratisMinor, bytes32 mac, uint64 opNonce) external returns (uint256 coenMinor);
    /// Mine Gratis from the native COEN sent with the call, 1 COEN to 1 Gratis. `msg.value`
    /// must be a non-zero whole number of Gratis minors (1e12 units each). The COEN is burned
    /// and credits the Promis Limit. WCOEN holders unwrap with `WCOEN.withdraw` first.
    /// `mac` and `opNonce` are the caller's Gratis modify key over `Mint` and `gratisMinor`.
    function mineGratis(bytes32 mac, uint64 opNonce) external payable returns (uint256 gratisMinor);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
