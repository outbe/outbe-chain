// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IGem {
    struct GemData {
        uint256 gemId;
        address owner;
        /// Read-time state: 0 Issued, 1 Called, 2 Settled,
        /// 3 Forfeited (a Called gem past its notice period, until the sweep burns it).
        uint8 state;
        uint256 promisLoadMinor;
        uint256 entryPriceMinor;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint64 issuedAt;
        uint256 callPriceMinor;
        uint64 calledAt;
        uint32 callNoticePeriod;
        /// Call terms sealed at issuance, in seconds.
        uint32 callWindow;
        uint32 callThreshold;
        /// Inclusive deadline: 0 when uncalled.
        uint64 settlementDeadline;
    }

    // ERC-165
    function supportsInterface(bytes4 interfaceId) external view returns (bool);

    // ERC-721
    function balanceOf(address owner) external view returns (uint256 balance);
    function ownerOf(uint256 gemId) external view returns (address);
    // Gems are soulbound: transfers and approvals always revert NonTransferable.
    function transferFrom(address from, address to, uint256 gemId) external;
    function safeTransferFrom(address from, address to, uint256 gemId) external;
    function safeTransferFrom(address from, address to, uint256 gemId, bytes calldata data) external;
    function approve(address to, uint256 gemId) external;
    function setApprovalForAll(address operator, bool approved) external;
    // No approval can exist: these read address(0) and false.
    function getApproved(uint256 gemId) external view returns (address);
    function isApprovedForAll(address owner, address operator) external view returns (bool);

    // ERC-721 Metadata
    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function tokenURI(uint256 gemId) external view returns (string memory);

    // Supply and per-owner lookup; the ERC-721 Enumerable interface id is not supported.
    function totalSupply() external view returns (uint256);
    function tokenOfOwnerByIndex(address owner, uint256 index) external view returns (uint256);

    // outbe-specific views
    function getGem(uint256 gemId) external view returns (GemData memory);

    // --- Events ---
    /// @notice Emitted when a gem is issued and when it is burned by forfeit or mining.
    event Transfer(address indexed from, address indexed to, uint256 indexed tokenId);
    // Declared for ERC-721 shape only: gems are soulbound, so these two are never emitted.
    event Approval(address indexed owner, address indexed approved, uint256 indexed tokenId);
    event ApprovalForAll(address indexed owner, address indexed operator, bool approved);
    /// @notice ERC-4906: emitted when a gem is settled.
    event MetadataUpdate(uint256 _tokenId);
    /// @notice ERC-4906: emitted over the whole id range when a bucket is called, since
    ///         gem ids carry no order a narrower range could follow.
    event BatchMetadataUpdate(uint256 _fromTokenId, uint256 _toTokenId);
    /// @notice The daily Call scan force-called every gem in the bucket.
    event GemBucketCalled(bytes32 indexed bucketKey, uint64 calledAt, uint64 settlementDeadline);
    /// @notice Called gem forfeit-burned after its notice period lapsed.
    event GemForfeited(uint256 indexed gemId, address owner, uint256 promisLoadMinor);
}
