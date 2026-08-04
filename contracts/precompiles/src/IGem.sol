// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IGem {
    struct GemData {
        uint256 gemId;
        address owner;
        uint8 gemType;
        uint8 state;
        uint256 gemLoad;
        uint256 entryPrice;
        uint256 costAmount;
        uint256 floorPrice;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint64 issuedAt;
    }

    // ERC-721
    function balanceOf(address owner) external view returns (uint256 balance);
    function ownerOf(uint256 gemId) external view returns (address);
    function transferFrom(address from, address to, uint256 gemId) external;
    function safeTransferFrom(address from, address to, uint256 gemId) external;
    function approve(address to, uint256 gemId) external;
    function setApprovalForAll(address operator, bool approved) external;
    function getApproved(uint256 gemId) external view returns (address);
    function isApprovedForAll(address owner, address operator) external view returns (bool);

    // ERC-721 Metadata
    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function tokenURI(uint256 gemId) external view returns (string memory);

    // ERC-721 Enumerable (partial)
    function totalSupply() external view returns (uint256);
    function tokenOfOwnerByIndex(address owner, uint256 index) external view returns (uint256);

    // outbe-specific views
    function getGemStatus(uint256 gemId) external view returns (GemData memory);
}
