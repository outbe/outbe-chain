// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ITribute {
    event TributeBodyStored(
        uint256 indexed tokenId,
        address owner,
        uint32 worldwideDay,
        uint256 issuanceAmountMinor,
        uint16 issuanceCurrency,
        uint256 nominalAmountMinor,
        uint16 referenceCurrency,
        uint256 tributePriceMinor,
        bool excludeFromIntexIssuance
    );

    event TributeBodyDeleted(uint256 indexed tokenId);

    event TributeIssued(
        address indexed owner,
        uint256 tokenId,
        uint32 worldwideDay,
        uint256 issuanceAmountMinor,
        uint16 settlementCurrency,
        uint256 nominalAmountMinor
    );

    event TributeBurned(uint256 indexed tokenId, address owner, uint32 worldwideDay);

    event TributeWorldwideDaySealed(uint32 indexed worldwideDay, bool isSealed);

    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function totalSupply() external view returns (uint256);
    function balanceOf(address owner) external view returns (uint256);
    function ownerOf(uint256 tokenId) external view returns (address);
    function tokenURI(uint256 tokenId) external view returns (string memory);
    function getDayTotals(uint32 worldwideDay)
        external
        view
        returns (uint32 tributeCount, uint256 tributeNominalAmount, bool isSealed);
    function getTributesByOwner(address owner) external view returns (uint256[] memory tokenIds);
    function getTributesByDay(uint32 worldwideDay) external view returns (uint256[] memory tokenIds);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
