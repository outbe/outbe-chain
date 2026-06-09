// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface INod {
    event NodBucketQualified(
        bytes32 indexed bucketKey, uint256 worldwideDay, uint256 floorPriceMinor, bool isQualified
    );

    struct NodData {
        uint256 nodId;
        address owner;
        uint32 worldwideDay;
        uint32 leagueId;
        uint256 floorPriceMinor;
        uint256 gratisLoadMinor;
        uint256 costOfGratisMinor;
        uint256 costAmountMinor;
        bool isQualified;
        uint16 issuanceCurrency;
        uint64 unlocksAt;
        uint16 referenceCurrency;
        uint64 issuedAt;
    }

    // ERC-165
    function supportsInterface(bytes4 interfaceId) external view returns (bool);

    // ERC-721
    function balanceOf(address owner) external view returns (uint256 balance);
    function ownerOf(uint256 nodId) external view returns (address);

    // ERC-721-metadata
    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function tokenURI(uint256 nodId) external view returns (string memory);

    // ERC-721-enumerable
    function totalSupply() external view returns (uint256);
    function tokenByIndex(uint256 index) external view returns (uint256);
    function tokenOfOwnerByIndex(address owner, uint256 index) external view returns (uint256);

    // outbe-specific
    function nodData(uint256 nodId) external view returns (NodData memory);

    // backward compatibility
    function tokens(address owner) external view returns (uint256[] memory);
}
