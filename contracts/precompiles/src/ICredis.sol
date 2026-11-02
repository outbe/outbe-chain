// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @notice Principal and interest amounts are in the Credis asset's atomic units. Gratis
///         amounts and prices are at scale 1e6.
interface ICredis {
    error CredisNotFound();
    error InvalidStateValue(uint8 value);
    error ArithmeticOverflow();
    error IndexOutOfBounds();
    error NonTransferable();
    error UnknownProfileSelector(uint8 value);
    error DateStep(uint8 kind, uint32 dateKey);

    /// Emitted when a Credis opens, and to address(0) when a forfeited Credis is deleted.
    event Transfer(address indexed from, address indexed to, uint256 indexed tokenId);
    // Declared for ERC-721 shape only. A Credis is bound to its owner, so these two
    // are never emitted.
    event Approval(address indexed owner, address indexed approved, uint256 indexed tokenId);
    event ApprovalForAll(address indexed owner, address indexed operator, bool approved);
    /// ERC-4906: emitted on every settlement. A lapsed Credis reads Forfeited until the
    ///         cleanup deletes it.
    event MetadataUpdate(uint256 _tokenId);
    /// ERC-4906: emitted over the whole id range when groups are called.
    event BatchMetadataUpdate(uint256 _fromTokenId, uint256 _toTokenId);

    /// Every unsettled Credis of the series is called.
    event CredisSeriesCalled(
        bytes14 indexed seriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint64 calledAt,
        uint64 settlementDeadline
    );

    /// The series's settlement deadline passed: its `forfeitedCount` unsettled Credis and
    /// their `forfeitedAmountMinor` Gratis are forfeited. The cleanup burns each one's
    /// Gratis into the Promis reserve and deletes it.
    event CredisSeriesClosed(
        bytes14 indexed seriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint32 forfeitedCount,
        uint256 forfeitedAmountMinor
    );

    /// @notice A forfeited Credis is deleted: by its owner, or by the cleanup after its series's
    ///         deadline. Records the principal forfeited. Unpaid interest is left out of this
    ///         event and out of `interestSettledMinor`.
    event CredisForfeited(
        uint256 indexed credisId,
        address indexed owner,
        address indexed cca,
        address asset,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint256 forfeitedPrincipalMinor
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

    /// Call terms in the reference currency. A live Credis is called and forfeited on the
    /// window, threshold and notice its series's first Credis sealed.
    struct CallTerms {
        /// Call anchor price, scale 1e6: the previous closed UTC-day VWAP, fixed by the reservation.
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

    /// Where the principal and the pledged Gratis went, cumulative.
    struct Outcome {
        uint256 principalSettledMinor;
        /// Principal left unpaid at forfeiture.
        uint256 forfeitedPrincipalMinor;
        uint256 gratisReleasedMinor;
        /// Gratis forfeited at forfeiture.
        uint256 forfeitedGratisMinor;
    }

    /// principalMinor = principalSettledMinor + outstandingPrincipalMinor + forfeitedPrincipalMinor
    /// and gratisMinor = gratisReleasedMinor + outstandingGratisMinor + forfeitedGratisMinor.
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
        /// r - the annual policy rate of the issuance currency, scale 1e6, fixed by the reservation.
        uint256 policyRate;
        /// Entry price in the issuance currency, scale 1e6, fixed by the reservation.
        uint256 entryPriceMinor;
        /// Issuance timestamp.
        uint64 issuedAt;
        /// Anchor of the interest day count: issuance, advanced by the whole days each
        /// settlement charges. It may differ from the last settlement's time.
        uint64 lastSettledAt;
        /// See {State}. Read-time: a Called Credis past its settlement deadline reads
        /// Forfeited, with its outcome, until the cleanup deletes it.
        uint8 state;
        /// Sum of interest settled on this Credis, in asset minor units. Current-period accrual is
        /// {interestAccruedMinor}.
        uint256 interestSettledMinor;
        CallTerms call;
        Outcome outcome;
        /// The Credis series the Credis belongs to.
        bytes14 seriesId;
    }

    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function tokenURI(uint256 credisId) external view returns (string memory);

    function getCredis(uint256 credisId) external view returns (Credis memory);
    function ownerOf(uint256 credisId) external view returns (address);

    // ERC-721 transfer surface. A Credis is bound to its owner: transfers and
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

    function balanceOf(address owner) external view returns (uint256 balance);
    function tokensByOwner(address owner) external view returns (uint256[] memory tokenIds);

    /// @notice Interest accrued on the outstanding principal since the last
    ///         settlement (simple, ACT/365), evaluated at the current block
    ///         timestamp. While the Credis is settleable, this is the next payment's
    ///         interest delta and the minimum acceptable payment; 0 once it reads
    ///         Settled or Forfeited. Sum of interest settled is
    ///         {interestSettledMinor}.
    function interestAccruedMinor(uint256 credisId) external view returns (uint256);

    /// @notice Sum of interest settled on this Credis, in asset minor units.
    ///         Forfeiture does not add unpaid interest.
    function interestSettledMinor(uint256 credisId) external view returns (uint256);

    /// @notice Sum of `principalMinor` and `outstandingPrincipalMinor` across the account's
    ///         live Credis. A Credis past its settlement deadline counts until the cleanup
    ///         deletes it.
    function credisPrincipalAndOutstandingOf(address owner)
        external
        view
        returns (uint256 principalMinor, uint256 outstandingPrincipalMinor);

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
