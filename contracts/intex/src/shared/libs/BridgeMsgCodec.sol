// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IIntexAuction} from "../../target/interfaces/IIntexAuction.sol";

/// @title BridgeMsgCodec
/// @author Outbe
/// @notice Library for encoding and decoding bridge messages between BNB and Outbe chains.
/// @dev All auction/series messages are keyed by `seriesId` (uint32).
/// @dev Wire layout: `[bodyVersion(1)][msgType(1)][body]`. `bodyVersion` lets the format
///      evolve independently of `msgType`; decoders reject unknown versions.
library BridgeMsgCodec {
    /// @notice Active body version emitted by every `encode*` and required by every `decode*`.
    uint8 internal constant BODY_VERSION_V1 = 1;

    // Message types: BNB -> Outbe
    uint8 internal constant MSG_BIDS_BATCH = 1;

    // Message types: Outbe -> BNB
    uint8 internal constant MSG_AUCTION_STAGE_START = 4;
    uint8 internal constant MSG_AUCTION_STAGE_REVEAL = 5;
    uint8 internal constant MSG_AUCTION_STAGE_CLEARING = 6;
    uint8 internal constant MSG_AUCTION_RESULT = 7;
    uint8 internal constant MSG_ISSUANCE_INSTRUCTIONS = 8;
    uint8 internal constant MSG_REFUND_INSTRUCTIONS = 9;
    uint8 internal constant MSG_MARK_CALLED = 10;
    uint8 internal constant MSG_MARK_QUALIFIED = 11;

    /// @notice Upper bound on every caller-supplied cross-chain payload array
    ///         (`BIDS_BATCH`, `ISSUANCE_INSTRUCTIONS`, `REFUND_INSTRUCTIONS`).
    /// @dev One system-wide cap (unified with the ONFT `MAX_BATCH_SIZE`). Derived from the binding
    ///      `maxMessageSize = 10_000` byte ceiling (bids ~128 B/item caps near 78) with
    ///      destination-gas headroom. Enforced OUTBOUND inside every `encode*` function (fail-fast
    ///      at the source) AND re-checked INBOUND inside the variable-length `decode*` functions
    ///      (defence-in-depth against a peer compromise or future encoder change). An inbound
    ///      over-cap revert is caught by the drop-don't-block handler so the ORDERED lane stays
    ///      live.
    uint16 internal constant MAX_PAYLOAD_ARRAY_LEN = 64;

    /// @notice Fixed-point scale for bid/clearing rates (`1e6` = 100%). Shared with the Outbe
    ///         `RATE_SCALE` and `IntexAuction`; escrow math is `qty * strike * rate / RATE_SCALE`.
    uint32 internal constant RATE_SCALE = 1_000_000;

    // --- Minimum encoded lengths ---
    // Header is fixed at 2 bytes: [bodyVersion(1)][msgType(1)].
    uint16 internal constant HEADER_LEN = 2;

    // encodePacked messages have a tight upper bound that equals the lower bound.
    uint16 internal constant MIN_LEN_AUCTION_STAGE_START = 76;
    uint16 internal constant MIN_LEN_AUCTION_STAGE_REVEAL = 7;
    uint16 internal constant MIN_LEN_AUCTION_STAGE_CLEARING = 6;
    uint16 internal constant MIN_LEN_AUCTION_RESULT = 22;
    uint16 internal constant MIN_LEN_MARK_CALLED = 6;
    uint16 internal constant MIN_LEN_MARK_QUALIFIED = 6;

    // abi.encode payloads have variable length. The minimum corresponds to all
    // dynamic arrays being empty:
    //   BIDS_BATCH(uint32, uint32, bool, uint32, t[]×4):
    //     4 static head words + 4 dynamic head offsets + 4 empty length words = 12×32 = 384
    //   REFUND_INSTRUCTIONS(uint32, address[], uint64[], uint64[]):
    //     1 static head word + 3 dynamic offsets + 3 empty length words = 7×32 = 224
    //   ISSUANCE_INSTRUCTIONS(struct with 12 static + 2 dynamic, dynamic struct):
    //     outer offset(32) + 12 static + 2 inner offsets + 2 empty length words = 17×32 = 544
    uint16 internal constant MIN_LEN_BIDS_BATCH = HEADER_LEN + 384;
    uint16 internal constant MIN_LEN_REFUND_INSTRUCTIONS = HEADER_LEN + 224;
    uint16 internal constant MIN_LEN_ISSUANCE_INSTRUCTIONS = HEADER_LEN + 544;

    /// @notice Per-message cap on inbound BIDS_BATCH entries. Bounds the crosschainMint/storage loop the
    ///         receiver runs so one oversized batch cannot exceed the inbound gas limit and stall
    ///         the ordered lane; larger bid sets are chunked into multiple batches by the sender.
    /// @dev Unified with the outbound `MAX_PAYLOAD_ARRAY_LEN` so inbound and outbound
    ///      agree on one number. The earlier value of 256 was the original ticket figure and is
    ///      physically unsendable: a bids batch is ~128 B/item, so 256 items is ~32 KB — over 3×
    ///      LayerZero's send-side `maxMessageSize = 10_000` byte cap (an over-cap send reverts on
    ///      the source chain). The real byte ceiling lands near 78 items; 64 sits under it with gas
    ///      headroom, and the outbound encoder already rejects anything larger.
    uint256 internal constant MAX_BIDS_BATCH = MAX_PAYLOAD_ARRAY_LEN;

    /// @notice Body decoded with an unsupported `bodyVersion` byte.
    /// @param got The version byte read from the payload.
    error UnsupportedBodyVersion(uint8 got);

    /// @notice Inbound payload is shorter than the minimum encoding for its `msgType`.
    /// @param msgType The message-type byte read from the payload.
    /// @param got The actual length of the inbound payload.
    /// @param minimum The minimum required length for this `msgType`.
    error InvalidPayloadLength(uint8 msgType, uint256 got, uint256 minimum);

    /// @notice Inbound payload's `msgType` is not in the handler's accepted set.
    /// @param got The unrecognised type byte.
    error UnknownMsgType(uint8 got);

    /// @notice A `bytes32` interpreted as an address has non-zero high bits.
    /// @dev The Solidity address ABI uses the low 20 bytes; high 12 bytes must be zero.
    /// @param got The malformed `bytes32` slot.
    error MalformedAddress(bytes32 got);

    /// @notice REFUND_INSTRUCTIONS parallel arrays decoded to unequal lengths.
    /// @param bidders Length of the bidders array.
    /// @param refundedAmounts Length of the refunded-amounts array.
    /// @param paidAmounts Length of the paid-amounts array.
    error RefundArrayLengthMismatch(uint256 bidders, uint256 refundedAmounts, uint256 paidAmounts);

    /// @notice Inbound BIDS_BATCH exceeds the per-message entry cap.
    /// @param count Decoded number of bidders.
    /// @param max Maximum permitted entries per batch.
    error BidsBatchTooLarge(uint256 count, uint256 max);

    /// @notice BIDS_BATCH parallel arrays decoded to unequal lengths.
    /// @param bidders Length of the bidder-addresses array.
    /// @param quantities Length of the intex-quantities array.
    /// @param rates Length of the intex-bid-rates array.
    /// @param timestamps Length of the timestamps array.
    error BidsArrayLengthMismatch(uint256 bidders, uint256 quantities, uint256 rates, uint256 timestamps);

    /// @notice ISSUANCE_INSTRUCTIONS parallel arrays decoded to unequal lengths.
    /// @param recipients Length of the recipients array.
    /// @param quantities Length of the quantities array.
    error IssuanceArrayLengthMismatch(uint256 recipients, uint256 quantities);

    /// @notice Inbound ISSUANCE_INSTRUCTIONS exceeds the per-message recipient cap.
    /// @param count Decoded number of recipients.
    /// @param max Maximum permitted recipients per message.
    error IssuanceBatchTooLarge(uint256 count, uint256 max);

    /// @notice Inbound REFUND_INSTRUCTIONS exceeds the per-message bidder cap.
    /// @param count Decoded number of bidders.
    /// @param max Maximum permitted bidders per message.
    error RefundBatchTooLarge(uint256 count, uint256 max);

    /// @notice The `isGreenDay` flag byte was neither `0x00` nor `0x01`.
    /// @dev A corrupted byte must not be silently coerced to `false`; reject it.
    /// @param got The out-of-range flag byte read from the payload.
    error InvalidGreenDayFlag(uint8 got);

    /// @notice An outbound payload array exceeds `MAX_PAYLOAD_ARRAY_LEN`.
    /// @dev Fail-fast on the source chain so the relayer learns before any LZ fee is burned.
    /// @param got The actual array length the encoder was given.
    /// @param max The configured `MAX_PAYLOAD_ARRAY_LEN`.
    error PayloadArrayTooLong(uint256 got, uint256 max);

    // --- Encoding ---

    /// @notice Reverts `PayloadArrayTooLong` if `_actual` exceeds `_max`.
    /// @dev The outbound cross-chain array cap. Called only from `encode*`.
    /// @param _actual The array length the encoder was given.
    /// @param _max The configured upper bound.
    function requireMaxArrayLen(uint256 _actual, uint256 _max) internal pure {
        if (_actual > _max) revert PayloadArrayTooLong(_actual, _max);
    }

    /// @notice Encodes BIDS_BATCH message.
    /// @dev `_isLast` marks the final chunk of a (possibly multi-chunk) bid set: the receiver
    ///      accumulates chunks and only finalizes the auction's bid collection on the last one.
    ///      `_relayGeneration` is stamped per flush so the receiver replaces a re-flushed set
    ///      rather than double-counting it. Reverts `PayloadArrayTooLong` if `_bidderAddresses`
    ///      exceeds `MAX_PAYLOAD_ARRAY_LEN`.
    /// @param _seriesId The auction series identifier.
    /// @param _srcEid The LayerZero source endpoint id the bids originated from.
    /// @param _isLast Whether this is the final chunk of the bid set.
    /// @param _relayGeneration The flush generation stamp the receiver uses to replace re-flushed sets.
    /// @param _bidderAddresses The bidder addresses (parallel with the other three arrays).
    /// @param _intexQuantities The intex quantities per bidder.
    /// @param _intexBidRates The intex bid rates per bidder (`1e6` fixed-point, % of strike).
    /// @param _timestamps The bid timestamps per bidder.
    /// @return The wire-encoded BIDS_BATCH message.
    function encodeBidsBatch(
        uint32 _seriesId,
        uint32 _srcEid,
        bool _isLast,
        uint32 _relayGeneration,
        address[] memory _bidderAddresses,
        uint16[] memory _intexQuantities,
        uint32[] memory _intexBidRates,
        uint32[] memory _timestamps
    ) internal pure returns (bytes memory) {
        // Decoder rejects parallel-array mismatch with BidsArrayLengthMismatch; fail-fast at the
        // source so a sender-side bug aborts before paying the LZ fee.
        if (
            _bidderAddresses.length != _intexQuantities.length || _bidderAddresses.length != _intexBidRates.length
                || _bidderAddresses.length != _timestamps.length
        ) {
            revert BidsArrayLengthMismatch(
                _bidderAddresses.length, _intexQuantities.length, _intexBidRates.length, _timestamps.length
            );
        }
        requireMaxArrayLen(_bidderAddresses.length, MAX_PAYLOAD_ARRAY_LEN);
        return abi.encodePacked(
            BODY_VERSION_V1,
            MSG_BIDS_BATCH,
            abi.encode(
                _seriesId,
                _srcEid,
                _isLast,
                _relayGeneration,
                _bidderAddresses,
                _intexQuantities,
                _intexBidRates,
                _timestamps
            )
        );
    }

    /// @notice Encodes AUCTION_STAGE_START message.
    /// @dev encodePacked layout (76 bytes), field order mirrors the Outbe `sol_ext` struct:
    ///      [bodyVersion(1)][msgType(1)][seriesId(4)][commitEnd(4)][revealEnd(4)][issuanceEnd(4)]
    ///      [issuanceCurrency(2)][referenceCurrency(2)][promisLoadMinor(16)][minIntexBidRate(4)][entryPrice(8)][floorPriceMinor(8)]
    ///      [callPriceMinor(8)][intexCallPeriod(4)][callWindowDays(2)][callThresholdDays(2)][minIntexBidQuantity(2)]
    /// @param _seriesId The auction series identifier.
    /// @param _commitEnd The commit-stage end timestamp.
    /// @param _revealEnd The reveal-stage end timestamp.
    /// @param _issuanceEnd The issuance-stage end timestamp.
    /// @param _issuanceCurrency The issuance currency (ISO numeric).
    /// @param _referenceCurrency The reference currency (ISO numeric).
    /// @param _promisLoadMinor The Promis load (minor units) for the series.
    /// @param _minIntexBidRate The minimum acceptable intex bid rate (`1e6` fixed-point).
    /// @param _entryPrice The per-unit entry price (reference ccy); strike derives from it.
    /// @param _floorPriceMinor The floor price (minor units).
    /// @param _callPriceMinor The call price (minor units).
    /// @param _intexCallPeriod The Called→deadline window in seconds (0 = default).
    /// @param _callWindowDays The call-trigger observation window in days.
    /// @param _callThresholdDays The call-trigger threshold in days.
    /// @param _minIntexBidQuantity The minimum acceptable intex bid quantity.
    /// @return The wire-encoded AUCTION_STAGE_START message.
    function encodeAuctionStageStart(
        uint32 _seriesId,
        uint32 _commitEnd,
        uint32 _revealEnd,
        uint32 _issuanceEnd,
        uint16 _issuanceCurrency,
        uint16 _referenceCurrency,
        uint128 _promisLoadMinor,
        uint32 _minIntexBidRate,
        uint64 _entryPrice,
        uint64 _floorPriceMinor,
        uint64 _callPriceMinor,
        uint32 _intexCallPeriod,
        uint16 _callWindowDays,
        uint16 _callThresholdDays,
        uint16 _minIntexBidQuantity
    ) internal pure returns (bytes memory) {
        return abi.encodePacked(
            BODY_VERSION_V1,
            MSG_AUCTION_STAGE_START,
            _seriesId,
            _commitEnd,
            _revealEnd,
            _issuanceEnd,
            _issuanceCurrency,
            _referenceCurrency,
            _promisLoadMinor,
            _minIntexBidRate,
            _entryPrice,
            _floorPriceMinor,
            _callPriceMinor,
            _intexCallPeriod,
            _callWindowDays,
            _callThresholdDays,
            _minIntexBidQuantity
        );
    }

    /// @notice Encodes AUCTION_STAGE_REVEAL message.
    /// @dev encodePacked layout (7 bytes): [bodyVersion(1)][msgType(1)][seriesId(4)][isGreenDay(1)]
    /// @param _seriesId The auction series identifier.
    /// @param _isGreenDay The green-day flag for the series.
    /// @return The wire-encoded AUCTION_STAGE_REVEAL message.
    function encodeAuctionStageReveal(uint32 _seriesId, bool _isGreenDay) internal pure returns (bytes memory) {
        return abi.encodePacked(BODY_VERSION_V1, MSG_AUCTION_STAGE_REVEAL, _seriesId, _isGreenDay);
    }

    /// @notice Encodes AUCTION_STAGE_CLEARING message.
    /// @dev encodePacked layout (6 bytes): [bodyVersion(1)][msgType(1)][seriesId(4)]
    /// @param _seriesId The auction series identifier.
    /// @return The wire-encoded AUCTION_STAGE_CLEARING message.
    function encodeAuctionStageClearing(uint32 _seriesId) internal pure returns (bytes memory) {
        return abi.encodePacked(BODY_VERSION_V1, MSG_AUCTION_STAGE_CLEARING, _seriesId);
    }

    /// @notice Encodes AUCTION_RESULT message.
    /// @dev encodePacked layout (22 bytes):
    ///      [bodyVersion(1)][msgType(1)][seriesId(4)][issuedIntexCount(4)][auctionClearingRate(8)][wonBidsCount(4)]
    /// @param _seriesId The auction series identifier.
    /// @param _issuedIntexCount The number of intex issued by the cleared auction.
    /// @param _auctionClearingRate The uniform auction clearing rate (`1e6` fixed-point).
    /// @param _wonBidsCount The number of winning bids.
    /// @return The wire-encoded AUCTION_RESULT message.
    function encodeAuctionResult(
        uint32 _seriesId,
        uint32 _issuedIntexCount,
        uint64 _auctionClearingRate,
        uint32 _wonBidsCount
    ) internal pure returns (bytes memory) {
        return abi.encodePacked(
            BODY_VERSION_V1, MSG_AUCTION_RESULT, _seriesId, _issuedIntexCount, _auctionClearingRate, _wonBidsCount
        );
    }

    /// @notice Issuance instructions payload — grouped into a struct to keep the
    ///         encoder/decoder API resilient against EVM stack depth limits.
    /// @dev `issuedIntexCount` mirrors the auction-cleared count; the destination chain
    ///      pins it on `SeriesData` and `IntexNFT1155.mint`/`mintBatch` reject any mint
    ///      that would push `totalSupply` past it.
    struct IssuanceInstructionsPayload {
        uint32 seriesId;
        uint32 issuedIntexCount;
        uint128 promisLoadMinor;
        uint64 costAmountMinor;
        uint64 entryPriceMinor;
        uint64 floorPriceMinor;
        /// @notice Duration in seconds between Called and the settlement deadline; 0 uses default.
        uint32 intexCallPeriod;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint16 callWindowDays;
        uint16 callThresholdDays;
        uint64 callPriceMinor;
        address[] recipients;
        uint256[] quantities;
    }

    /// @notice Decode AUCTION_STAGE_START straight into the auction schedule + params structs.
    ///         Kept `external` so the struct construction lives in the linked library, off the
    ///         messenger's runtime size (EIP-170). Mirrors `decodeAuctionStageStart`'s layout.
    /// @param _msg The wire-encoded AUCTION_STAGE_START message.
    /// @return seriesId The auction series identifier.
    /// @return schedule The decoded commit/reveal/issuance schedule.
    /// @return params The decoded auction params.
    function decodeAuctionParams(bytes calldata _msg)
        external
        pure
        returns (uint32 seriesId, IIntexAuction.AuctionSchedule memory schedule, IIntexAuction.AuctionParams memory params)
    {
        _assertExactLength(_msg, MSG_AUCTION_STAGE_START, MIN_LEN_AUCTION_STAGE_START);
        _assertBodyVersion(_msg);
        seriesId = uint32(bytes4(_msg[2:6]));
        schedule = IIntexAuction.AuctionSchedule({
            commitEnd: uint32(bytes4(_msg[6:10])),
            revealEnd: uint32(bytes4(_msg[10:14])),
            issuanceEnd: uint32(bytes4(_msg[14:18]))
        });
        params = IIntexAuction.AuctionParams({
            issuanceCurrency: uint16(bytes2(_msg[18:20])),
            referenceCurrency: uint16(bytes2(_msg[20:22])),
            promisLoadMinor: uint128(bytes16(_msg[22:38])),
            callTrigger: IIntexAuction.IntexCallTrigger({
                windowDays: uint16(bytes2(_msg[70:72])),
                thresholdDays: uint16(bytes2(_msg[72:74])),
                intexCallPeriod: uint32(bytes4(_msg[66:70]))
            }),
            minIntexBidRate: uint32(bytes4(_msg[38:42])),
            minIntexBidQuantity: uint16(bytes2(_msg[74:76])),
            entryPriceMinor: uint64(bytes8(_msg[42:50])),
            floorPriceMinor: uint64(bytes8(_msg[50:58])),
            callPriceMinor: uint64(bytes8(_msg[58:66]))
        });
    }

    /// @notice Encodes ISSUANCE_INSTRUCTIONS message.
    /// @dev Reverts `PayloadArrayTooLong` if `_payload.recipients` exceeds `MAX_PAYLOAD_ARRAY_LEN`.
    /// @param _payload The issuance instructions payload to encode.
    /// @return The wire-encoded ISSUANCE_INSTRUCTIONS message.
    function encodeIssuanceInstructions(IssuanceInstructionsPayload memory _payload)
        internal
        pure
        returns (bytes memory)
    {
        if (_payload.recipients.length != _payload.quantities.length) {
            revert IssuanceArrayLengthMismatch(_payload.recipients.length, _payload.quantities.length);
        }
        requireMaxArrayLen(_payload.recipients.length, MAX_PAYLOAD_ARRAY_LEN);
        return abi.encodePacked(BODY_VERSION_V1, MSG_ISSUANCE_INSTRUCTIONS, abi.encode(_payload));
    }

    /// @notice Encodes REFUND_INSTRUCTIONS message.
    /// @dev Reverts `PayloadArrayTooLong` if `_bidders` exceeds `MAX_PAYLOAD_ARRAY_LEN`.
    /// @param _seriesId The auction series identifier.
    /// @param _bidders The bidder addresses (parallel with `_refundedAmounts` and `_paidAmounts`).
    /// @param _refundedAmounts The amount refunded to each bidder.
    /// @param _paidAmounts The amount paid by each bidder.
    /// @return The wire-encoded REFUND_INSTRUCTIONS message.
    function encodeRefundInstructions(
        uint32 _seriesId,
        address[] memory _bidders,
        uint64[] memory _refundedAmounts,
        uint64[] memory _paidAmounts
    ) internal pure returns (bytes memory) {
        if (_bidders.length != _refundedAmounts.length || _bidders.length != _paidAmounts.length) {
            revert RefundArrayLengthMismatch(_bidders.length, _refundedAmounts.length, _paidAmounts.length);
        }
        requireMaxArrayLen(_bidders.length, MAX_PAYLOAD_ARRAY_LEN);
        return abi.encodePacked(
            BODY_VERSION_V1, MSG_REFUND_INSTRUCTIONS, abi.encode(_seriesId, _bidders, _refundedAmounts, _paidAmounts)
        );
    }

    /// @notice Encodes MARK_CALLED message.
    /// @dev The settlement deadline is derived locally on the destination chain
    ///      from the series `intexCallPeriod` and the moment markCalled is applied.
    ///      encodePacked layout (6 bytes): [bodyVersion(1)][msgType(1)][seriesId(4)]
    /// @param _seriesId The auction series identifier.
    /// @return The wire-encoded MARK_CALLED message.
    function encodeMarkCalled(uint32 _seriesId) internal pure returns (bytes memory) {
        return abi.encodePacked(BODY_VERSION_V1, MSG_MARK_CALLED, _seriesId);
    }

    /// @notice Encodes MARK_QUALIFIED message.
    /// @dev encodePacked layout (6 bytes): [bodyVersion(1)][msgType(1)][seriesId(4)]
    /// @param _seriesId The auction series identifier.
    /// @return The wire-encoded MARK_QUALIFIED message.
    function encodeMarkQualified(uint32 _seriesId) internal pure returns (bytes memory) {
        return abi.encodePacked(BODY_VERSION_V1, MSG_MARK_QUALIFIED, _seriesId);
    }

    // --- Decoding ---

    /// @notice Returns the body version byte (offset 0).
    /// @param _msg The wire-encoded bridge message.
    /// @return The body version byte at offset 0.
    function bodyVersion(bytes calldata _msg) internal pure returns (uint8) {
        return uint8(_msg[0]);
    }

    /// @notice Returns the message type byte (offset 1).
    /// @param _msg The wire-encoded bridge message.
    /// @return The message-type byte at offset 1.
    function msgType(bytes calldata _msg) internal pure returns (uint8) {
        return uint8(_msg[1]);
    }

    /// @dev Validates `_msg[0] == BODY_VERSION_V1`; reverts `UnsupportedBodyVersion` otherwise.
    function _assertBodyVersion(bytes calldata _msg) private pure {
        uint8 v = uint8(_msg[0]);
        if (v != BODY_VERSION_V1) revert UnsupportedBodyVersion(v);
    }

    /// @dev Asserts a fixed-width payload is *exactly* `_expected` bytes.
    ///      Closes (truncated/empty payloads would index past the slice and `Panic`)
    ///      and (over-long payloads were silently truncated to their valid prefix)
    ///      in one guard. Called *before* `_assertBodyVersion` so an empty payload yields a
    ///      typed `InvalidPayloadLength` rather than an out-of-bounds panic on `_msg[0]`.
    function _assertExactLength(bytes calldata _msg, uint8 _msgType, uint16 _expected) private pure {
        if (_msg.length != _expected) revert InvalidPayloadLength(_msgType, _msg.length, _expected);
    }

    /// @notice Decodes BIDS_BATCH message.
    /// @dev Reverts `UnsupportedBodyVersion` on a stale version byte,
    ///      `BidsArrayLengthMismatch` if the four parallel arrays differ in length, and
    ///      `BidsBatchTooLarge` if the batch exceeds `MAX_BIDS_BATCH`.
    /// @param _msg The wire-encoded BIDS_BATCH message.
    /// @return seriesId The auction series identifier.
    /// @return srcEid The LayerZero source endpoint id the bids originated from.
    /// @return isLast Whether this is the final chunk of the bid set.
    /// @return relayGeneration The flush generation stamp the receiver uses to replace re-flushed sets.
    /// @return bidderAddresses The bidder addresses (parallel with the other three arrays).
    /// @return intexQuantities The intex quantities per bidder.
    /// @return intexBidRates The intex bid rates per bidder (`1e6` fixed-point, % of strike).
    /// @return timestamps The bid timestamps per bidder.
    function decodeBidsBatch(bytes calldata _msg)
        internal
        pure
        returns (
            uint32 seriesId,
            uint32 srcEid,
            bool isLast,
            uint32 relayGeneration,
            address[] memory bidderAddresses,
            uint16[] memory intexQuantities,
            uint32[] memory intexBidRates,
            uint32[] memory timestamps
        )
    {
        // Match the fixed-length decoders' typed empty-payload revert (mirrors readHeader and the
        // _assertExactLength helpers) so the symmetric path produces InvalidPayloadLength rather
        // than an out-of-bounds Panic(0x32) on `_msg[0]`.
        if (_msg.length < HEADER_LEN) revert InvalidPayloadLength(MSG_BIDS_BATCH, _msg.length, HEADER_LEN);
        _assertBodyVersion(_msg);
        (seriesId, srcEid, isLast, relayGeneration, bidderAddresses, intexQuantities, intexBidRates, timestamps) =
            abi.decode(_msg[2:], (uint32, uint32, bool, uint32, address[], uint16[], uint32[], uint32[]));
        // The four arrays are indexed in lockstep downstream; unequal lengths would index out of
        // bounds and panic inside the ordered lane. Reject with a typed error instead.
        if (
            bidderAddresses.length != intexQuantities.length || bidderAddresses.length != intexBidRates.length
                || bidderAddresses.length != timestamps.length
        ) {
            revert BidsArrayLengthMismatch(
                bidderAddresses.length, intexQuantities.length, intexBidRates.length, timestamps.length
            );
        }
        // Cap the batch so the receiver's crosschainMint/storage loop cannot exceed the inbound gas limit.
        if (bidderAddresses.length > MAX_BIDS_BATCH) revert BidsBatchTooLarge(bidderAddresses.length, MAX_BIDS_BATCH);
    }

    /// @notice Decodes AUCTION_STAGE_START message.
    /// @dev encodePacked layout (76 bytes), field order mirrors the Outbe `sol_ext` struct:
    ///      [bodyVersion(1)][msgType(1)][seriesId(4)][commitEnd(4)][revealEnd(4)][issuanceEnd(4)]
    ///      [issuanceCurrency(2)][referenceCurrency(2)][promisLoadMinor(16)][minIntexBidRate(4)][entryPrice(8)][floorPriceMinor(8)]
    ///      [callPriceMinor(8)][intexCallPeriod(4)][callWindowDays(2)][callThresholdDays(2)][minIntexBidQuantity(2)]
    ///      Reverts `InvalidPayloadLength` unless the payload is exactly 76 bytes, then
    ///      `UnsupportedBodyVersion` on a stale version byte.
    /// @param _msg The wire-encoded AUCTION_STAGE_START message.
    /// @return seriesId The auction series identifier.
    /// @return commitEnd The commit-stage end timestamp.
    /// @return revealEnd The reveal-stage end timestamp.
    /// @return issuanceEnd The issuance-stage end timestamp.
    /// @return issuanceCurrency The issuance currency (ISO numeric).
    /// @return referenceCurrency The reference currency (ISO numeric).
    /// @return promisLoadMinor The Promis load (minor units) for the series.
    /// @return minIntexBidRate The minimum acceptable intex bid rate (`1e6` fixed-point).
    /// @return entryPrice The per-unit entry price (reference ccy); strike derives from it.
    /// @return floorPriceMinor The floor price (minor units).
    /// @return callPriceMinor The call price (minor units).
    /// @return intexCallPeriod The Called→deadline window in seconds (0 = default).
    /// @return callWindowDays The call-trigger observation window in days.
    /// @return callThresholdDays The call-trigger threshold in days.
    /// @return minIntexBidQuantity The minimum acceptable intex bid quantity.
    function decodeAuctionStageStart(bytes calldata _msg)
        internal
        pure
        returns (
            uint32 seriesId,
            uint32 commitEnd,
            uint32 revealEnd,
            uint32 issuanceEnd,
            uint16 issuanceCurrency,
            uint16 referenceCurrency,
            uint128 promisLoadMinor,
            uint32 minIntexBidRate,
            uint64 entryPrice,
            uint64 floorPriceMinor,
            uint64 callPriceMinor,
            uint32 intexCallPeriod,
            uint16 callWindowDays,
            uint16 callThresholdDays,
            uint16 minIntexBidQuantity
        )
    {
        _assertExactLength(_msg, MSG_AUCTION_STAGE_START, MIN_LEN_AUCTION_STAGE_START);
        _assertBodyVersion(_msg);
        seriesId = uint32(bytes4(_msg[2:6]));
        commitEnd = uint32(bytes4(_msg[6:10]));
        revealEnd = uint32(bytes4(_msg[10:14]));
        issuanceEnd = uint32(bytes4(_msg[14:18]));
        issuanceCurrency = uint16(bytes2(_msg[18:20]));
        referenceCurrency = uint16(bytes2(_msg[20:22]));
        promisLoadMinor = uint128(bytes16(_msg[22:38]));
        minIntexBidRate = uint32(bytes4(_msg[38:42]));
        entryPrice = uint64(bytes8(_msg[42:50]));
        floorPriceMinor = uint64(bytes8(_msg[50:58]));
        callPriceMinor = uint64(bytes8(_msg[58:66]));
        intexCallPeriod = uint32(bytes4(_msg[66:70]));
        callWindowDays = uint16(bytes2(_msg[70:72]));
        callThresholdDays = uint16(bytes2(_msg[72:74]));
        minIntexBidQuantity = uint16(bytes2(_msg[74:76]));
    }

    /// @notice Decodes AUCTION_STAGE_REVEAL message.
    /// @dev encodePacked layout (7 bytes): [bodyVersion(1)][msgType(1)][seriesId(4)][isGreenDay(1)]
    ///      Reverts `InvalidPayloadLength` unless exactly 7 bytes, `UnsupportedBodyVersion` on a
    ///      stale version byte, and `InvalidGreenDayFlag` if the flag byte is neither 0 nor 1.
    /// @param _msg The wire-encoded AUCTION_STAGE_REVEAL message.
    /// @return seriesId The auction series identifier.
    /// @return isGreenDay The decoded green-day flag.
    function decodeAuctionStageReveal(bytes calldata _msg) internal pure returns (uint32 seriesId, bool isGreenDay) {
        _assertExactLength(_msg, MSG_AUCTION_STAGE_REVEAL, MIN_LEN_AUCTION_STAGE_REVEAL);
        _assertBodyVersion(_msg);
        seriesId = uint32(bytes4(_msg[2:6]));
        uint8 flag = uint8(_msg[6]);
        if (flag > 1) revert InvalidGreenDayFlag(flag);
        isGreenDay = flag == 1;
    }

    /// @notice Decodes AUCTION_STAGE_CLEARING message.
    /// @dev encodePacked layout (6 bytes): [bodyVersion(1)][msgType(1)][seriesId(4)]
    ///      Reverts `InvalidPayloadLength` unless exactly 6 bytes, then `UnsupportedBodyVersion`.
    /// @param _msg The wire-encoded AUCTION_STAGE_CLEARING message.
    /// @return seriesId The auction series identifier.
    function decodeAuctionStageClearing(bytes calldata _msg) internal pure returns (uint32 seriesId) {
        _assertExactLength(_msg, MSG_AUCTION_STAGE_CLEARING, MIN_LEN_AUCTION_STAGE_CLEARING);
        _assertBodyVersion(_msg);
        seriesId = uint32(bytes4(_msg[2:6]));
    }

    /// @notice Decodes AUCTION_RESULT message.
    /// @dev encodePacked layout (22 bytes):
    ///      [bodyVersion(1)][msgType(1)][seriesId(4)][issuedIntexCount(4)][auctionClearingRate(8)][wonBidsCount(4)]
    ///      Reverts `InvalidPayloadLength` unless exactly 22 bytes, then `UnsupportedBodyVersion`.
    /// @param _msg The wire-encoded AUCTION_RESULT message.
    /// @return seriesId The auction series identifier.
    /// @return issuedIntexCount The number of intex issued by the cleared auction.
    /// @return auctionClearingRate The uniform auction clearing rate (`1e6` fixed-point).
    /// @return wonBidsCount The number of winning bids.
    function decodeAuctionResult(bytes calldata _msg)
        internal
        pure
        returns (uint32 seriesId, uint32 issuedIntexCount, uint64 auctionClearingRate, uint32 wonBidsCount)
    {
        _assertExactLength(_msg, MSG_AUCTION_RESULT, MIN_LEN_AUCTION_RESULT);
        _assertBodyVersion(_msg);
        seriesId = uint32(bytes4(_msg[2:6]));
        issuedIntexCount = uint32(bytes4(_msg[6:10]));
        auctionClearingRate = uint64(bytes8(_msg[10:18]));
        wonBidsCount = uint32(bytes4(_msg[18:22]));
    }

    /// @notice Decodes ISSUANCE_INSTRUCTIONS message.
    /// @dev Reverts `UnsupportedBodyVersion` on a stale version byte,
    ///      `IssuanceArrayLengthMismatch` if `recipients` and `quantities` differ in length, and
    ///      `IssuanceBatchTooLarge` if `recipients` exceeds `MAX_PAYLOAD_ARRAY_LEN`.
    /// @param _msg The wire-encoded ISSUANCE_INSTRUCTIONS message.
    /// @return payload The decoded issuance instructions payload.
    function decodeIssuanceInstructions(bytes calldata _msg)
        external
        pure
        returns (IssuanceInstructionsPayload memory payload)
    {
        if (_msg.length < HEADER_LEN) {
            revert InvalidPayloadLength(MSG_ISSUANCE_INSTRUCTIONS, _msg.length, HEADER_LEN);
        }
        _assertBodyVersion(_msg);
        // Decode in a dedicated frame so the struct ABI-decoder's locals don't share this
        // function's stack — keeps the 14-field payload within bounds under via_ir.
        payload = _decodeIssuancePayload(_msg[2:]);
        // `recipients` and `quantities` are indexed in lockstep when minting; unequal lengths would
        // index out of bounds and panic inside the ordered lane. Reject with a typed error instead.
        if (payload.recipients.length != payload.quantities.length) {
            revert IssuanceArrayLengthMismatch(payload.recipients.length, payload.quantities.length);
        }
        // Cap the recipient count so the receiver's mint loop cannot exceed the inbound gas limit.
        if (payload.recipients.length > MAX_PAYLOAD_ARRAY_LEN) {
            revert IssuanceBatchTooLarge(payload.recipients.length, MAX_PAYLOAD_ARRAY_LEN);
        }
    }

    /// @dev Isolated frame for the `IssuanceInstructionsPayload` ABI decode (via_ir stack relief).
    function _decodeIssuancePayload(bytes calldata _body) private pure returns (IssuanceInstructionsPayload memory) {
        return abi.decode(_body, (IssuanceInstructionsPayload));
    }

    /// @notice Decodes REFUND_INSTRUCTIONS message.
    /// @dev Reverts `UnsupportedBodyVersion` on a stale version byte and
    ///      `RefundArrayLengthMismatch` if the three parallel arrays differ in length.
    /// @param _msg The wire-encoded REFUND_INSTRUCTIONS message.
    /// @return seriesId The auction series identifier.
    /// @return bidders The bidder addresses (parallel with `refundedAmounts` and `paidAmounts`).
    /// @return refundedAmounts The amount refunded to each bidder.
    /// @return paidAmounts The amount paid by each bidder.
    function decodeRefundInstructions(bytes calldata _msg)
        external
        pure
        returns (
            uint32 seriesId,
            address[] memory bidders,
            uint64[] memory refundedAmounts,
            uint64[] memory paidAmounts
        )
    {
        if (_msg.length < HEADER_LEN) {
            revert InvalidPayloadLength(MSG_REFUND_INSTRUCTIONS, _msg.length, HEADER_LEN);
        }
        _assertBodyVersion(_msg);
        (seriesId, bidders, refundedAmounts, paidAmounts) =
            abi.decode(_msg[2:], (uint32, address[], uint64[], uint64[]));
        // The three arrays are indexed in lockstep downstream; unequal lengths would index
        // out of bounds and panic inside the ordered lane. Reject with a typed error instead.
        if (bidders.length != refundedAmounts.length || bidders.length != paidAmounts.length) {
            revert RefundArrayLengthMismatch(bidders.length, refundedAmounts.length, paidAmounts.length);
        }
        // Symmetric with the BIDS and ISSUANCE inbound caps: a peer compromise or a future encoder
        // change could deliver an over-cap REFUND that exhausts the receiver's gas in the
        // per-bidder loop. The drop-don't-block handler catches this typed revert.
        if (bidders.length > MAX_PAYLOAD_ARRAY_LEN) {
            revert RefundBatchTooLarge(bidders.length, MAX_PAYLOAD_ARRAY_LEN);
        }
    }

    /// @notice Decodes MARK_CALLED message.
    /// @dev encodePacked layout (6 bytes): [bodyVersion(1)][msgType(1)][seriesId(4)]
    ///      Reverts `InvalidPayloadLength` unless exactly 6 bytes, then `UnsupportedBodyVersion`.
    /// @param _msg The wire-encoded MARK_CALLED message.
    /// @return seriesId The auction series identifier.
    function decodeMarkCalled(bytes calldata _msg) internal pure returns (uint32 seriesId) {
        _assertExactLength(_msg, MSG_MARK_CALLED, MIN_LEN_MARK_CALLED);
        _assertBodyVersion(_msg);
        seriesId = uint32(bytes4(_msg[2:6]));
    }

    /// @notice Decodes MARK_QUALIFIED message.
    /// @dev encodePacked layout (6 bytes): [bodyVersion(1)][msgType(1)][seriesId(4)]
    ///      Reverts `InvalidPayloadLength` unless exactly 6 bytes, then `UnsupportedBodyVersion`.
    /// @param _msg The wire-encoded MARK_QUALIFIED message.
    /// @return seriesId The auction series identifier.
    function decodeMarkQualified(bytes calldata _msg) internal pure returns (uint32 seriesId) {
        _assertExactLength(_msg, MSG_MARK_QUALIFIED, MIN_LEN_MARK_QUALIFIED);
        _assertBodyVersion(_msg);
        seriesId = uint32(bytes4(_msg[2:6]));
    }

    // --- Validation helpers ---

    /// @notice Returns the minimum encoded length for the given `msgType`, or 0 if not recognised.
    /// @dev Caller is expected to validate `msgType ∈ allowedSet` separately via
    ///      `UnknownMsgType` — a 0 return here means "unknown to the codec".
    /// @param _msgType The message-type byte to look up.
    /// @return The minimum encoded length for `_msgType`, or 0 if unknown to the codec.
    function minLengthFor(uint8 _msgType) internal pure returns (uint16) {
        if (_msgType == MSG_AUCTION_STAGE_START) return MIN_LEN_AUCTION_STAGE_START;
        if (_msgType == MSG_AUCTION_STAGE_REVEAL) return MIN_LEN_AUCTION_STAGE_REVEAL;
        if (_msgType == MSG_AUCTION_STAGE_CLEARING) return MIN_LEN_AUCTION_STAGE_CLEARING;
        if (_msgType == MSG_AUCTION_RESULT) return MIN_LEN_AUCTION_RESULT;
        if (_msgType == MSG_MARK_CALLED) return MIN_LEN_MARK_CALLED;
        if (_msgType == MSG_MARK_QUALIFIED) return MIN_LEN_MARK_QUALIFIED;
        if (_msgType == MSG_BIDS_BATCH) return MIN_LEN_BIDS_BATCH;
        if (_msgType == MSG_REFUND_INSTRUCTIONS) return MIN_LEN_REFUND_INSTRUCTIONS;
        if (_msgType == MSG_ISSUANCE_INSTRUCTIONS) return MIN_LEN_ISSUANCE_INSTRUCTIONS;
        return 0;
    }

    /// @notice Reverts `InvalidPayloadLength` if `_msg.length < minLengthFor(msgType)`.
    /// @dev Must be called *after* msgType validation; assumes `_msg.length >= 2`.
    /// @param _msg The wire-encoded bridge message.
    /// @param _msgType The message-type byte governing the minimum length.
    function assertMinLength(bytes calldata _msg, uint8 _msgType) internal pure {
        uint16 minLen = minLengthFor(_msgType);
        if (_msg.length < minLen) revert InvalidPayloadLength(_msgType, _msg.length, minLen);
    }

    /// @notice Validates the 2-byte header and returns the `msgType` byte.
    /// @dev Reverts `InvalidPayloadLength(0, got, HEADER_LEN)` if shorter than the header.
    ///      Does NOT validate the `msgType` is in any particular handler's accepted set —
    ///      that check is the caller's responsibility (revert `UnknownMsgType` on mismatch).
    /// @param _msg The wire-encoded bridge message.
    /// @return _msgType The message-type byte read from offset 1.
    function readHeader(bytes calldata _msg) internal pure returns (uint8 _msgType) {
        if (_msg.length < HEADER_LEN) revert InvalidPayloadLength(0, _msg.length, HEADER_LEN);
        _msgType = uint8(_msg[1]);
    }

    /// @notice Reverts `MalformedAddress(got)` if `_value` cannot be losslessly cast to `address`.
    /// @dev The Solidity address ABI uses the low 20 bytes; the high 12 bytes must be zero.
    /// @param _value The `bytes32` slot interpreted as an address.
    function assertAddress(bytes32 _value) internal pure {
        if (uint256(_value) >> 160 != 0) revert MalformedAddress(_value);
    }
}
