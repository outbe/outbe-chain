// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ICredis {
    event PositionCreated(
        uint256 indexed positionId,
        address indexed smartAccount,
        address indexed cca,
        uint256 principal,
        uint256 collateral
    );

    event PositionSettleable(uint256 indexed positionId, uint256 floorPrice);

    event PositionCalled(uint256 indexed positionId, uint64 calledAt, uint64 settlementDeadline);

    event SettlementApplied(
        uint256 indexed positionId,
        uint256 interestPaid,
        uint256 principalPaid,
        uint256 gratisReleased,
        uint256 outstanding
    );

    event PositionSettled(uint256 indexed positionId);

    event PositionVoided(
        uint256 indexed positionId,
        address indexed cca,
        uint256 gratisBurned,
        uint256 principalWrittenOff,
        uint256 interestWrittenOff
    );

    /// @notice Lifecycle state of a position, mirroring the Rust `CredisState`.
    ///         `Open -> Settleable -> (Called) -> Settled | Void`.
    enum State {
        Open,
        Settleable,
        Called,
        Settled,
        Void
    }

    struct Position {
        uint256 positionId;
        address smartAccount;
        address cca;
        address asset;
        /// ISO 4217 numeric code of the position's currency, from the disbursed asset.
        uint16 issuanceCurrency;
        // Pledger EOA ciphertext (not an address). The enclave recovers
        // the plaintext EOA on-chain via a RevealOwner round-trip.
        bytes eoaCiphertext;
        /// P — the stablecoin amount disbursed. Never changes.
        uint256 principal;
        /// P_out — decreases with each settlement; the position closes at zero.
        uint256 outstanding;
        /// G — the pledged Gratis, valued 1:1 against principal at the entry price.
        uint256 collateral;
        /// The share of G still locked. Released principal-proportionally.
        uint256 collateralLocked;
        /// r — the annual policy rate of the currency, scale 1e6, fixed at opening.
        uint256 policyRate;
        /// P_0 — the COEN price in the position's currency, quoted at pledge time.
        uint256 entryPrice;
        /// P_0 + 8%. Crossing it latches the position settleable, permanently.
        uint256 floorPrice;
        /// P_0 + 32%. A sustained breach triggers the call.
        uint256 callPrice;
        uint64 originatedAt;
        /// Anchor of the interest day count: origination until the first settlement.
        uint64 lastSettledAt;
        /// 0 until the position is called.
        uint64 calledAt;
        /// See {State}.
        uint8 state;
    }

    function totalSupply() external view returns (uint256);
    function getPosition(uint256 positionId) external view returns (Position memory);
    function ownerOf(bytes calldata positionId) external view returns (address);

    function positionByIndex(uint256 index) external view returns (Position memory);

    function balanceOf(address smartAccount) external view returns (uint256 balance);
    function positionOfAddressByIndex(address smartAccount, uint256 index) external view returns (Position memory);


    /// @notice True while `smartAccount` holds any CALLED position. Such an owner
    ///         cannot open new positions until the call resolves.
    function hasCalledPosition(address smartAccount) external view returns (bool);

    /// @notice Interest accrued on the outstanding principal since the last
    ///         settlement (simple, ACT/365), evaluated at the current block
    ///         timestamp. A settlement below this amount is rejected, so this is
    ///         also the minimum acceptable payment.
    function accruedInterest(uint256 positionId) external view returns (uint256);

    /// @notice Sum of `principal` and `outstanding` across the account's positions.
    function credisPrincipalAndOutstandingOf(address smartAccount) external view returns (uint256, uint256);

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
