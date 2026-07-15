// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ITribute {
    event TributeBodyStored(
        bytes tributeId,
        uint32 commitmentSchemeVersion,
        uint32 schemaVersion,
        bytes32 previousCommitment,
        bytes32 newCommitment,
        bytes canonicalPayload
    );

    event TributeBodyDeleted(bytes tributeId, bytes32 previousCommitment);

    event TributeIssued(
        address indexed owner,
        bytes tributeId,
        uint32 worldwideDay,
        uint256 issuanceAmountMinor,
        uint16 settlementCurrency,
        uint256 nominalAmountMinor
    );

    event TributeBurned(bytes tributeId, address owner, uint32 worldwideDay);

    event TributeWorldwideDaySealed(uint32 indexed worldwideDay, bool isSealed);

    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function totalSupply() external view returns (uint256);
    function balanceOf(address owner) external view returns (uint256);
    function ownerOf(bytes calldata tributeId) external view returns (address);
    function tokenURI(bytes calldata tributeId) external view returns (string memory);
    function getDayTotals(uint32 worldwideDay)
        external
        view
        returns (uint32 tributeCount, uint256 tributeNominalAmount, bool isSealed);
    function getTributesByOwner(address owner) external view returns (bytes[] memory tributeIds);
    function getTributesByDay(uint32 worldwideDay) external view returns (bytes[] memory tributeIds);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
