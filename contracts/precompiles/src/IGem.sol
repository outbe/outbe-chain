// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IGem {
    error NonTransferable();
    error GemNotFound();
    error AlreadySettled();
    error IndexOutOfBounds();
    error AlreadyExists();
    error ZeroOwner();
    error UnknownProfileSelector(uint8 value);
    error GroupExistenceMismatch(uint256 groupId, uint32 memberCount);
    error TotalSupplyOverflow();
    error SettledCountOverflow();
    error TotalSupplyUnderflow();
    error SettledCountUnderflow(uint256 groupId);
    error EmptyGroup(bytes32 group);
    error GemNotInGroup(uint256 gemId, bytes32 group);
    error EmptyGroupSlotOnRemoval(bytes32 group, uint32 slot);
    error GemStateChangedAfterLoading();

    struct GemData {
        uint256 gemId;
        address owner;
        /// Read-time state: 0 Issued, 1 Called, 2 Settled,
        /// 3 Forfeited (a Called gem past its notice period, until the cleanup deletes it).
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
        /// The call group the gem is called and forfeited with.
        bytes32 groupKey;
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
    /// @notice Emitted when a gem is issued, when the cleanup deletes it after its group
    ///         was forfeited, and when it is burned by mining.
    event Transfer(address indexed from, address indexed to, uint256 indexed tokenId);
    // Declared for ERC-721 shape only: gems are soulbound, so these two are never emitted.
    event Approval(address indexed owner, address indexed approved, uint256 indexed tokenId);
    event ApprovalForAll(address indexed owner, address indexed operator, bool approved);
    /// @notice ERC-4906: emitted when a gem is settled.
    event MetadataUpdate(uint256 _tokenId);
    /// @notice ERC-4906: emitted over the whole id range when a group is called, since
    ///         gem ids carry no order a narrower range could follow.
    event BatchMetadataUpdate(uint256 _fromTokenId, uint256 _toTokenId);
    /// @notice The daily Call scan force-called every gem in the group.
    event GemGroupCalled(bytes32 indexed groupKey, uint64 calledAt, uint64 settlementDeadline);
    /// @notice The group's notice period lapsed: its `gems` unpaid gems are forfeited and
    ///         their `promisLoadMinor` returned at once. The cleanup deletes them after.
    event GemGroupForfeited(bytes32 indexed groupKey, uint32 gems, uint256 promisLoadMinor);
}
