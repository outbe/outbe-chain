// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface INodFactory {
    event NodIssued(
        address indexed owner,
        uint256 nodId,
        uint256 worldwideDay,
        uint256 leagueId,
        uint256 callPriceMinor,
        bytes encryptedGratisAmount,
        uint256 entryPriceMinor,
        uint256 settlementCostMinor
    );

    event NodExercised(address indexed owner, uint256 nodId, bytes encryptedGratisAmount);

    event NodBurned(address indexed owner, uint256 nodId, bytes encryptedGratisAmount);

    /// @notice Emitted when a Nod is settled.
    event NodSettled(address indexed owner, uint256 nodId, address asset, uint256 paymentMinor);

    /// @notice The daily call sweep (`sweep` = 1) fell two days behind. `skippedDay`
    ///         gave its place to a newer day, and the sweep will not walk it.
    event SweepDaySkipped(uint8 indexed sweep, uint32 skippedDay, uint32 inFlightDay);

    /// @notice A reference currency was left out of one day's Call scan because its
    ///         window price could not be indexed. The next daily pass tries it again.
    event CallScanSkipped(uint16 indexed referenceCurrency, uint32 indexed utcDay);

    /// @notice A lapsed bucket kept a Nod that could not burn, or its Nods were still
    ///         landing. The forfeit sweep comes back to it at `retryAt`.
    event ExpiryDeferred(bytes32 indexed bucketKey, uint64 retryAt);

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

    /// @notice Exercise a paid Nod and mint its Gratis load to the Nod owner.
    /// @param nonce PoW over
    /// `sha256("OUTBE_NOD_MINING_V1" || nodId_be32 || owner_20 || miningSequence_be8 || nonce_be8)`
    /// with `miningSequence = 0` and the required leading zero bytes. The owner is the Nod owner.
    /// @param mac Gratis mint authorization under the owner's modify key.
    /// @param opNonce The owner's current Gratis operation nonce, bound by `mac`.
    function mineGratis(uint256 nodId, uint64 nonce, bytes32 mac, uint64 opNonce)
        external
        returns (bytes memory encryptedBalance);
}
