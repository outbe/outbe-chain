// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IGem {
    struct GemData {
        uint256 gemId;
        address owner;
        uint8 gemType;
        uint8 state;
        uint256 promisLoad;
        uint256 entryPrice;
        uint256 floorPrice;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint64 issuedAt;
        uint256 callPrice;
        uint64 calledAt;
        uint32 callNoticePeriod;
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

    // ERC-721 Enumerable
    function totalSupply() external view returns (uint256);
    function tokenByIndex(uint256 index) external view returns (uint256);
    function tokenOfOwnerByIndex(address owner, uint256 index) external view returns (uint256);

    // outbe-specific views
    function getGemStatus(uint256 gemId) external view returns (GemData memory);
    /// @notice Born Qualified (Genesis), or derived from finalized daily VWAPs; never stored.
    function isQualified(uint256 gemId) external view returns (bool);

    // --- Events ---
    /// @notice Emitted when a gem is issued and when it is burned by forfeit or mining.
    event Transfer(address indexed from, address indexed to, uint256 indexed tokenId);
    // Declared for ERC-721 shape only: gems are soulbound, so these two are never emitted.
    event Approval(address indexed owner, address indexed approved, uint256 indexed tokenId);
    event ApprovalForAll(address indexed owner, address indexed operator, bool approved);
    /// @notice ERC-4906: emitted when a gem is called or settled.
    event MetadataUpdate(uint256 _tokenId);
    /// @notice ERC-4906, declared for the standard's shape only: never emitted.
    event BatchMetadataUpdate(uint256 _fromTokenId, uint256 _toTokenId);
    /// @notice Gem force-called by the daily Call scan.
    event GemCalled(uint256 indexed gemId, uint64 calledAt);
    /// @notice Called gem forfeit-burned after its notice period lapsed.
    event GemExpired(uint256 indexed gemId, address owner, uint256 promisLoad);
    /// @notice A reference currency was left out of one day's Call scan because its
    ///         window price could not be indexed. The next daily pass tries it again.
    event CallScanSkipped(uint16 indexed referenceCurrency, uint32 indexed utcDay);
    /// @notice The daily call sweep (`sweep` = 1) fell two days behind: `skippedDay`
    ///         gave its place to a newer day and will not be walked.
    event SweepDaySkipped(uint8 indexed sweep, uint32 skippedDay, uint32 inFlightDay);
}
