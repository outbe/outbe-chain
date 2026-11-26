// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface INodFactory {
    error InvalidOwner();
    error InvalidEncryptedNod();
    error NodAlreadyExists();
    error NodNotFound();
    error NotNodOwner();
    error EntryPriceOutOfBounds();
    error GroupNotFound();
    error NodAlreadySettled();
    error NodNotSettled();
    error NodNotIssued();
    error InsufficientProofOfWork();
    error SettlementDeadlinePassed();
    error EmptyGroupMemberSlotDuringForfeit(bytes32 groupKey);
    error MissingGroupMemberRecordDuringForfeit(bytes32 groupKey, uint256 nodId);
    error IneligibleGroupMember(bytes32 groupKey, uint256 nodId);
    error MissingGroupRecordDuringForfeit(bytes32 groupKey);
    error MissingGroupForCall(bytes32 groupKey);
    error CallPriceOverflow();
    error DateStep(uint8 kind, uint32 dateKey);
    error EnclaveRejected(string reason);
    error OfferEnclaveFailed(string reason);
    error NodLoadBelowOneMinorUnit();
    error ZkProofRequired();
    error UnknownCircuitVersion(uint32 chainId, string version);
    error UnsupportedZkCircuit();
    error MalformedZkProof(string value);
    error ZkPublicInputMismatch(string field);
    error InvalidZkProof();
    error ZkVerifierUnavailable(string value);
    error InvalidWorldwideDay(uint32 worldwideDay);
    error WorldwideDayNotOffering(uint32 worldwideDay);
    error IssuanceCurrencyNotRegistered(uint16 issuanceCurrency);
    error CurrencyPriceUnavailable(uint32 worldwideDay);
    error EmptySuHashList();
    error InvalidSuHashHex(string hash);
    error InvalidSuHashLength(uint64 length);
    error WalletAddressesRequiredWhenSraProvided();
    error SraAddressesRequiredWhenWalletProvided();
    error InvalidWalletAddressAtIndex(uint64 index, string walletAddress);
    error InvalidSraAddressAtIndex(uint64 index, string sraAddress);

    /// The first Nod of sub-series `subSeriesId` opened it with its entry price, its call
    /// price and the call terms of its group.
    event NodSeriesOpened(
        bytes14 indexed seriesId,
        uint16 indexed subSeriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint256 entryPriceMinor,
        uint256 callPriceMinor,
        uint64 callWindow,
        uint64 callThreshold,
        uint64 callNoticePeriod
    );
    /// The daily call scan called the group of the sub-series. Every Nod in it must be
    /// settled by `settlementDeadline`, or it is forfeited when the sub-series closes.
    event NodSeriesCalled(
        bytes14 indexed seriesId,
        uint16 indexed subSeriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint64 calledAt,
        uint64 settlementDeadline
    );
    /// The settlement deadline lapsed: the `forfeitedCount` unsettled Nods of the sub-series
    /// are forfeited and their `forfeitedAmountMinor` returned to the Promis Limit at once.
    /// The cleanup deletes them after.
    event NodSeriesClosed(
        bytes14 indexed seriesId,
        uint16 indexed subSeriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint32 forfeitedCount,
        uint256 forfeitedAmountMinor
    );

    /// @notice Offer the spends of `worldwideDay` and issue their Nod in this transaction.
    /// The Nod gets its entry price, its call price and its Gratis load: 32% of the
    /// nominal of the offer. Reverts with no Nod when a currency has no 24-hour VWAP at
    /// the offer hour, or when the load is below one minor unit.
    /// @param cipherText The offer payload, encrypted to the network offer key.
    /// @param chainId The registered L2 that signed `zkMerkleRoot`.
    /// @param version The exact L2 circuit version of `zkProof`.
    /// @param signature The L2 committee's BLS MinSig signature in G1 over `zkMerkleRoot`.
    function issueNod(
        bytes calldata cipherText,
        bytes calldata nonce,
        uint256 ephemeralPubkey,
        uint32 worldwideDay,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        bytes calldata zkProof,
        uint32 chainId,
        string calldata version,
        bytes calldata zkMerkleRoot,
        bytes calldata signature
    ) external returns (uint256 nodId);

    /// @notice Settle an uncalled Nod, or a called one at or before its settlement
    /// deadline, in ERC20 base units of `asset`. Any caller may pay. The Nod stays
    /// with its owner. Approve the VaultRouter for `amountCapMinor` first.
    /// The asset must have a reserve vault and report the Nod's reference or
    /// issuance ISO 4217 code. Issuance-currency payment converts the
    /// reference-currency entry cost at the COEN cross rate of the trailing VWAP
    /// snapshot required at this block.
    /// @param snapshotId The snapshot `quoteSettlement` returned. An
    /// issuance-currency payment naming any other snapshot reverts. Ignored on
    /// the reference rail.
    /// @param amountCapMinor The most the payer pays, in `asset` units. A payer whose
    /// allowance or balance is below it reverts `InsufficientFunds`; a cost above
    /// it reverts `CostAboveCap`. The call takes only the cost.
    function settleNod(uint256 nodId, address asset, uint256 snapshotId, uint256 amountCapMinor) external;

    /// @notice What settling `nodId` with `asset` costs, and which of the Nod's
    /// two currencies that asset settles on. Only the Nod owner may call this
    /// quote; reverts for any other caller or for an asset the Nod does not accept.
    /// @return settlementCurrency ISO 4217 code the payment is denominated in.
    /// @return encryptedPaymentMinor Amount to pay in `asset`'s own minor units,
    /// sealed to the Nod owner's view key.
    /// @return snapshotId Trailing VWAP snapshot the amount converts at. Zero on
    /// the reference rail. It goes stale at the next update cutoff.
    function quoteSettlement(uint256 nodId, address asset)
        external
        view
        returns (uint16 settlementCurrency, bytes memory encryptedPaymentMinor, uint256 snapshotId);

    /// @notice How close a right is to its group's call. The window ends at the last
    ///         finalized UTC day. Prices are COEN in `referenceCurrency` at scale 1e6.
    struct CallState {
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
    function callState(uint256 nodId) external view returns (CallState memory);

    /// @notice Mine a settled Nod and mint its Gratis load to the Nod owner.
    /// @param nonce PoW over
    /// `sha256("OUTBE_NOD_MINING_V1" || nodId_be32 || owner_20 || miningSequence_be8 || nonce_be8)`
    /// with `miningSequence = 0` and the required leading zero bytes. The owner is the Nod owner.
    /// @param mac Gratis mint authorization under the owner's modify key.
    /// @param opNonce The owner's current Gratis operation nonce, bound by `mac`.
    function mineGratis(uint256 nodId, uint64 nonce, bytes32 mac, uint64 opNonce)
        external
        returns (bytes memory encryptedBalance);
}
