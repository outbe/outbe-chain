// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IGratisFactory {
    event CoenMined(address indexed sender, uint256 amount);
    event PledgeNoteCreated(bytes encryptedReceipt);
    event PledgeNoteCancelled(bytes encryptedReceipt);
    /// Payload encodes the public Quote and encrypted owner authorization.
    /// Use an independently funded relayer to hide the source account.
    function createPledgeNote(bytes calldata request) external returns (bytes memory encryptedReceipt);
    /// A pending note remains cancellable after expiry. Failed vault refunds retry.
    function cancelPledgeNote(bytes calldata encryptedAuth) external returns (bytes memory encryptedReceipt);
    function mineCoen(uint256 amount, bytes32 mac, uint64 opNonce) external returns (uint256);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
