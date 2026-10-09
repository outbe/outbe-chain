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
    /// ERC-4906: emitted when a Credis is called, on every settlement, and when the forfeit
    ///         sweep forfeits it. A lapsed Credis reads Forfeited before that event.
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

    /// Call terms, sealed at issuance in the reference currency.
    struct CallTerms {
        /// Call anchor price, scale 1e6: the previous closed UTC-day VWAP at issuance.
        uint256 callAnchorPriceMinor;
        /// callAnchorPriceMinor * 1.64.
        uint256 callPriceMinor;
        /// Breach window and threshold, in seconds.
        uint32 callWindow;
        uint32 callThreshold;
        /// Settlement notice after a call, in seconds.
        uint32 callNoticePeriod;
        /// 0 until the Credis is called.
        uint64 calledAt;
        /// Inclusive deadline: 0 when uncalled.
        uint64 settlementDeadline;
    }

    /// Where the principal and the pledged Gratis went, cumulative. Per-payment
    /// deltas are in the events.
    struct Outcome {
        uint256 principalPaidMinor;
        /// Principal left unpaid at forfeiture.
        uint256 principalWrittenOffMinor;
        uint256 gratisReturnedMinor;
        /// Gratis burned at forfeiture.
        uint256 gratisBurnedMinor;
    }

    /// principalMinor = principalPaidMinor + outstandingPrincipalMinor + principalWrittenOffMinor
    /// and gratisMinor = gratisReturnedMinor + outstandingGratisMinor + gratisBurnedMinor.
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
        /// P_out - decreases with each settlement; 0 once Settled or Forfeited.
        uint256 outstandingPrincipalMinor;
        /// G - the pledged Gratis: principal priced at the reservation's eight-hour VWAP.
        uint256 gratisMinor;
        /// The share of G still locked; 0 once Settled or Forfeited.
        uint256 outstandingGratisMinor;
        /// r - the annual policy rate of the issuance currency, scale 1e6, fixed at issuance.
        uint256 policyRate;
        /// Entry price in the issuance currency, scale 1e6, fixed by the reservation.
        uint256 entryPriceMinor;
        /// Issuance timestamp.
        uint64 issuedAt;
        /// Anchor of the interest day count: issuance, advanced by the whole days each
        /// settlement charges. It may differ from the last settlement's time.
        uint64 lastSettledAt;
        /// See {State}. Read-time: a Called Credis past its settlement deadline reads
        /// Forfeited, with its outcome, before the forfeit sweep reaches it.
        uint8 state;
        /// Lifetime interest collected, in asset minor units. The sum of successful
        /// `SettlementApplied.interestPaidMinor` payments. Current-period accrual is
        /// {interestAccruedMinor}.
        uint256 interestPaidMinor;
        CallTerms call;
        Outcome outcome;
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
    ///         timestamp. While the Credis is settleable, this is the next payment's
    ///         interest delta and the minimum acceptable payment; 0 once it reads
    ///         Settled or Forfeited. Lifetime interest collected is
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
