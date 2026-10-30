// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface INod {
    error InvalidNodIdLength();
    error InvalidNodIdHex();
    error NodNotFound();
    error GroupNotFound();
    error QueryLimitExceeded();
    error ZeroReferenceCurrency();
    error NonTransferable();
    error UnknownProfileSelector(uint8 selector);
    error CanonicalIdentityMismatch(uint256 expected, uint256 found);
    error EncryptedTermsMismatch();
    error CannotIssueSettledNod();
    error GroupIdentityMismatch(bytes32 expected, bytes32 found);
    error NodAlreadyExists();
    error GroupExistenceDisagreesWithMemberCount(uint256 groupId, uint32 memberCount);
    error GroupUnpaidLoadOverflow(bytes32 groupKey);
    error GroupUnpaidLoadUnderflow(bytes32 groupKey);
    error GroupSettledCountUnderflow(uint256 groupId);
    error AlreadySettled();
    error SettledCountOverflow();
    error GroupMemberIndexOverflow(bytes32 groupKey);
    error GroupMemberCountUnderflowDuringRemoval(bytes32 groupKey);
    error NodNotIndexedInGroup(uint256 nodId, bytes32 groupKey);
    error EmptyGroupMemberSlotDuringRemoval(bytes32 groupKey, uint32 slot);
    error AmountReadFailed(string reason);
    error NodStateChangedAfterLoading();
    error DateStep(uint8 kind, uint32 dateKey);
    error NodNotForfeited();
    error NodAlreadySettled();

    /// Emitted when a Nod is issued and when it is burned by forfeit or mining.
    event Transfer(address indexed from, address indexed to, uint256 indexed tokenId);
    // Declared for ERC-721 shape only: Nods are soulbound, so these two are never emitted.
    event Approval(address indexed owner, address indexed approved, uint256 indexed tokenId);
    event ApprovalForAll(address indexed owner, address indexed operator, bool approved);

    /// ERC-4906: emitted when a Nod is settled.
    event MetadataUpdate(uint256 _tokenId);
    /// ERC-4906: emitted over the whole id range when a group is called. A group holds
    /// Nods of several series, so no narrower range covers it.
    event BatchMetadataUpdate(uint256 _fromTokenId, uint256 _toTokenId);

    /// Group force-called by the daily Call scan: the reference price
    /// exceeded the group's call price on enough of the trailing window. Every
    /// Nod in the group must be settled by `settlementDeadline`. Otherwise it is
    /// forfeited with its group.
    event NodGroupCalled(bytes32 indexed groupKey, uint64 calledAt, uint64 settlementDeadline);

    /// The group's settlement deadline lapsed: its `nods` unpaid Nods are forfeited and
    /// their `gratisLoadMinor` returned to the Promis reserve at once. No Gratis is
    /// minted. The cleanup deletes them after.
    event NodGroupForfeited(bytes32 indexed groupKey, uint32 nods, uint256 gratisLoadMinor);

    struct NodData {
        // keccak256(seriesId || owner).
        uint256 nodId;
        address owner;
        /// The emission day `YYYYMMDD` and the currencies of the Tribute the Nod comes from.
        bytes14 seriesId;
        uint16 leagueId;
        /// Encrypted Gratis entitlement, readable by the owner and enclave.
        bytes encryptedGratisAmount;
        bytes32 encryptionBinding;
        uint64 chainId;
        /// Price of one whole COEN in referenceCurrency at six-decimal precision.
        uint256 entryPriceMinor;
        /// floor(entryPriceMinor * gratisLoadMinor / 1,000,000), in referenceCurrency
        /// at six-decimal precision. Payment in an asset is quoted separately.
        uint256 settlementCostMinor;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint64 issuedAt;
        /// Block timestamp when the Nod's group was force-called. `0` while not
        /// called. The settlement deadline is this plus the call notice period.
        uint64 calledAt;
        bool isSettled;
        /// Read-time state: 0 Issued, 1 Called, 2 Settled, 3 Forfeited.
        /// Paid entitlements remain Settled after expiry. Forfeited items stay
        /// in storage until cleanup. Reads of deleted items revert with NodNotFound.
        uint8 effectiveState;
        /// Group terms sealed at issuance, independent of current defaults.
        uint256 callPriceMinor;
        uint16 callRate; // percent
        uint32 callWindow; // seconds
        uint32 callThreshold; // seconds
        uint32 callNoticePeriod; // seconds
        /// Inclusive deadline: 0 when uncalled.
        uint64 settlementDeadline;
        /// The call group the Nod is called and forfeited with.
        bytes32 groupKey;
    }

    // ERC-165
    function supportsInterface(bytes4 interfaceId) external view returns (bool);

    // Identity and ownership reads (32-byte entity IDs, carried as uint256)
    function balanceOf(address owner) external view returns (uint256 balance);
    function ownerOf(uint256 nodId) external view returns (address);

    // ERC-721 transfer surface. Nods are soulbound: transfers and approvals always revert.
    function transferFrom(address from, address to, uint256 nodId) external;
    function safeTransferFrom(address from, address to, uint256 nodId) external;
    function safeTransferFrom(address from, address to, uint256 nodId, bytes calldata data) external;
    function approve(address to, uint256 nodId) external;
    function setApprovalForAll(address operator, bool approved) external;
    // No approval can exist: these read address(0) and false.
    function getApproved(uint256 nodId) external view returns (address);
    function isApprovedForAll(address owner, address operator) external view returns (bool);

    // Metadata reads
    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function tokenURI(uint256 nodId) external view returns (string memory);

    // Enumeration reads
    function tokensByOwner(address owner) external view returns (uint256[] memory tokenIds);

    // outbe-specific
    function getNod(uint256 nodId) external view returns (NodData memory);
    function clean(uint256 nodId) external;
}
