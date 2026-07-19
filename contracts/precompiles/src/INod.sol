// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface INod {
    event NodBodyStored(
        bytes nodId,
        uint32 commitmentSchemeVersion,
        uint32 schemaVersion,
        bytes32 previousCommitment,
        bytes32 newCommitment,
        bytes canonicalPayload
    );

    event NodBodyDeleted(bytes nodId, bytes32 previousCommitment);

    event NodBucketBodyStored(
        bytes bucketId,
        uint32 commitmentSchemeVersion,
        uint32 schemaVersion,
        bytes32 previousCommitment,
        bytes32 newCommitment,
        bytes canonicalPayload
    );

    event NodBucketBodyDeleted(bytes bucketId, bytes32 previousCommitment);

    event NodBucketQualified(
        bytes32 indexed bucketKey, uint256 worldwideDay, uint256 floorPriceMinor, bool isQualified
    );

    struct NodData {
        bytes nodId;
        address owner;
        uint32 worldwideDay;
        uint16 leagueId;
        uint256 floorPriceMinor;
        uint256 gratisLoadMinor;
        uint256 costOfGratisMinor;
        uint256 costAmountMinor;
        bool isQualified;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint64 issuedAt;
    }

    // ERC-165
    function supportsInterface(bytes4 interfaceId) external view returns (bool);

    // Identity and ownership reads (36-byte entity IDs)
    function balanceOf(address owner) external view returns (uint256 balance);
    function ownerOf(bytes calldata nodId) external view returns (address);

    // Metadata reads
    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function tokenURI(bytes calldata nodId) external view returns (string memory);

    // Enumeration reads
    function totalSupply() external view returns (uint256);
    function tokenByIndex(uint256 index) external view returns (bytes memory);
    function tokenOfOwnerByIndex(address owner, uint256 index) external view returns (bytes memory);

    // outbe-specific
    function nodData(bytes calldata nodId) external view returns (NodData memory);
}
