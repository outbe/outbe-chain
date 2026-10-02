// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @notice Principal and interest amounts are in the position asset's atomic units; Gratis
///         amounts and prices are at scale 1e6.
interface ICredis {
    /// Emitted once, when a position opens: positions are never burned.
    event Transfer(address indexed from, address indexed to, uint256 indexed tokenId);
    // Declared for ERC-721 shape only: a position is bound to its smart account, so these two are never emitted.
    event Approval(address indexed owner, address indexed approved, uint256 indexed tokenId);
    event ApprovalForAll(address indexed owner, address indexed operator, bool approved);
    /// ERC-4906: emitted when a position is called, on every settlement, and when it is voided.
    event MetadataUpdate(uint256 _tokenId);
    /// ERC-4906, declared for the standard's shape only: never emitted.
    event BatchMetadataUpdate(uint256 _fromTokenId, uint256 _toTokenId);

    event PositionCreated(
        uint256 indexed positionId,
        address indexed smartAccount,
        address indexed cca,
        uint256 principalMinor,
        uint256 gratisMinor
    );

    event PositionCalled(uint256 indexed positionId, uint64 calledAt, uint64 settlementDeadline);

    /// @notice One successful settlement. `interestMinor` is this payment's interest.
    ///         The position's `interestPaidMinor` is the sum of these payments
    ///         across successful settlements. A reverted settlement emits nothing
    ///         and leaves that total unchanged.
    event SettlementApplied(
        uint256 indexed positionId,
        uint256 interestMinor,
        uint256 principalPaidMinor,
        uint256 gratisReturnedMinor,
        uint256 outstandingPrincipalMinor
    );

    event PositionSettled(uint256 indexed positionId);

    /// @notice Forfeiture of a called position. Records the principal written off.
    ///         Unpaid interest is left out of this event and out of `interestPaidMinor`.
    event PositionVoided(
        uint256 indexed positionId, address indexed cca, uint256 gratisBurnedMinor, uint256 principalWrittenOffMinor
    );

    /// @notice Lifecycle state of a position, mirroring the Rust `CredisState`.
    ///         `Open -> (Called) -> Settled | Void`. A position is settleable
    ///         from the moment it opens.
    enum State {
        Open,
        Called,
        Settled,
        Void
    }

    struct Position {
        uint256 positionId;
        address smartAccount;
        address cca;
        address asset;
        /// ISO 4217 numeric code of the disbursed asset.
        uint16 issuanceCurrency;
        /// ISO 4217 numeric code of the reference currency elected at origination
        /// and fixed for the position's life.
        uint16 referenceCurrency;
        // Pledger EOA ciphertext (not an address). The enclave recovers
        // the plaintext EOA on-chain via a RevealOwner round-trip.
        bytes eoaCiphertext;
        /// P - the stablecoin amount disbursed. Never changes.
        uint256 principalMinor;
        /// P_out - decreases with each settlement; the position closes at zero.
        uint256 outstandingPrincipalMinor;
        /// G - the pledged Gratis, valued 1:1 against principal at the pledge quote rate.
        uint256 gratisMinor;
        /// The share of G still locked. Released principal-proportionally.
        uint256 outstandingGratisMinor;
        /// r - the annual policy rate of the issuance currency, scale 1e6, fixed at opening.
        uint256 policyRate;
        /// Entry price in the issuance currency, scale 1e6, sealed on the pledge.
        uint256 entryPriceMinor;
        /// Call anchor price in the reference currency, scale 1e6, sealed at issuance.
        uint256 callAnchorPriceMinor;
        /// callAnchorPriceMinor * 1.64, in the reference currency.
        uint256 callPriceMinor;
        /// Issuance timestamp.
        uint64 issuedAt;
        /// Anchor of the interest day count: origination until the first settlement.
        uint64 lastSettledAt;
        /// 0 until the position is called.
        uint64 calledAt;
        /// See {State}.
        uint8 state;
        /// Lifetime interest collected, in asset minor units. The sum of successful
        /// `SettlementApplied.interestMinor` payments. Current-period accrual is
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
    function tokenURI(uint256 positionId) external view returns (string memory);

    function totalSupply() external view returns (uint256);
    function getPosition(uint256 positionId) external view returns (Position memory);
    function ownerOf(uint256 positionId) external view returns (address);

    // ERC-721 transfer surface. A position is bound to its smart account: transfers and approvals always revert.
    function transferFrom(address from, address to, uint256 positionId) external;
    function safeTransferFrom(address from, address to, uint256 positionId) external;
    function safeTransferFrom(address from, address to, uint256 positionId, bytes calldata data) external;
    function approve(address to, uint256 positionId) external;
    function setApprovalForAll(address operator, bool approved) external;
    // No approval can exist: these read address(0) and false.
    function getApproved(uint256 positionId) external view returns (address);
    function isApprovedForAll(address owner, address operator) external view returns (bool);

    function positionByIndex(uint256 index) external view returns (Position memory);

    function balanceOf(address smartAccount) external view returns (uint256 balance);
    function positionOfAddressByIndex(address smartAccount, uint256 index) external view returns (Position memory);

    /// @notice True while `smartAccount` holds any CALLED position.
    function hasCalledPosition(address smartAccount) external view returns (bool);

    /// @notice Interest accrued on the outstanding principal since the last
    ///         settlement (simple, ACT/365), evaluated at the current block
    ///         timestamp. This is the next payment's interest delta and the
    ///         minimum acceptable payment. Lifetime interest collected is
    ///         {interestPaidMinor}.
    function interestAccruedMinor(uint256 positionId) external view returns (uint256);

    /// @notice Lifetime interest collected on this position, in asset minor units.
    ///         Equals the sum of `SettlementApplied.interestMinor` over successful
    ///         settlements. Forfeiture does not add unpaid interest.
    function interestPaidMinor(uint256 positionId) external view returns (uint256);

    /// @notice Sum of `principalMinor` and `outstandingPrincipalMinor` across the account's positions.
    function credisPrincipalAndOutstandingOf(address smartAccount) external view returns (uint256, uint256);

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
