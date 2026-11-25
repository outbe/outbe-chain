// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @notice Amounts and fees are in the Credis asset's atomic units. Gratis
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

    event CredisIssued(
        uint256 indexed credisId,
        address indexed owner,
        bytes14 indexed seriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        address cca,
        address asset,
        uint256 amountMinor,
        uint256 gratisMinor,
        uint256 policyRate
    );

    /// @notice A forfeited Credis is deleted: by its owner, or by the cleanup after its group's
    ///         deadline. Records the amount forfeited. Unpaid fee is left out of this
    ///         event and out of `settledFeeMinor`.
    event CredisForfeited(
        uint256 indexed credisId,
        address indexed owner,
        address indexed cca,
        address asset,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint256 forfeitedAmountMinor
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

    /// Call terms in the reference currency, sealed by the first Credis of the call group.
    struct CallTerms {
        /// entryPriceMinor marked up by the call rate.
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

    /// Where the amount and the pledged Gratis went, cumulative.
    struct Outcome {
        uint256 settledAmountMinor;
        /// Amount left unpaid at forfeiture.
        uint256 forfeitedAmountMinor;
        uint256 unpledgedGratisMinor;
        /// Gratis forfeited at forfeiture.
        uint256 forfeitedGratisMinor;
    }

    /// amountMinor = settledAmountMinor + outstandingAmountMinor + forfeitedAmountMinor
    /// and gratisMinor = unpledgedGratisMinor + outstandingGratisMinor + forfeitedGratisMinor.
    struct CredisData {
        uint256 credisId;
        address owner;
        address cca;
        address asset;
        /// ISO 4217 numeric code of the disbursed asset.
        uint16 issuanceCurrency;
        /// ISO 4217 numeric code of the reference currency elected at origination
        /// and fixed for the Credis's life.
        uint16 referenceCurrency;
        /// P - the stablecoin amount disbursed. Never changes.
        uint256 amountMinor;
        /// P_out - decreases with each settlement; 0 once Settled or Forfeited.
        uint256 outstandingAmountMinor;
        /// G - the pledged Gratis at the 24-hour VWAP of the reservation hour.
        uint256 gratisMinor;
        /// The share of G still locked; 0 once Settled or Forfeited.
        uint256 outstandingGratisMinor;
        /// r - the annual policy rate of the issuance currency, scale 1e6, fixed by the reservation.
        uint256 policyRate;
        /// Entry price, scale 1e6: the 24-hour VWAP in the reference currency at the reservation hour.
        uint256 entryPriceMinor;
        /// Issuance timestamp.
        uint64 issuedAt;
        /// Anchor of the fee day count: issuance, advanced by the whole days each
        /// settlement charges. It may differ from the last settlement's time.
        uint64 lastSettledAt;
        /// See {State}. Read-time: a Called Credis past its settlement deadline reads
        /// Forfeited, with its outcome, until the cleanup deletes it.
        uint8 state;
        /// Sum of the fees settled on this Credis, in asset minor units. Current-period accrual is
        /// {accruedFeeMinor}.
        uint256 settledFeeMinor;
        CallTerms call;
        Outcome outcome;
        /// The Credis series the Credis belongs to: its reservation's UTC day and currencies.
        bytes14 seriesId;
        /// The sub-series: the UTC hour of the reservation, 0-23.
        uint8 subSeries;
    }

    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function tokenURI(uint256 credisId) external view returns (string memory);

    function getCredis(uint256 credisId) external view returns (CredisData memory);
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

    /// @notice Fee accrued on the outstanding amount since the last
    ///         settlement (simple, ACT/365), evaluated at the current block
    ///         timestamp. While the Credis is settleable, this is the next payment's
    ///         fee delta and the minimum acceptable payment; 0 once it reads
    ///         Settled or Forfeited. Sum of the fees settled is
    ///         {settledFeeMinor}.
    function accruedFeeMinor(uint256 credisId) external view returns (uint256);

    /// @notice Sum of the fees settled on this Credis, in asset minor units.
    ///         Forfeiture does not add unpaid fee.
    function settledFeeMinor(uint256 credisId) external view returns (uint256);

    /// @notice Sum of `amountMinor` and `outstandingAmountMinor` across the account's
    ///         live Credis. A Credis past its settlement deadline counts until the cleanup
    ///         deletes it.
    function credisAmountAndOutstandingOf(address owner)
        external
        view
        returns (uint256 amountMinor, uint256 outstandingAmountMinor);

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
