// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IGratisFactory {
    error NativeCoenOverflow();
    error InvalidCoenAmount();

    event CoenMinedFromGratis(address indexed account, uint256 amountMinor);
    event GratisMinedFromCoen(address indexed account, uint256 amountMinor);
    function mineCoen(uint256 gratisMinor, bytes32 mac, uint64 opNonce) external returns (uint256 coenMinor);
    /// Mine Gratis from the native COEN sent with the call, 1 COEN to 1 Gratis. `msg.value`
    /// must be a non-zero whole number of Gratis minors (1e12 units each). The COEN is burned
    /// and credits the Promis Limit. WCOEN holders unwrap with `WCOEN.withdraw` first.
    /// `mac` and `opNonce` are the caller's Gratis modify key over `Mint` and `gratisMinor`.
    function mineGratis(bytes32 mac, uint64 opNonce) external payable returns (uint256 gratisMinor);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
