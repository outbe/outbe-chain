// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface INodFactory {
    error InvalidOwner();
    error InvalidEncryptedNod();
    error NodAlreadyExists();
    error NodNotFound();
    error EntryPriceOutOfBounds();
    error GroupNotFound();
    error NodAlreadySettled();
    error NodNotSettled();
    error NodNotIssued();
    error InsufficientProofOfWork();
    error SettlementDeadlinePassed();
    error EmptyGroupMemberSlotDuringCleanup(bytes32 groupKey);
    error MissingGroupMemberRecordDuringCleanup(bytes32 groupKey, uint256 nodId);
    error IneligibleGroupMember(bytes32 groupKey, uint256 nodId);
    error MissingGroupRecordDuringCleanup(bytes32 groupKey);
    error MissingGroupForCall(bytes32 groupKey);
    error GroupNotQueuedForTest();
    error CallPriceOverflow();
    error DateStep(uint8 kind, uint32 dateKey);

    event NodIssued(
        address indexed owner,
        uint256 nodId,
        bytes14 seriesId,
        uint256 leagueId,
        uint256 callPriceMinor,
        bytes encryptedGratisAmount,
        uint256 entryPriceMinor,
        uint256 settlementCostMinor
    );

    event NodMined(address indexed owner, uint256 nodId, bytes encryptedGratisAmount);

    /// @notice Emitted when a Nod is settled.
    event NodSettled(address indexed owner, uint256 nodId, address asset, uint256 paymentMinor);

    /// @notice Pay an uncalled Nod, or a called one at or before its settlement
    /// deadline, in ERC20 base units of `asset`. Any caller may pay. The Nod stays
    /// with its owner. Approve NodFactory for the `quoteSettlement` amount first.
    /// The asset must have a reserve vault and report the Nod's reference or
    /// issuance ISO 4217 code. Issuance-currency payment converts the
    /// reference-currency entry cost at the COEN cross rate of the trailing VWAP
    /// snapshot required at this block.
    /// @param snapshotId The snapshot `quoteSettlement` returned. An
    /// issuance-currency payment naming any other snapshot reverts. Ignored on
    /// the reference rail.
    function settleNod(uint256 nodId, address asset, uint256 snapshotId) external;

    /// @notice What settling `nodId` with `asset` costs, and which of the Nod's
    /// two currencies that asset settles on. Reverts for an asset the Nod
    /// does not accept.
    /// @return settlementCurrency ISO 4217 code the payment is denominated in.
    /// @return paymentMinor Amount to pay, in `asset`'s own minor units.
    /// @return snapshotId Trailing VWAP snapshot the amount converts at. Zero on
    /// the reference rail. It goes stale at the next update cutoff.
    function quoteSettlement(uint256 nodId, address asset)
        external
        view
        returns (uint16 settlementCurrency, uint256 paymentMinor, uint256 snapshotId);

    /// @notice How close a right is to its group's call. The window ends at the last
    ///         finalized UTC day. Prices are COEN in `referenceCurrency` at scale 1e6.
    struct CallProgress {
        uint32 callWindowDays;
        uint32 callThresholdDays;
        /// Window days since the group's start, priced or not.
        uint32 daysChecked;
        /// Of them, days whose VWAP was above `callPriceMinor`.
        uint32 daysAboveCallPrice;
        /// The fewest further days above the call price before the call, as the oldest
        /// days leave the window. 0 once the call is decided.
        uint32 minDaysToCall;
        uint16 referenceCurrency;
        uint256 callPriceMinor;
        /// VWAP of the last finalized day, 0 when it has none.
        uint256 lastDayPriceMinor;
    }

    /// @notice Read the call progress of `nodId`. Reverts `NodNotFound`, or
    ///         `NodNotIssued` once it is Called, Settled or Forfeited.
    function getCallProgress(uint256 nodId) external view returns (CallProgress memory);

    /// @notice Mine a paid Nod and mint its Gratis load to the Nod owner.
    /// @param nonce PoW over
    /// `sha256("OUTBE_NOD_MINING_V1" || nodId_be32 || owner_20 || miningSequence_be8 || nonce_be8)`
    /// with `miningSequence = 0` and the required leading zero bytes. The owner is the Nod owner.
    /// @param mac Gratis mint authorization under the owner's modify key.
    /// @param opNonce The owner's current Gratis operation nonce, bound by `mac`.
    function mineGratis(uint256 nodId, uint64 nonce, bytes32 mac, uint64 opNonce)
        external
        returns (bytes memory encryptedBalance);
}
