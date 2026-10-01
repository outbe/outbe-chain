// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;
interface IGratisFactory {
    struct ModifyAuth { bytes32 mac; uint64 opNonce; }
    event PledgeNote(bytes32 indexed commitment, uint32 leafIndex, bytes32 rootAfter, uint256 amount);
    event PledgeSpent(bytes32 indexed nullifier);
    event CoenMined(address indexed sender, uint256 amount);
    /// Debit authenticated Gratis and append a note bound to the source account.
    function pledgeGratis(uint256 amount, ModifyAuth calldata auth) external returns (bytes32 commitment);
    /// Credit the original owner authenticated by the unpledge proof.
    function unpledgeGratis(bytes calldata proof) external;
    function pledgeRoot() external view returns (bytes32);
    function pledgeLeafCount() external view returns (uint64);
    function pledgeSpent(bytes32 nullifier) external view returns (bool);
    function mineCoen(uint256 amount, bytes32 mac, uint64 opNonce) external returns (uint256);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
