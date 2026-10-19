// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface INod {
    /// Emitted when a Nod is issued and when it is burned by forfeit or mining.
    event Transfer(address indexed from, address indexed to, uint256 indexed tokenId);
    // Declared for ERC-721 shape only: Nods are soulbound, so these two are never emitted.
    event Approval(address indexed owner, address indexed approved, uint256 indexed tokenId);
    event ApprovalForAll(address indexed owner, address indexed operator, bool approved);

    /// ERC-4906: emitted when a Nod is settled.
    event MetadataUpdate(uint256 _tokenId);
    /// ERC-4906: emitted per Worldwide Day when the call pass calls its groups.
    /// The range is the ids of that day, whose top four bytes are the day.
    event BatchMetadataUpdate(uint256 _fromTokenId, uint256 _toTokenId);

    /// Group force-called by the daily Call scan: the reference price
    /// exceeded the group's call price on enough of the trailing window. Every
    /// Nod in the group must be settled by `settlementDeadline`. Otherwise the
    /// Nod is forfeit-burned.
    event NodGroupCalled(bytes32 indexed groupKey, uint64 calledAt, uint64 settlementDeadline);

    /// Nod burned by the Call scan because its group's settlement deadline
    /// lapsed while the Nod was still unpaid. No Gratis is minted.
    event NodForfeited(address indexed owner, uint256 nodId, bytes encryptedGratisAmount);

    struct NodData {
        uint256 nodId;
        address owner;
        uint32 worldwideDay;
        uint16 leagueId;
        /// Encrypted Gratis entitlement, readable by the owner and enclave.
        bytes encryptedGratisAmount;
        bytes encryptedCreatorPublicKey;
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
    function totalSupply() external view returns (uint256);
    function tokenByIndex(uint256 index) external view returns (uint256);
    function tokenOfOwnerByIndex(address owner, uint256 index) external view returns (uint256);

    // outbe-specific
    function getNod(uint256 nodId) external view returns (NodData memory);
}
