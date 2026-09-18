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
    /// ERC-4906: emitted per Worldwide Day when a qualification or call pass changes its buckets.
    /// The range is the ids of that day, whose top four bytes are the day.
    event BatchMetadataUpdate(uint256 _fromTokenId, uint256 _toTokenId);

    event NodBodyStored(
        uint256 nodId,
        uint32 commitmentSchemeVersion,
        uint32 schemaVersion,
        bytes32 previousCommitment,
        bytes32 newCommitment,
        bytes canonicalPayload
    );

    event NodBodyDeleted(uint256 nodId, bytes32 previousCommitment);

    event NodBucketBodyStored(
        uint256 bucketId,
        uint32 commitmentSchemeVersion,
        uint32 schemaVersion,
        bytes32 previousCommitment,
        bytes32 newCommitment,
        bytes canonicalPayload
    );

    event NodBucketBodyDeleted(uint256 bucketId, bytes32 previousCommitment);

    event NodBucketQualified(
        bytes32 indexed bucketKey, uint256 worldwideDay, uint256 floorPriceMinor, uint16 referenceCurrency
    );

    /// Qualified bucket force-called by the daily Call scan: the reference price
    /// exceeded the bucket's call price on enough of the trailing window. Every
    /// Nod in the bucket must be settled by `settlementDeadline` or it
    /// is forfeit-burned.
    event NodBucketCalled(bytes32 indexed bucketKey, uint64 calledAt, uint64 settlementDeadline);

    /// Nod burned by the Call scan because its bucket's settlement deadline
    /// lapsed while the Nod was still unpaid. No Gratis is minted.
    event NodForfeited(address indexed owner, uint256 nodId, uint256 gratisLoadMinor);

    /// @notice A reference currency was left out of one day's qualification because its
    ///         day price could not be indexed. The next day's pass tries it again.
    event QualifyScanSkipped(uint16 indexed referenceCurrency, uint32 indexed utcDay);

    /// @notice A daily sweep (0 qualification, 1 call) fell two days behind: `skippedDay`
    ///         gave its place to a newer day and will not be walked.
    event SweepDaySkipped(uint8 indexed sweep, uint32 skippedDay, uint32 inFlightDay);

    struct NodData {
        uint256 nodId;
        address owner;
        uint32 worldwideDay;
        uint16 leagueId;
        uint256 floorPriceMinor;
        /// Gratis entitlement in protocol units (1,000,000 per whole COEN).
        uint256 gratisLoadMinor;
        /// Price of one whole COEN in referenceCurrency at six-decimal precision.
        uint256 entryPriceMinor;
        /// floor(entryPriceMinor * gratisLoadMinor / 1,000,000), in referenceCurrency
        /// at six-decimal precision; payment in an asset is quoted separately.
        uint256 settlementCostMinor;
        bool isQualified;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint64 issuedAt;
        /// Block timestamp the Nod's bucket was force-called; `0` while not
        /// called. The settlement deadline is this plus the call notice period.
        uint64 calledAt;
        bool isSettled;
        /// Read-time state: 0 Issued, 1 Qualified, 2 Called, 3 Settled, 4 Forfeited.
        /// Paid entitlements remain Settled after expiry. Forfeited items are
        /// still stored pending cleanup; deleted items revert with NodNotFound.
        uint8 effectiveState;
        /// Bucket terms sealed at issuance, independent of current defaults.
        uint256 callPriceMinor;
        uint16 callRate; // percent
        uint32 callWindow; // seconds
        uint32 callThreshold; // seconds
        uint32 callNoticePeriod; // seconds
        /// Inclusive deadline: 0 when uncalled, uint64.max for zero notice.
        uint64 settlementDeadline;
    }

    /// Finalized on-chain commitment to one activated OCOMP Nod generation.
    ///
    /// Individual Nod bodies remain in content-addressed storage and are
    /// accepted only with a Merkle proof against `nodRoot`.
    struct CertifiedGenerationData {
        bool exists;
        uint32 worldwideDay;
        uint64 generation;
        bytes32 nodRoot;
        bytes32 bucketRoot;
        bytes32 outputManifestRoot;
        uint32 tributeCount;
        uint32 nodCount;
        uint32 bucketCount;
        uint256 nodAmountTotal;
        /// Actual Lysis Allocation: sum of Nod gratisLoadMinor, not the Lysis Limit.
        uint256 lysisAllocationMinor;
        uint64 issuedAt;
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
    function nodData(uint256 nodId) external view returns (NodData memory);
    function certifiedGeneration(uint32 worldwideDay) external view returns (CertifiedGenerationData memory);
}
