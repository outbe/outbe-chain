// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @notice Principal and interest amounts are in the Credis asset's atomic units. Gratis
///         amounts and prices are at scale 1e6.
interface ICredis {
    /// Emitted once, when a Credis opens: Credis are never burned.
    event Transfer(address indexed from, address indexed to, uint256 indexed tokenId);
    // Declared for ERC-721 shape only. A Credis is bound to its smart account, so these two
    // are never emitted.
    event Approval(address indexed owner, address indexed approved, uint256 indexed tokenId);
    event ApprovalForAll(address indexed owner, address indexed operator, bool approved);
    /// ERC-4906: emitted when a Credis is called, on every settlement, and when it is forfeited.
    event MetadataUpdate(uint256 _tokenId);
    /// ERC-4906, declared for the standard's shape only: never emitted.
    event BatchMetadataUpdate(uint256 _fromTokenId, uint256 _toTokenId);

    event CredisCalled(uint256 indexed credisId, uint64 calledAt, uint64 settlementDeadline);

    /// @notice One successful settlement. `interestPaidMinor` is this payment's interest.
    ///         The Credis's `interestPaidMinor` is the sum of these payments
    ///         across successful settlements. A reverted settlement emits nothing
    ///         and leaves that total unchanged.
    event SettlementApplied(
        uint256 indexed credisId,
        uint256 interestPaidMinor,
        uint256 principalPaidMinor,
        uint256 gratisReturnedMinor,
        uint256 outstandingPrincipalMinor
    );

    event CredisSettled(uint256 indexed credisId);

    /// @notice Forfeiture of a called Credis. Records the principal written off.
    ///         Unpaid interest is left out of this event and out of `interestPaidMinor`.
    event CredisForfeited(
        uint256 indexed credisId, address indexed cca, uint256 gratisBurnedMinor, uint256 principalWrittenOffMinor
    );

    /// @notice Lifecycle state of a Credis, mirroring the Rust `CredisState`.
    ///         `Issued -> (Called) -> Settled | Forfeited`. A Credis is settleable
    ///         from the moment it is issued.
    enum State {
        Issued,
        Called,
        Settled,
        Forfeited
    }

    struct Credis {
        uint256 credisId;
        address owner;
        address cca;
        address asset;
        /// ISO 4217 numeric code of the disbursed asset.
        uint16 issuanceCurrency;
        /// ISO 4217 numeric code of the reference currency elected at origination
        /// and fixed for the Credis's life.
        uint16 referenceCurrency;
        /// Main account whose pledged Gratis backs the Credis.
        address source;
        /// P - the stablecoin amount disbursed. Never changes.
        uint256 principalMinor;
        /// P_out - decreases with each settlement. The Credis closes at zero.
        uint256 outstandingPrincipalMinor;
        /// G - the pledged Gratis, valued 1:1 against principal at the reservation quote rate.
        uint256 gratisMinor;
        /// The share of G still locked. Released principal-proportionally.
        uint256 outstandingGratisMinor;
        /// r - the annual policy rate of the issuance currency, scale 1e6, fixed at opening.
        uint256 policyRate;
        /// Entry price in the issuance currency, scale 1e6, fixed by the reservation.
        uint256 entryPriceMinor;
        /// Call anchor price in the reference currency, scale 1e6, fixed by the reservation.
        uint256 callAnchorPriceMinor;
        /// callAnchorPriceMinor * 1.64, in the reference currency.
        uint256 callPriceMinor;
        /// Issuance timestamp.
        uint64 issuedAt;
        /// Anchor of the interest day count: origination until the first settlement.
        uint64 lastSettledAt;
        /// 0 until the Credis is called.
        uint64 calledAt;
        /// See {State}. Read-time: a Called Credis past its settlement deadline reads
        /// Forfeit before the forfeit sweep reaches it.
        uint8 state;
        /// Lifetime interest collected, in asset minor units. The sum of successful
        /// `SettlementApplied.interestPaidMinor` payments. Current-period accrual is
        /// {interestAccruedMinor}.
        uint256 interestPaidMinor;
        /// Inclusive deadline: 0 when uncalled.
        uint64 settlementDeadline;
        /// Call terms sealed at issuance, in seconds.
        uint32 callNoticePeriod;
        uint32 callWindow;
        uint32 callThreshold;
    }

    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function tokenURI(uint256 credisId) external view returns (string memory);

    function totalSupply() external view returns (uint256);
    function getCredis(uint256 credisId) external view returns (Credis memory);
    function ownerOf(uint256 credisId) external view returns (address);

    // ERC-721 transfer surface. A Credis is bound to its smart account: transfers and
    // approvals always revert.
    function transferFrom(address from, address to, uint256 credisId) external;
    function safeTransferFrom(address from, address to, uint256 credisId) external;
    function safeTransferFrom(address from, address to, uint256 credisId, bytes calldata data) external;
    function approve(address to, uint256 credisId) external;
    function setApprovalForAll(address operator, bool approved) external;
    // No approval can exist: these read address(0) and false.
    function getApproved(uint256 credisId) external view returns (address);
    function isApprovedForAll(address owner, address operator) external view returns (bool);

    function credisExists(uint256 credisId) external view returns (bool);

    // ERC-721 Enumerable: ids in issuance order, globally and per owner.
    function tokenByIndex(uint256 index) external view returns (uint256);
    function balanceOf(address owner) external view returns (uint256 balance);
    function tokenOfOwnerByIndex(address owner, uint256 index) external view returns (uint256);

    /// @notice Interest accrued on the outstanding principal since the last
    ///         settlement (simple, ACT/365), evaluated at the current block
    ///         timestamp. This is the next payment's interest delta and the
    ///         minimum acceptable payment. Lifetime interest collected is
    ///         {interestPaidMinor}.
    function interestAccruedMinor(uint256 credisId) external view returns (uint256);

    /// @notice Lifetime interest collected on this Credis, in asset minor units.
    ///         Equals the sum of `SettlementApplied.interestPaidMinor` over successful
    ///         settlements. Forfeiture does not add unpaid interest.
    function interestPaidMinor(uint256 credisId) external view returns (uint256);

    /// @notice Sum of `principalMinor` and `outstandingPrincipalMinor` across the account's
    ///         Credis.
    function credisPrincipalAndOutstandingOf(address owner)
        external
        view
        returns (uint256 principalMinor, uint256 outstandingPrincipalMinor);

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
