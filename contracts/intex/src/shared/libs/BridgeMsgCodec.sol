// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IIntexAuction} from "../../target/interfaces/IIntexAuction.sol";
import {IOriginRouter} from "../../origin/interfaces/IOriginRouter.sol";

/// @title BridgeMsgCodec
/// @author Outbe
/// @notice Library for encoding and decoding bridge messages between the target chains and Outbe.
/// @dev Auction messages (stages, bids, result, refunds) are keyed by `emissionDay`. Series
///      messages (issuance, mark) are keyed by `seriesId` and carry their day alongside it.
/// @dev Wire layout: `[bodyVersion(1)][msgType(1)][body]`. `bodyVersion` lets the format
///      evolve independently of `msgType`. Decoders reject unknown versions.
library BridgeMsgCodec {
    /// @notice Active body version emitted by every `encode*` and required by every `decode*`.
    uint8 internal constant BODY_VERSION_V1 = 1;

    // Message types: target chain -> Outbe
    uint8 internal constant MSG_BIDS_PAGE = 1;
    uint8 internal constant MSG_BIDS_TOTALS = 2;

    // Message types: Outbe -> target chain
    uint8 internal constant MSG_INTEX_AUCTION_START = 3;
    uint8 internal constant MSG_INTEX_AUCTION_CLEARING_START = 4;
    uint8 internal constant MSG_INTEX_AUCTION_RESULT = 5;
    uint8 internal constant MSG_INTEX_SERIES = 6;
    uint8 internal constant MSG_INTEX_AUCTION_WINNERS = 7;
    uint8 internal constant MSG_INTEX_SERIES_CALL_NOTICE = 8;
    /// @dev Target -> origin: the day's relay stopped with chunks left, so the origin sends another round.
    uint8 internal constant MSG_BIDS_PAUSE = 9;
    /// @dev Outbe -> target chain: the day is canceled before its result.
    uint8 internal constant MSG_INTEX_AUCTION_CANCEL = 10;

    /// @notice Upper bound on every caller-supplied cross-chain payload array
    ///         (`BIDS_BATCH`, `ISSUANCE_INSTRUCTIONS`, `REFUND_INSTRUCTIONS`).
    /// @dev One system-wide cap (unified with the bridge `MAX_BATCH_SIZE`). Derived from the binding
    ///      `maxMessageSize = 10_000` byte ceiling (bids ~128 B/item caps near 78) with
    ///      destination-gas headroom. Every `encode*` function enforces it OUTBOUND (fail-fast at
    ///      the source). The variable-length `decode*` functions re-check it INBOUND
    ///      (defence-in-depth against a peer compromise or future encoder change). The
    ///      drop-don't-block handler catches an inbound over-cap revert, so the ORDERED lane stays
    ///      live.
    uint16 internal constant MAX_PAYLOAD_ARRAY_LEN = 64;

    /// @notice Series one ISSUANCE_INSTRUCTIONS message may carry. A wider day spans several messages.
    /// @dev A series encodes to 416 bytes, so the 10_000-byte send cap fits 23. This keeps headroom.
    uint16 internal constant MAX_SERIES_PER_ISSUANCE = 16;

    /// @notice Series one MARK_CALLED message may carry. A batch is one day's series called
    ///         together.
    uint16 internal constant MAX_SERIES_PER_CALL_NOTICE = 8;

    /// @notice Chunks one day's fan-out may span. This keeps a receiver's arrival set in one word.
    uint16 internal constant MAX_PAGES = 256;

    /// @notice Numeric fixed-point scale for bid/clearing rates (`1e6` = 100%).
    uint32 internal constant SCALE_1E6 = 1_000_000;

    /// @notice Decimals of protocol amounts and COEN/ISO prices.
    uint8 internal constant PROTOCOL_DECIMALS = 6;

    // --- Minimum encoded lengths ---
    // Header is fixed at 2 bytes: [bodyVersion(1)][msgType(1)].
    uint16 internal constant HEADER_LEN = 2;

    // Fixed head of AUCTION_STAGE_START; the price rows follow it.
    uint16 internal constant MIN_LEN_INTEX_AUCTION_START = 69;
    /// @notice Bytes per reference-price row: [iso(2)][entry(8)][call(8)].
    uint16 internal constant REFERENCE_PRICE_LEN = 18;
    uint16 internal constant MIN_LEN_INTEX_AUCTION_CLEARING_START = 6;
    uint16 internal constant MIN_LEN_INTEX_AUCTION_RESULT = 34;
    // INTEX_AUCTION_CANCEL: [ver(1)][type(1)][emissionDay(4)][reason(1)]
    uint16 internal constant MIN_LEN_INTEX_AUCTION_CANCEL = 7;
    // MARK_CALLED: header + abi.encode(emissionDay, calledAt, seriesIds). One series is 5 words.
    uint16 internal constant MIN_LEN_INTEX_SERIES_CALL_NOTICE = HEADER_LEN + 160;
    // BIDS_TOTALS: [ver(1)][type(1)][emissionDay(4)][srcChainId(4)][totalPages(2)][totalBids(4)]
    uint16 internal constant MIN_LEN_BIDS_TOTALS = 16;
    // BIDS_PAUSE: [ver(1)][type(1)][emissionDay(4)][srcChainId(4)][nextPage(2)][totalPages(2)]
    uint16 internal constant MIN_LEN_BIDS_PAUSE = 14;

    // abi.encode payloads have variable length. The minimum corresponds to all
    // dynamic arrays being empty:
    //   BIDS_BATCH(uint32, uint32, uint16, uint16, address[], uint256[]):
    //     4 static head words + 2 dynamic head offsets + 2 empty length words = 8x32 = 256
    //   REFUND_INSTRUCTIONS(uint32, uint16, uint16, uint64, uint128, address[], uint16, uint16):
    //     7 static head words + 1 dynamic offset + 1 empty length word = 9x32 = 288
    //   ISSUANCE_INSTRUCTIONS(3 static head words + dynamic array of a 13-field static struct):
    //     3 head words + array offset(32) + array length(32) + one element(13x32) = 18x32 = 576
    uint16 internal constant MIN_LEN_BIDS_PAGE = HEADER_LEN + 256;
    uint16 internal constant MIN_LEN_INTEX_AUCTION_WINNERS = HEADER_LEN + 288;
    uint16 internal constant MIN_LEN_INTEX_SERIES = HEADER_LEN + 576;

    /// @notice Per-message cap on inbound BIDS_BATCH entries. Bounds the crosschainMint/storage loop the
    ///         receiver runs so one oversized batch cannot exceed the inbound gas limit and stall
    ///         the ordered lane. The sender chunks larger bid sets into multiple batches.
    /// @dev Unified with the outbound `MAX_PAYLOAD_ARRAY_LEN` so inbound and outbound agree on one
    ///      number. The earlier value of 256 was the original ticket figure and is physically
    ///      unsendable. A bids batch is ~128 B/item, so 256 items is ~32 KB. That is over 3x
    ///      ERC-7786's send-side `maxMessageSize = 10_000` byte cap (an over-cap send reverts on
    ///      the source chain). The real byte ceiling lands near 78 items. 64 sits under it with gas
    ///      headroom, and the outbound encoder already rejects anything larger.
    uint256 internal constant MAX_BIDS_PAGE = MAX_PAYLOAD_ARRAY_LEN;

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
    /// @dev The Solidity address ABI uses the low 20 bytes. The high 12 bytes must be zero.
    /// @param got The malformed `bytes32` slot.
    error MalformedAddress(bytes32 got);

    /// @notice REFUND_INSTRUCTIONS names a partially filled winner outside its own winners.
    /// @param partialIndex Index the partial fill points at.
    /// @param winners Winners the chunk carries.
    error InvalidReturnPartial(uint16 partialIndex, uint256 winners);

    /// @notice Inbound BIDS_BATCH exceeds the per-message entry cap.
    /// @param count Decoded number of bidders.
    /// @param max Maximum permitted entries per batch.
    error IntexAuctionBidsPageTooLarge(uint256 count, uint256 max);

    /// @notice BIDS_BATCH parallel arrays decoded to unequal lengths.
    /// @param bidders Length of the bidder-addresses array.
    /// @param packedBids Length of the packed-bid array.
    error IntexAuctionBidsArrayLengthMismatch(uint256 bidders, uint256 packedBids);

    /// @notice A mark message carries no series. There is nothing for the target to apply.
    error EmptyCallNoticePage();

    /// @notice A mark message exceeds the per-message series cap.
    /// @param count Number of series in the batch.
    /// @param max Maximum permitted series per mark message.
    error CallNoticePageTooLarge(uint256 count, uint256 max);

    /// @notice Inbound REFUND_INSTRUCTIONS exceeds the per-message bidder cap.
    /// @param count Decoded number of bidders.
    /// @param max Maximum permitted bidders per message.
    error IntexAuctionWinnersPageTooLarge(uint256 count, uint256 max);
    /// @notice A live day was encoded without a single reference price to bid against.
    error MissingReferencePrices();
    /// @notice An ISSUANCE_INSTRUCTIONS message carried no series at all.
    error EmptyIntexSeriesPage();
    /// @notice An ISSUANCE_INSTRUCTIONS message carried more series than one may hold.
    error IntexSeriesPageTooLarge(uint256 count, uint256 max);
    /// @notice The refund chunk header is inconsistent: no chunks claimed, more than
    ///         `MAX_PAGES`, or an index outside the claimed count.
    error InvalidIntexAuctionWinnersPage(uint16 pageIndex, uint16 totalPages);
    /// @notice The issuance chunk header is inconsistent: no chunks claimed, more than
    ///         `MAX_PAGES`, or an index outside the claimed count.
    error InvalidIntexSeriesPage(uint16 pageIndex, uint16 totalPages);
    /// @notice A series in an ISSUANCE_INSTRUCTIONS message belongs to a different day than the
    ///         message header.
    error IssuanceDayMismatch(bytes14 seriesId, uint32 seriesDay, uint32 messageDay);

    /// @notice An outbound payload array exceeds `MAX_PAYLOAD_ARRAY_LEN`.
    /// @dev Fail-fast on the source chain so the relayer learns before any bridge fee is burned.
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

    /// @notice Encodes a BIDS_TOTALS marker: source chain `_srcChainId` has sent all `_totalPages` batches
    ///         of its relay for `_emissionDay`.
    /// @dev Fixed-length encodePacked. `_totalBids` is an integrity check: the receiver requires it to equal the
    ///      sum of the arrived batch sizes before it treats the chain as complete. Redundant with the per-batch
    ///      `totalPages` field, kept as the explicit completeness marker + a cross-check.
    /// @return The wire-encoded BIDS_TOTALS message.
    function encodeBidsTotals(uint32 _emissionDay, uint32 _srcChainId, uint16 _totalPages, uint32 _totalBids)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodePacked(BODY_VERSION_V1, MSG_BIDS_TOTALS, _emissionDay, _srcChainId, _totalPages, _totalBids);
    }

    /// @notice Encodes BIDS_BATCH message.
    /// @dev A bid set larger than `MAX_PAYLOAD_ARRAY_LEN` is relayed as multiple batches. The
    ///      receiver collects all `_totalPages` (in any order) before finalizing. Reverts
    ///      `PayloadArrayTooLong` if `_bidders` exceeds `MAX_PAYLOAD_ARRAY_LEN`.
    /// @param _emissionDay The emission day (yyyymmdd).
    /// @param _srcChainId The source chainId the bids originated from.
    /// @param _pageIndex Index of this batch within the relay (0-based).
    /// @param _totalPages Total number of batches in the relay (the receiver waits for all of them).
    /// @param _bidders The bidder addresses (parallel with `_packedBids`).
    /// @param _packedBids One [`packBid`] word per bidder: quantity, rate, timestamp and the pair.
    /// @return The wire-encoded BIDS_BATCH message.
    function encodeBidsPage(
        uint32 _emissionDay,
        uint32 _srcChainId,
        uint16 _pageIndex,
        uint16 _totalPages,
        address[] memory _bidders,
        uint256[] memory _packedBids
    ) internal pure returns (bytes memory) {
        // The decoder rejects a parallel-array mismatch with IntexAuctionBidsArrayLengthMismatch. The encoder
        // fails fast at the source so a sender-side bug aborts before paying the bridge fee.
        if (_bidders.length != _packedBids.length) {
            revert IntexAuctionBidsArrayLengthMismatch(_bidders.length, _packedBids.length);
        }
        requireMaxArrayLen(_bidders.length, MAX_PAYLOAD_ARRAY_LEN);
        return abi.encodePacked(
            BODY_VERSION_V1,
            MSG_BIDS_PAGE,
            abi.encode(_emissionDay, _srcChainId, _pageIndex, _totalPages, _bidders, _packedBids)
        );
    }

    /// @notice Packs one bid's scalars into a single word: 64 bytes per bid rather than 128.
    /// @dev Layout, low bits up: [referenceCurrency(16)][issuanceCurrency(16)][timestamp(32)]
    ///      [bidRate(32)][quantity(16)].
    function packBid(
        uint16 _quantity,
        uint32 _bidRate,
        uint32 _timestamp,
        uint16 _issuanceCurrency,
        uint16 _referenceCurrency
    ) internal pure returns (uint256) {
        return uint256(_referenceCurrency) | (uint256(_issuanceCurrency) << 16) | (uint256(_timestamp) << 32)
            | (uint256(_bidRate) << 64) | (uint256(_quantity) << 96);
    }

    /// @notice Inverse of [`packBid`].
    function unpackBid(uint256 _packed)
        internal
        pure
        returns (uint16 quantity, uint32 bidRate, uint32 timestamp, uint16 issuanceCurrency, uint16 referenceCurrency)
    {
        referenceCurrency = uint16(_packed);
        issuanceCurrency = uint16(_packed >> 16);
        timestamp = uint32(_packed >> 32);
        bidRate = uint32(_packed >> 64);
        quantity = uint16(_packed >> 96);
    }

    /// @notice Encodes AUCTION_STAGE_START message.
    /// @dev encodePacked layout, 69 bytes of head plus 18 per priced currency:
    ///      [bodyVersion(1)][msgType(1)][emissionDay(4)][commitEnd(4)][revealEnd(4)][issuanceEnd(4)]
    ///      [promisLoadMinor(16)][minBidRate(4)][callNoticePeriod(4)][callWindow(4)]
    ///      [callThreshold(4)][minBidQuantity(2)][bidBondMinor(16)][priceCount(1)]
    ///      then [isoCode(2)][entryPrice(8)][callPrice(8)] per currency.
    /// @param _emissionDay The emission day (yyyymmdd).
    /// @param _commitEnd The commit-stage end timestamp.
    /// @param _revealEnd The reveal-stage end timestamp.
    /// @param _issuanceEnd The issuance-stage end timestamp.
    /// @param _promisLoadMinor The Promis load (minor units) for the series.
    /// @param _minBidRate The minimum acceptable intex bid rate (`1e6` fixed-point).
    /// @param _prices Entry and call price of every currency the day can clear in.
    /// @param _callNoticePeriod The Called->deadline window in seconds (0 = default).
    /// @param _callWindow The call-trigger observation window in seconds.
    /// @param _callThreshold The call-trigger threshold in seconds.
    /// @param _minBidQuantity The minimum acceptable intex bid quantity.
    /// @param _bidBondMinor The commit-entry bond (payment-token minor units). 0 disables the
    ///        bond.
    /// @return The wire-encoded AUCTION_STAGE_START message.
    function encodeIntexAuctionStart(
        uint32 _emissionDay,
        uint32 _commitEnd,
        uint32 _revealEnd,
        uint32 _issuanceEnd,
        uint128 _promisLoadMinor,
        uint32 _minBidRate,
        IOriginRouter.ReferenceCurrencyPrice[] memory _prices,
        uint32 _callNoticePeriod,
        uint32 _callWindow,
        uint32 _callThreshold,
        uint16 _minBidQuantity,
        uint128 _bidBondMinor
    ) internal pure returns (bytes memory) {
        // The row count is one byte on the wire.
        if (_prices.length > type(uint8).max) {
            revert PayloadArrayTooLong(_prices.length, type(uint8).max);
        }
        // A day must price something to bid against.
        if (_prices.length == 0) revert MissingReferencePrices();
        bytes memory rows = abi.encodePacked(uint8(_prices.length));
        for (uint256 i = 0; i < _prices.length; ++i) {
            rows = abi.encodePacked(rows, _prices[i].isoCode, _prices[i].entryPriceMinor, _prices[i].callPriceMinor);
        }
        // Split into packed halves: this many packed args in one call is too deep for the IR
        // pipeline. Concatenation of encodePacked results is byte-identical to a single call.
        return abi.encodePacked(
            abi.encodePacked(
                BODY_VERSION_V1,
                MSG_INTEX_AUCTION_START,
                _emissionDay,
                _commitEnd,
                _revealEnd,
                _issuanceEnd,
                _promisLoadMinor,
                _minBidRate
            ),
            _callNoticePeriod,
            _callWindow,
            _callThreshold,
            _minBidQuantity,
            _bidBondMinor,
            rows
        );
    }

    /// @notice Encodes INTEX_AUCTION_CANCEL message.
    /// @dev encodePacked layout (7 bytes): [bodyVersion(1)][msgType(1)][emissionDay(4)][reason(1)]
    /// @param _emissionDay The emission day (yyyymmdd).
    /// @param _reason The reason code of the cancel on Outbe.
    /// @return The wire-encoded INTEX_AUCTION_CANCEL message.
    function encodeIntexAuctionCancel(uint32 _emissionDay, uint8 _reason) internal pure returns (bytes memory) {
        return abi.encodePacked(BODY_VERSION_V1, MSG_INTEX_AUCTION_CANCEL, _emissionDay, _reason);
    }

    /// @notice Encodes INTEX_AUCTION_CLEARING_START message.
    /// @dev encodePacked layout (6 bytes): [bodyVersion(1)][msgType(1)][emissionDay(4)]
    /// @param _emissionDay The emission day (yyyymmdd).
    /// @return The wire-encoded INTEX_AUCTION_CLEARING_START message.
    function encodeIntexAuctionClearingStart(uint32 _emissionDay) internal pure returns (bytes memory) {
        return abi.encodePacked(BODY_VERSION_V1, MSG_INTEX_AUCTION_CLEARING_START, _emissionDay);
    }

    /// @notice The result of a day for one chain.
    struct IntexAuctionChainResult {
        /// @notice Intex quantity this chain's winners won.
        uint32 wonQuantity;
        /// @notice Winning bids of this chain.
        uint32 wonBidsCount;
        /// @notice Intex quantity the day issued on all chains.
        uint32 quantity;
        /// @notice Uniform clearing rate (`1e6` fixed-point).
        uint64 clearingRate;
        /// @notice Intex quantity all revealed bids asked for.
        uint64 totalDemand;
    }

    /// @notice Encodes INTEX_AUCTION_RESULT message.
    /// @dev encodePacked layout (34 bytes): [bodyVersion(1)][msgType(1)][emissionDay(4)][wonQuantity(4)]
    ///      [wonBidsCount(4)][quantity(4)][clearingRate(8)][totalDemand(8)]
    /// @param _emissionDay The emission day (yyyymmdd).
    /// @param _result The result of the day for the receiving chain.
    /// @return The wire-encoded INTEX_AUCTION_RESULT message.
    function encodeIntexAuctionResult(uint32 _emissionDay, IntexAuctionChainResult memory _result)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodePacked(
            BODY_VERSION_V1,
            MSG_INTEX_AUCTION_RESULT,
            _emissionDay,
            _result.wonQuantity,
            _result.wonBidsCount,
            _result.quantity,
            _result.clearingRate,
            _result.totalDemand
        );
    }

    /// @notice One series of the issuance instructions. It is grouped into a struct to keep the
    ///         encoder/decoder API resilient against EVM stack depth limits.
    /// @dev `issuedUnits` mirrors the auction-cleared count across all chains.
    struct IntexSeriesPayload {
        bytes14 seriesId;
        /// @notice Emission day the series was derived from. It is carried so the destination
        ///         records real provenance.
        uint32 emissionDay;
        /// @notice When the origin created the series, so every chain dates it from the same moment.
        uint64 issuedAt;
        uint32 issuedUnits;
        uint128 promisLoadMinor;
        uint64 entryPriceMinor;
        /// @notice Duration in seconds between Called and the settlement deadline. 0 uses default.
        uint32 callNoticePeriod;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint32 callWindow;
        uint32 callThreshold;
        uint64 callPriceMinor;
        uint32 transitMargin;
    }

    /// @notice Decode AUCTION_STAGE_START straight into the auction schedule + params structs.
    ///         Kept `external` so the struct construction lives in the linked library, off the
    ///         router's runtime size (EIP-170). Mirrors `encodeIntexAuctionStart`'s layout:
    ///         [bodyVersion(1)][msgType(1)][emissionDay(4)][commitEnd(4)][revealEnd(4)][issuanceEnd(4)]
    ///         [promisLoadMinor(16)][minBidRate(4)][callNoticePeriod(4)][callWindow(4)]
    ///         [callThreshold(4)][minBidQuantity(2)][bidBondMinor(16)][priceCount(1)]
    /// @param _msg The wire-encoded AUCTION_STAGE_START message.
    /// @return emissionDay The emission day (yyyymmdd).
    /// @return schedule The decoded commit/reveal/issuance schedule.
    /// @return params The decoded auction params.
    function decodeAuctionParams(bytes calldata _msg)
        external
        pure
        returns (
            uint32 emissionDay,
            IIntexAuction.AuctionSchedule memory schedule,
            IIntexAuction.AuctionParams memory params
        )
    {
        _assertMinLength(_msg, MSG_INTEX_AUCTION_START, MIN_LEN_INTEX_AUCTION_START);
        _assertBodyVersion(_msg);
        emissionDay = uint32(bytes4(_msg[2:6]));
        schedule = IIntexAuction.AuctionSchedule({
            commitEnd: uint32(bytes4(_msg[6:10])),
            revealEnd: uint32(bytes4(_msg[10:14])),
            issuanceEnd: uint32(bytes4(_msg[14:18]))
        });
        params = IIntexAuction.AuctionParams({
            promisLoadMinor: uint128(bytes16(_msg[18:34])),
            callTrigger: IIntexAuction.IntexCallTrigger({
                callWindow: uint32(bytes4(_msg[42:46])),
                callThreshold: uint32(bytes4(_msg[46:50])),
                callNoticePeriod: uint32(bytes4(_msg[38:42]))
            }),
            minBidRate: uint32(bytes4(_msg[34:38])),
            minBidQuantity: uint16(bytes2(_msg[50:52])),
            prices: new IIntexAuction.ReferenceCurrencyPrice[](0),
            bidBondMinor: uint128(bytes16(_msg[52:68]))
        });

        params.prices = _referencePrices(_msg);
    }

    /// @notice Every priced row the message carries.
    function _referencePrices(bytes calldata _msg)
        private
        pure
        returns (IIntexAuction.ReferenceCurrencyPrice[] memory rows)
    {
        uint256 count = uint8(_msg[68]);
        if (_msg.length != MIN_LEN_INTEX_AUCTION_START + count * REFERENCE_PRICE_LEN) {
            revert InvalidPayloadLength(MSG_INTEX_AUCTION_START, _msg.length, MIN_LEN_INTEX_AUCTION_START);
        }
        rows = new IIntexAuction.ReferenceCurrencyPrice[](count);
        for (uint256 i = 0; i < count; ++i) {
            uint256 at = MIN_LEN_INTEX_AUCTION_START + i * REFERENCE_PRICE_LEN;
            rows[i] = IIntexAuction.ReferenceCurrencyPrice({
                isoCode: uint16(bytes2(_msg[at:at + 2])),
                entryPriceMinor: uint64(bytes8(_msg[at + 2:at + 10])),
                callPriceMinor: uint64(bytes8(_msg[at + 10:at + 18]))
            });
        }
    }

    /// @notice Encodes one chunk of the ISSUANCE_INSTRUCTIONS a chain receives from one day.
    /// @dev Capped at `MAX_SERIES_PER_ISSUANCE` series. The sender splits a larger set into
    ///      `_totalPages` numbered messages.
    /// @param _emissionDay The emission day (yyyymmdd) every series in the message belongs to.
    /// @param _pageIndex Position of this chunk in the chain-day's run of issuance messages.
    /// @param _totalPages How many chunks the chain-day's issuance spans.
    /// @param _series The per-series issuance payloads carried by this message.
    /// @return The wire-encoded ISSUANCE_INSTRUCTIONS message.
    function encodeIntexSeries(
        uint32 _emissionDay,
        uint16 _pageIndex,
        uint16 _totalPages,
        IntexSeriesPayload[] memory _series
    ) internal pure returns (bytes memory) {
        _assertIssuanceLimits(_emissionDay, _pageIndex, _totalPages, _series);
        return
            abi.encodePacked(
                BODY_VERSION_V1, MSG_INTEX_SERIES, abi.encode(_emissionDay, _pageIndex, _totalPages, _series)
            );
    }

    /// @dev Chunk header sanity, the series count and each series' day.
    function _assertIssuanceLimits(
        uint32 _emissionDay,
        uint16 _pageIndex,
        uint16 _totalPages,
        IntexSeriesPayload[] memory _series
    ) private pure {
        if (_totalPages == 0 || _totalPages > MAX_PAGES || _pageIndex >= _totalPages) {
            revert InvalidIntexSeriesPage(_pageIndex, _totalPages);
        }
        if (_series.length == 0) revert EmptyIntexSeriesPage();
        if (_series.length > MAX_SERIES_PER_ISSUANCE) {
            revert IntexSeriesPageTooLarge(_series.length, MAX_SERIES_PER_ISSUANCE);
        }
        for (uint256 i = 0; i < _series.length; i++) {
            if (_series[i].emissionDay != _emissionDay) {
                revert IssuanceDayMismatch(_series[i].seriesId, _series[i].emissionDay, _emissionDay);
            }
        }
    }

    /// @notice Encodes one chunk of a day's REFUND_INSTRUCTIONS: the chain's winners and the day's clearing
    ///         terms, from which the target works out what each of them paid.
    /// @dev Reverts `PayloadArrayTooLong` above `MAX_PAYLOAD_ARRAY_LEN`. An empty chunk closes a day with no
    ///      winners on the chain.
    /// @param _emissionDay The emission day (yyyymmdd).
    /// @param _clearingRate The day's clearing rate (`1e6` fixed-point).
    /// @param _basis The day's escrow basis (`promisLoadMinor`).
    /// @param _winners Winners on the chain in this chunk.
    /// @param _partialIndex Index of the partially filled winner. Read only when `_partialWon` is
    ///        non-zero.
    /// @param _partialWon Units the partially filled winner received. Zero when the chunk has none.
    /// @return The wire-encoded REFUND_INSTRUCTIONS message.
    function encodeIntexAuctionWinners(
        uint32 _emissionDay,
        uint16 _pageIndex,
        uint16 _totalPages,
        uint64 _clearingRate,
        uint128 _basis,
        address[] memory _winners,
        uint16 _partialIndex,
        uint16 _partialWon
    ) internal pure returns (bytes memory) {
        requireMaxArrayLen(_winners.length, MAX_PAYLOAD_ARRAY_LEN);
        if (_totalPages == 0 || _totalPages > MAX_PAGES || _pageIndex >= _totalPages) {
            revert InvalidIntexAuctionWinnersPage(_pageIndex, _totalPages);
        }
        if (_partialWon != 0 && _partialIndex >= _winners.length) {
            revert InvalidReturnPartial(_partialIndex, _winners.length);
        }
        return abi.encodePacked(
            BODY_VERSION_V1,
            MSG_INTEX_AUCTION_WINNERS,
            abi.encode(
                _emissionDay, _pageIndex, _totalPages, _clearingRate, _basis, _winners, _partialIndex, _partialWon
            )
        );
    }

    /// @notice Encodes MARK_CALLED message for one day's batch of series.
    /// @dev Layout: [bodyVersion(1)][msgType(1)] ++ abi.encode(emissionDay, calledAt, seriesIds).
    ///      The origin's stamp travels so every chain derives the same deadline.
    /// @param _emissionDay The emission day the series were derived from.
    /// @param _calledAt Unix time the origin marked the series Called.
    /// @param _seriesIds The auction series identifiers, 1..`MAX_SERIES_PER_CALL_NOTICE` of them.
    /// @return The wire-encoded MARK_CALLED message.
    function encodeIntexSeriesCallNotice(uint32 _emissionDay, uint64 _calledAt, bytes14[] memory _seriesIds)
        internal
        pure
        returns (bytes memory)
    {
        _assertCallNoticePage(_seriesIds);
        return abi.encodePacked(
            BODY_VERSION_V1, MSG_INTEX_SERIES_CALL_NOTICE, abi.encode(_emissionDay, _calledAt, _seriesIds)
        );
    }

    /// @dev A mark batch carries at least one series and at most `MAX_SERIES_PER_CALL_NOTICE`.
    function _assertCallNoticePage(bytes14[] memory _seriesIds) private pure {
        if (_seriesIds.length == 0) revert EmptyCallNoticePage();
        if (_seriesIds.length > MAX_SERIES_PER_CALL_NOTICE) {
            revert CallNoticePageTooLarge(_seriesIds.length, MAX_SERIES_PER_CALL_NOTICE);
        }
    }

    // --- Decoding ---

    /// @notice Encodes a BIDS_PAUSE report: the day's relay on `_srcChainId` has sent chunks up to
    ///         `_nextPage` of `_totalPages` and needs another round for the rest.
    /// @param _emissionDay Emission day (yyyymmdd).
    /// @param _srcChainId Chain the relay is running on.
    /// @param _nextPage First chunk still to send.
    /// @param _totalPages Chunks the day's relay spans.
    /// @return The wire-encoded BIDS_PAUSE message.
    function encodeBidsPause(uint32 _emissionDay, uint32 _srcChainId, uint16 _nextPage, uint16 _totalPages)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodePacked(BODY_VERSION_V1, MSG_BIDS_PAUSE, _emissionDay, _srcChainId, _nextPage, _totalPages);
    }

    /// @notice Decodes a BIDS_PAUSE report.
    /// @param _msg The wire-encoded BIDS_PAUSE message.
    /// @return emissionDay Emission day (yyyymmdd).
    /// @return srcChainId Chain the relay is running on.
    /// @return nextPage First chunk still to send.
    /// @return totalPages Chunks the day's relay spans.
    function decodeBidsPause(bytes calldata _msg)
        internal
        pure
        returns (uint32 emissionDay, uint32 srcChainId, uint16 nextPage, uint16 totalPages)
    {
        _assertExactLength(_msg, MSG_BIDS_PAUSE, MIN_LEN_BIDS_PAUSE);
        _assertBodyVersion(_msg);
        emissionDay = uint32(bytes4(_msg[2:6]));
        srcChainId = uint32(bytes4(_msg[6:10]));
        nextPage = uint16(bytes2(_msg[10:12]));
        totalPages = uint16(bytes2(_msg[12:14]));
    }

    /// @notice Decodes a BIDS_TOTALS marker.
    /// @param _msg The wire-encoded BIDS_TOTALS message.
    /// @return emissionDay The emission day (yyyymmdd).
    /// @return srcChainId The source chainId that finished sending its bids.
    /// @return totalPages Total batches the source chain sent for the day.
    /// @return totalBids Total bids across those batches (integrity cross-check).
    function decodeBidsTotals(bytes calldata _msg)
        internal
        pure
        returns (uint32 emissionDay, uint32 srcChainId, uint16 totalPages, uint32 totalBids)
    {
        _assertExactLength(_msg, MSG_BIDS_TOTALS, MIN_LEN_BIDS_TOTALS);
        _assertBodyVersion(_msg);
        emissionDay = uint32(bytes4(_msg[2:6]));
        srcChainId = uint32(bytes4(_msg[6:10]));
        totalPages = uint16(bytes2(_msg[10:12]));
        totalBids = uint32(bytes4(_msg[12:16]));
    }

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

    /// @dev Validates `_msg[0] == BODY_VERSION_V1`. Reverts `UnsupportedBodyVersion` otherwise.
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

    /// @dev Asserts a variable-width payload carries at least its fixed head. The
    ///      exact length is checked against the row count once that head is read.
    function _assertMinLength(bytes calldata _msg, uint8 _msgType, uint16 _minimum) private pure {
        if (_msg.length < _minimum) revert InvalidPayloadLength(_msgType, _msg.length, _minimum);
    }

    /// @notice Decodes BIDS_BATCH message.
    /// @dev Reverts:
    ///      - `UnsupportedBodyVersion` on a stale version byte.
    ///      - `IntexAuctionBidsArrayLengthMismatch` if the four parallel arrays differ in length.
    ///      - `IntexAuctionBidsPageTooLarge` if the batch exceeds `MAX_BIDS_PAGE`.
    /// @param _msg The wire-encoded BIDS_BATCH message.
    /// @return emissionDay The emission day (yyyymmdd).
    /// @return srcChainId The source chainId the bids originated from.
    /// @return pageIndex Index of this batch within the relay (0-based).
    /// @return totalPages Total number of batches in the relay (the receiver waits for all of them).
    /// @return bidders The bidder addresses (parallel with `packedBids`).
    /// @return packedBids One [`packBid`] word per bidder.
    function decodeBidsPage(bytes calldata _msg)
        internal
        pure
        returns (
            uint32 emissionDay,
            uint32 srcChainId,
            uint16 pageIndex,
            uint16 totalPages,
            address[] memory bidders,
            uint256[] memory packedBids
        )
    {
        // Match the fixed-length decoders' typed empty-payload revert (mirrors readHeader and the
        // _assertExactLength helpers) so the symmetric path produces InvalidPayloadLength rather
        // than an out-of-bounds Panic(0x32) on `_msg[0]`.
        if (_msg.length < HEADER_LEN) revert InvalidPayloadLength(MSG_BIDS_PAGE, _msg.length, HEADER_LEN);
        _assertBodyVersion(_msg);
        (emissionDay, srcChainId, pageIndex, totalPages, bidders, packedBids) =
            abi.decode(_msg[2:], (uint32, uint32, uint16, uint16, address[], uint256[]));
        // The two arrays are indexed in lockstep downstream. Unequal lengths would index out of
        // bounds and panic inside the ordered lane. Reject with a typed error instead.
        if (bidders.length != packedBids.length) {
            revert IntexAuctionBidsArrayLengthMismatch(bidders.length, packedBids.length);
        }
        // Cap the batch so the receiver's crosschainMint/storage loop cannot exceed the inbound gas
        // limit.
        if (bidders.length > MAX_BIDS_PAGE) {
            revert IntexAuctionBidsPageTooLarge(bidders.length, MAX_BIDS_PAGE);
        }
    }

    /// @notice Decodes INTEX_AUCTION_CANCEL message.
    /// @dev Reverts `InvalidPayloadLength` unless exactly 7 bytes, then `UnsupportedBodyVersion`.
    /// @param _msg The wire-encoded INTEX_AUCTION_CANCEL message.
    /// @return emissionDay The emission day (yyyymmdd).
    /// @return reason The reason code of the cancel on Outbe.
    function decodeIntexAuctionCancel(bytes calldata _msg) internal pure returns (uint32 emissionDay, uint8 reason) {
        _assertExactLength(_msg, MSG_INTEX_AUCTION_CANCEL, MIN_LEN_INTEX_AUCTION_CANCEL);
        _assertBodyVersion(_msg);
        emissionDay = uint32(bytes4(_msg[2:6]));
        reason = uint8(_msg[6]);
    }

    /// @notice Decodes INTEX_AUCTION_CLEARING_START message.
    /// @dev encodePacked layout (6 bytes): [bodyVersion(1)][msgType(1)][emissionDay(4)]
    ///      Reverts `InvalidPayloadLength` unless exactly 6 bytes, then `UnsupportedBodyVersion`.
    /// @param _msg The wire-encoded INTEX_AUCTION_CLEARING_START message.
    /// @return emissionDay The emission day (yyyymmdd).
    function decodeIntexAuctionClearingStart(bytes calldata _msg) internal pure returns (uint32 emissionDay) {
        _assertExactLength(_msg, MSG_INTEX_AUCTION_CLEARING_START, MIN_LEN_INTEX_AUCTION_CLEARING_START);
        _assertBodyVersion(_msg);
        emissionDay = uint32(bytes4(_msg[2:6]));
    }

    /// @notice Decodes INTEX_AUCTION_RESULT message.
    /// @dev Reverts `InvalidPayloadLength` unless exactly 34 bytes, then `UnsupportedBodyVersion`.
    /// @param _msg The wire-encoded INTEX_AUCTION_RESULT message.
    /// @return emissionDay The emission day (yyyymmdd).
    /// @return result The result of the day for this chain.
    function decodeIntexAuctionResult(bytes calldata _msg)
        internal
        pure
        returns (uint32 emissionDay, IntexAuctionChainResult memory result)
    {
        _assertExactLength(_msg, MSG_INTEX_AUCTION_RESULT, MIN_LEN_INTEX_AUCTION_RESULT);
        _assertBodyVersion(_msg);
        emissionDay = uint32(bytes4(_msg[2:6]));
        result = IntexAuctionChainResult({
            wonQuantity: uint32(bytes4(_msg[6:10])),
            wonBidsCount: uint32(bytes4(_msg[10:14])),
            quantity: uint32(bytes4(_msg[14:18])),
            clearingRate: uint64(bytes8(_msg[18:26])),
            totalDemand: uint64(bytes8(_msg[26:34]))
        });
    }

    /// @notice Decodes ISSUANCE_INSTRUCTIONS message.
    /// @dev Reverts:
    ///      - `UnsupportedBodyVersion` on a stale version byte.
    ///      - `InvalidIntexSeriesPage` on a bad chunk header.
    ///      - `IssuanceDayMismatch` if a series names another day.
    /// @param _msg The wire-encoded ISSUANCE_INSTRUCTIONS message.
    /// @return emissionDay The emission day (yyyymmdd) every series in the message belongs to.
    /// @return pageIndex Position of this chunk in the chain-day's run of issuance messages.
    /// @return totalPages How many chunks the chain-day's issuance spans.
    /// @return series The decoded per-series issuance payloads.
    function decodeIntexSeries(bytes calldata _msg)
        external
        pure
        returns (uint32 emissionDay, uint16 pageIndex, uint16 totalPages, IntexSeriesPayload[] memory series)
    {
        if (_msg.length < HEADER_LEN) {
            revert InvalidPayloadLength(MSG_INTEX_SERIES, _msg.length, HEADER_LEN);
        }
        _assertBodyVersion(_msg);
        // Decode in a dedicated frame so the struct ABI-decoder's locals don't share this
        // function's stack. This keeps the 13-field payload within bounds under via_ir.
        (emissionDay, pageIndex, totalPages, series) = _decodeIssuancePayload(_msg[2:]);
        // Re-checked inbound against a bad peer, as every variable-length decode here is.
        _assertIssuanceLimits(emissionDay, pageIndex, totalPages, series);
    }

    /// @dev Isolated frame for the issuance ABI decode (via_ir stack relief).
    function _decodeIssuancePayload(bytes calldata _body)
        private
        pure
        returns (uint32, uint16, uint16, IntexSeriesPayload[] memory)
    {
        return abi.decode(_body, (uint32, uint16, uint16, IntexSeriesPayload[]));
    }

    /// @notice Decodes REFUND_INSTRUCTIONS message.
    /// @dev Reverts `UnsupportedBodyVersion` on a stale version byte.
    /// @param _msg The wire-encoded REFUND_INSTRUCTIONS message.
    /// @return emissionDay The emission day (yyyymmdd).
    /// @return pageIndex Position of this chunk in the chain-day's run of refunds.
    /// @return totalPages How many chunks the chain-day's refunds span.
    /// @return clearingRate The day's clearing rate (`1e6` fixed-point).
    /// @return basis The day's escrow basis.
    /// @return winners Winners on the chain in this chunk.
    /// @return partialIndex Index of the partially filled winner. Meaningful only when `partialWon`
    ///         is non-zero.
    /// @return partialWon Units the partially filled winner received. Zero when the chunk has none.
    function decodeIntexAuctionWinners(bytes calldata _msg)
        external
        pure
        returns (
            uint32 emissionDay,
            uint16 pageIndex,
            uint16 totalPages,
            uint64 clearingRate,
            uint128 basis,
            address[] memory winners,
            uint16 partialIndex,
            uint16 partialWon
        )
    {
        if (_msg.length < HEADER_LEN) {
            revert InvalidPayloadLength(MSG_INTEX_AUCTION_WINNERS, _msg.length, HEADER_LEN);
        }
        _assertBodyVersion(_msg);
        (emissionDay, pageIndex, totalPages, clearingRate, basis, winners, partialIndex, partialWon) =
            abi.decode(_msg[2:], (uint32, uint16, uint16, uint64, uint128, address[], uint16, uint16));
        if (totalPages == 0 || totalPages > MAX_PAGES || pageIndex >= totalPages) {
            revert InvalidIntexAuctionWinnersPage(pageIndex, totalPages);
        }
        // A peer compromise or a future encoder change could deliver an over-cap REFUND that
        // exhausts the receiver's gas in the per-winner loop. The drop-don't-block handler catches
        // this typed revert.
        if (winners.length > MAX_PAYLOAD_ARRAY_LEN) {
            revert IntexAuctionWinnersPageTooLarge(winners.length, MAX_PAYLOAD_ARRAY_LEN);
        }
        if (partialWon != 0 && partialIndex >= winners.length) {
            revert InvalidReturnPartial(partialIndex, winners.length);
        }
    }

    /// @notice Decodes MARK_CALLED message.
    /// @dev Reverts `InvalidPayloadLength` below the one-series minimum, then
    ///      `UnsupportedBodyVersion`, then the batch bounds, re-checked inbound.
    /// @param _msg The wire-encoded MARK_CALLED message.
    /// @return emissionDay The emission day the series were derived from.
    /// @return calledAt Unix time the origin marked the series Called.
    /// @return seriesIds The auction series identifiers.
    function decodeIntexSeriesCallNotice(bytes calldata _msg)
        external
        pure
        returns (uint32 emissionDay, uint64 calledAt, bytes14[] memory seriesIds)
    {
        if (_msg.length < MIN_LEN_INTEX_SERIES_CALL_NOTICE) {
            revert InvalidPayloadLength(MSG_INTEX_SERIES_CALL_NOTICE, _msg.length, MIN_LEN_INTEX_SERIES_CALL_NOTICE);
        }
        _assertBodyVersion(_msg);
        (emissionDay, calledAt, seriesIds) = abi.decode(_msg[2:], (uint32, uint64, bytes14[]));
        _assertCallNoticePage(seriesIds);
    }

    // --- Validation helpers ---

    /// @notice Returns the minimum encoded length for the given `msgType`, or 0 if not recognised.
    /// @dev The caller is expected to validate `msgType in allowedSet` separately via
    ///      `UnknownMsgType`. A 0 return here means "unknown to the codec".
    /// @param _msgType The message-type byte to look up.
    /// @return The minimum encoded length for `_msgType`, or 0 if unknown to the codec.
    function minLengthFor(uint8 _msgType) internal pure returns (uint16) {
        if (_msgType == MSG_INTEX_AUCTION_START) return MIN_LEN_INTEX_AUCTION_START;
        if (_msgType == MSG_INTEX_AUCTION_CLEARING_START) return MIN_LEN_INTEX_AUCTION_CLEARING_START;
        if (_msgType == MSG_INTEX_AUCTION_RESULT) return MIN_LEN_INTEX_AUCTION_RESULT;
        if (_msgType == MSG_INTEX_AUCTION_CANCEL) return MIN_LEN_INTEX_AUCTION_CANCEL;
        if (_msgType == MSG_INTEX_SERIES_CALL_NOTICE) return MIN_LEN_INTEX_SERIES_CALL_NOTICE;
        if (_msgType == MSG_BIDS_PAGE) return MIN_LEN_BIDS_PAGE;
        if (_msgType == MSG_BIDS_TOTALS) return MIN_LEN_BIDS_TOTALS;
        if (_msgType == MSG_BIDS_PAUSE) return MIN_LEN_BIDS_PAUSE;
        if (_msgType == MSG_INTEX_AUCTION_WINNERS) return MIN_LEN_INTEX_AUCTION_WINNERS;
        if (_msgType == MSG_INTEX_SERIES) return MIN_LEN_INTEX_SERIES;
        return 0;
    }

    /// @notice Reverts `InvalidPayloadLength` if `_msg.length < minLengthFor(msgType)`.
    /// @dev Must be called *after* msgType validation. Assumes `_msg.length >= 2`.
    /// @param _msg The wire-encoded bridge message.
    /// @param _msgType The message-type byte governing the minimum length.
    function assertMinLength(bytes calldata _msg, uint8 _msgType) internal pure {
        uint16 minLen = minLengthFor(_msgType);
        if (_msg.length < minLen) revert InvalidPayloadLength(_msgType, _msg.length, minLen);
    }

    /// @notice Validates the 2-byte header and returns the `msgType` byte.
    /// @dev Reverts `InvalidPayloadLength(0, got, HEADER_LEN)` if shorter than the header.
    ///      Does NOT validate the `msgType` is in any particular handler's accepted set.
    ///      That check is the caller's responsibility (revert `UnknownMsgType` on mismatch).
    /// @param _msg The wire-encoded bridge message.
    /// @return _msgType The message-type byte read from offset 1.
    function readHeader(bytes calldata _msg) internal pure returns (uint8 _msgType) {
        if (_msg.length < HEADER_LEN) revert InvalidPayloadLength(0, _msg.length, HEADER_LEN);
        _msgType = uint8(_msg[1]);
    }

    /// @notice Reverts `MalformedAddress(got)` if `_value` cannot be losslessly cast to `address`.
    /// @dev The Solidity address ABI uses the low 20 bytes. The high 12 bytes must be zero.
    /// @param _value The `bytes32` slot interpreted as an address.
    function assertAddress(bytes32 _value) internal pure {
        if (uint256(_value) >> 160 != 0) revert MalformedAddress(_value);
    }
}
