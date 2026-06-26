// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {MessagingFee, MessagingReceipt} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";

/// @title IOriginMessenger
/// @author Outbe
/// @notice Interface for Outbe Chain bridge adapter.
/// @dev Deployed on Outbe Chain. Sends messages to BNB, receives messages from BNB.
///      Receive logic is handled internally via _lzReceive (called by LayerZero Endpoint).
///      All auction/series messages are keyed by `seriesId` (uint32).
interface IOriginMessenger {
    // --- Events ---
    /// @notice Emitted when bids batch is received from BNB.
    /// @param guid LayerZero message GUID
    /// @param srcEid LayerZero source endpoint id (`_origin.srcEid`)
    /// @param seriesId Series identifier
    /// @param bidsCount Number of bids received
    event BidsBatchReceived(bytes32 indexed guid, uint32 srcEid, uint32 indexed seriesId, uint256 bidsCount);

    /// @notice Emitted when an inbound message reverts during decode or dispatch and is dropped so the
    ///         ORDERED lane keeps advancing. The nonce has already moved; the message is not retried.
    /// @param guid LayerZero message GUID.
    /// @param srcEid LayerZero source endpoint id.
    /// @param reason Raw revert bytes from the dropped dispatch.
    event InboundMessageDropped(bytes32 indexed guid, uint32 indexed srcEid, bytes reason);

    /// @notice Emitted when auction stage message is sent to BNB.
    /// @param guid LayerZero message GUID
    /// @param seriesId Series identifier
    /// @param stageType Stage type (1=Start, 2=Reveal, 3=Clearing)
    event AuctionStageSent(bytes32 indexed guid, uint32 indexed seriesId, uint8 stageType);

    /// @notice Emitted when auction result is sent to BNB.
    /// @param guid LayerZero message GUID
    /// @param seriesId Series identifier
    /// @param issuedIntexCount Number of Intex units issued
    /// @param clearingRate Uniform clearing rate (`1e6` fixed-point)
    event AuctionResultSent(
        bytes32 indexed guid, uint32 indexed seriesId, uint32 issuedIntexCount, uint64 clearingRate
    );

    /// @notice Emitted when issuance instructions are sent to BNB.
    /// @param guid LayerZero message GUID
    /// @param seriesId Series identifier
    /// @param recipientsCount Number of recipients
    event IssuanceInstructionsSent(bytes32 indexed guid, uint32 indexed seriesId, uint256 recipientsCount);

    /// @notice Emitted when refund instructions are sent to BNB.
    /// @param guid LayerZero message GUID
    /// @param seriesId Series identifier
    /// @param instructionsCount Number of finalization instructions
    event RefundInstructionsSent(bytes32 indexed guid, uint32 indexed seriesId, uint256 instructionsCount);

    /// @notice Emitted when mark called message is sent to BNB.
    /// @param guid LayerZero message GUID
    /// @param seriesId Series identifier
    event MarkCalledSent(bytes32 indexed guid, uint32 indexed seriesId);

    /// @notice Emitted when mark qualified message is sent to BNB.
    /// @param guid LayerZero message GUID
    /// @param seriesId Series identifier
    event MarkQualifiedSent(bytes32 indexed guid, uint32 indexed seriesId);

    /// @notice Emitted when `_handleBidsBatch` auto-fires `Desis.clearAuction` for a series whose
    ///         stage is `BidsReceived`.
    /// @param seriesId Series identifier whose auction was auto-cleared.
    event ClearingAutoDispatched(uint32 indexed seriesId);

    /// @notice Emitted when the auto-fired `Desis.clearAuction` reverts; the bid intake is kept,
    ///         operators can retry clearing manually.
    /// @param seriesId Series identifier whose auto-clearing reverted.
    /// @param reason Raw revert bytes from the failed `clearAuction` call.
    event ClearingAutoDispatchFailed(uint32 indexed seriesId, bytes reason);

    /// @notice Emitted when `wire` updates the `desis` and `intexFactory` dependencies and rotates
    ///         their roles.
    /// @param desisOld Previous `desis` address before the rewire (zero on first wiring).
    /// @param desisNew New `desis` address granted `DESIS_ROLE`.
    /// @param intexFactoryOld Previous `intexFactory` address before the rewire (zero on first wiring).
    /// @param intexFactoryNew New `intexFactory` address granted `INTEX_FACTORY_ROLE`.
    event DependenciesWired(address desisOld, address desisNew, address intexFactoryOld, address intexFactoryNew);

    /// @notice Emitted when `sweepNative` transfers native tokens out of the contract.
    /// @param to Recipient of the swept native balance.
    /// @param amount Amount of native tokens (wei) swept.
    event NativeSwept(address indexed to, uint256 amount);

    // --- Types ---
    /// @notice Auction stage start parameters grouped to keep the calldata layout
    ///         resilient against EVM stack depth limits at call sites.
    struct AuctionStageStartParams {
        uint32 seriesId;
        /// @notice End of the commit stage (UNIX seconds).
        uint32 commitEnd;
        /// @notice End of the reveal stage (UNIX seconds).
        uint32 revealEnd;
        /// @notice End of the issuance stage (UNIX seconds).
        uint32 issuanceEnd;
        /// @notice Issuance currency (ISO numeric).
        uint16 issuanceCurrency;
        /// @notice Reference currency (ISO numeric).
        uint16 referenceCurrency;
        /// @notice Promis tokens per Intex unit (18 decimals).
        uint128 promisLoadMinor;
        /// @notice Minimum acceptable bid rate (`1e6` fixed-point, % of the escrow basis).
        uint32 minIntexBidRate;
        /// @notice Per-unit entry price (reference ccy); feeds floor/call.
        uint64 entryPrice;
        /// @notice Floor price (payment-token minor units).
        uint64 floorPriceMinor;
        /// @notice Call price (payment-token minor units).
        uint64 callPriceMinor;
        /// @notice Called→deadline window in seconds (0 = default).
        uint32 intexCallPeriod;
        /// @notice Call-trigger observation window in days.
        uint16 callWindowDays;
        /// @notice Call-trigger threshold in days.
        uint16 callThresholdDays;
        /// @notice Minimum quantity per bid (Intex units).
        uint16 minIntexBidQuantity;
    }

    /// @notice Issuance instructions parameters grouped to keep the calldata layout
    ///         resilient against EVM stack depth limits at call sites.
    /// @dev `issuedIntexCount` is the auction-cleared cap that pins `mint`/`mintBatch` on
    ///      the destination NFT contract. Must equal the auction's cleared count.
    struct IssuanceInstructionsParams {
        uint32 seriesId;
        uint32 issuedIntexCount;
        uint128 promisLoadMinor;
        uint64 entryPriceMinor;
        uint64 floorPriceMinor;
        /// @notice Duration in seconds for the Called -> deadline window (0 = default).
        uint32 intexCallPeriod;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint16 callWindowDays;
        uint16 callThresholdDays;
        uint64 callPriceMinor;
        address[] recipients;
        uint256[] quantities;
    }

    // --- Errors ---
    /// @notice Zero address provided.
    /// @param field Field name that contains zero address
    error ZeroAddress(string field);
    /// @notice Array lengths do not match.
    error ArrayLengthMismatch();
    /// @notice Empty array provided.
    error EmptyArray();
    /// @notice Refund of excess native fee to caller failed.
    error RefundFailed();
    /// @notice External entry-funded call supplied less native than the quoted LZ fee.
    /// @param msgValue Native supplied by the caller (`msg.value`).
    /// @param nativeFee Quoted LZ fee required for the send.
    error MsgValueBelowFee(uint256 msgValue, uint256 nativeFee);
    /// @notice Inbound BIDS_BATCH body-level `srcEid` disagrees with the LayerZero transport-level
    ///         `_origin.srcEid`. Indicates either a peer table misconfiguration or a forged body.
    /// @param origin srcEid from `_origin` (transport-layer truth)
    /// @param body srcEid claimed by the encoded body
    error SrcEidBodyMismatch(uint32 origin, uint32 body);
    /// @notice Address wired as `desis` recipient does not advertise `IDesis` via ERC-165 or
    ///         is an EOA. Wire-time guard against typo'd / wrong-contract addresses.
    /// @param wired Address that failed the interface probe
    error InvalidDesisInterface(address wired);
    /// @notice Native-token sweep transfer failed.
    error NativeSweepFailed();
    /// @notice Native-token balance is insufficient for the requested sweep.
    /// @param available Current contract balance
    /// @param requested Amount the admin attempted to sweep
    error NativeBalanceInsufficient(uint256 available, uint256 requested);
    /// @notice A self-only function was called by an account other than the contract itself.
    error NotSelf();

    // --- Admin ---
    /// @notice Wire contract dependencies and grant the demand/supply roles.
    /// @param desis Desis contract address — must advertise `IDesis` via ERC-165; granted `DESIS_ROLE`
    /// @param intexFactory IntexFactory precompile address; granted `INTEX_FACTORY_ROLE`
    function wire(address desis, address intexFactory) external;

    /// @notice Sweep native tokens (the relay-funded float) from the contract to an admin recipient.
    /// @param to Recipient address (must be non-zero)
    /// @param amount Amount in wei to sweep; must be ≤ contract balance
    function sweepNative(address payable to, uint256 amount) external;

    // --- Send Functions ---
    /// @notice Quote fee for sending auction stage start.
    /// @param params Auction stage start parameters
    /// @param extraOptions Additional LayerZero options
    /// @param payInLzToken Whether to pay fee in LZ token
    /// @return fee Messaging fee quote
    function quoteSendAuctionStageStart(
        AuctionStageStartParams calldata params,
        bytes calldata extraOptions,
        bool payInLzToken
    ) external view returns (MessagingFee memory fee);

    /// @notice Send auction stage start to BNB. Restricted to `DESIS_ROLE`.
    /// @param params Auction stage start parameters
    /// @param extraOptions Additional LayerZero options
    /// @param fee Messaging fee
    /// @param refundAddress Address for fee refund
    /// @return receipt Messaging receipt
    function sendAuctionStageStart(
        AuctionStageStartParams calldata params,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable returns (MessagingReceipt memory receipt);

    /// @notice Quote fee for sending auction stage reveal.
    /// @param seriesId Series identifier
    /// @param isGreenDay True if green day (proceed), false if red day (cancel)
    /// @param extraOptions Additional LayerZero options
    /// @param payInLzToken Whether to pay fee in LZ token
    /// @return fee Messaging fee quote
    function quoteSendAuctionStageReveal(
        uint32 seriesId,
        bool isGreenDay,
        bytes calldata extraOptions,
        bool payInLzToken
    ) external view returns (MessagingFee memory fee);

    /// @notice Send auction stage reveal to BNB. Restricted to `DESIS_ROLE`.
    /// @param seriesId Series identifier
    /// @param isGreenDay True if green day (proceed), false if red day (cancel)
    /// @param extraOptions Additional LayerZero options
    /// @param fee Messaging fee
    /// @param refundAddress Address for fee refund
    /// @return receipt Messaging receipt
    function sendAuctionStageReveal(
        uint32 seriesId,
        bool isGreenDay,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable returns (MessagingReceipt memory receipt);

    /// @notice Quote fee for sending auction stage clearing.
    /// @param seriesId Series identifier
    /// @param extraOptions Additional LayerZero options
    /// @param payInLzToken Whether to pay fee in LZ token
    /// @return fee Messaging fee quote
    function quoteSendAuctionStageClearing(uint32 seriesId, bytes calldata extraOptions, bool payInLzToken)
        external
        view
        returns (MessagingFee memory fee);

    /// @notice Send auction stage clearing to BNB. Restricted to `DESIS_ROLE`.
    /// @param seriesId Series identifier
    /// @param extraOptions Additional LayerZero options
    /// @param fee Messaging fee
    /// @param refundAddress Address for fee refund
    /// @return receipt Messaging receipt
    function sendAuctionStageClearing(
        uint32 seriesId,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable returns (MessagingReceipt memory receipt);

    /// @notice Quote fee for sending auction result.
    /// @param seriesId Series identifier
    /// @param issuedIntexCount Number of Intex units issued
    /// @param auctionClearingRate Uniform clearing rate (`1e6` fixed-point)
    /// @param wonBidsCount Number of winning bids
    /// @param extraOptions Additional LayerZero options
    /// @param payInLzToken Whether to pay fee in LZ token
    /// @return fee Messaging fee quote
    function quoteSendAuctionResult(
        uint32 seriesId,
        uint32 issuedIntexCount,
        uint64 auctionClearingRate,
        uint32 wonBidsCount,
        bytes calldata extraOptions,
        bool payInLzToken
    ) external view returns (MessagingFee memory fee);

    /// @notice Send auction result to BNB. Restricted to `DESIS_ROLE`.
    /// @param seriesId Series identifier
    /// @param issuedIntexCount Number of Intex units issued
    /// @param auctionClearingRate Uniform clearing rate (`1e6` fixed-point)
    /// @param wonBidsCount Number of winning bids
    /// @param extraOptions Additional LayerZero options
    /// @param fee Messaging fee
    /// @param refundAddress Address for fee refund
    /// @return receipt Messaging receipt
    function sendAuctionResult(
        uint32 seriesId,
        uint32 issuedIntexCount,
        uint64 auctionClearingRate,
        uint32 wonBidsCount,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable returns (MessagingReceipt memory receipt);

    /// @notice Quote fee for sending issuance instructions.
    /// @param params Issuance instructions parameters
    /// @param extraOptions Additional LayerZero options
    /// @param payInLzToken Whether to pay fee in LZ token
    /// @return fee Messaging fee quote
    function quoteSendIssuanceInstructions(
        IssuanceInstructionsParams calldata params,
        bytes calldata extraOptions,
        bool payInLzToken
    ) external view returns (MessagingFee memory fee);

    /// @notice Send issuance instructions to BNB. Restricted to `INTEX_FACTORY_ROLE`.
    /// @param params Issuance instructions parameters
    /// @param extraOptions Additional LayerZero options
    /// @param fee Messaging fee
    /// @param refundAddress Address for fee refund
    /// @return receipt Messaging receipt
    function sendIssuanceInstructions(
        IssuanceInstructionsParams calldata params,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable returns (MessagingReceipt memory receipt);

    /// @notice Quote fee for sending refund instructions.
    /// @param seriesId Series identifier
    /// @param bidders Array of bidder addresses
    /// @param refundedAmounts Array of refund amounts per bidder
    /// @param paidAmounts Array of paid-out amounts per bidder
    /// @param extraOptions Additional LayerZero options
    /// @param payInLzToken Whether to pay fee in LZ token
    /// @return fee Messaging fee quote
    function quoteSendRefundInstructions(
        uint32 seriesId,
        address[] calldata bidders,
        uint128[] calldata refundedAmounts,
        uint128[] calldata paidAmounts,
        bytes calldata extraOptions,
        bool payInLzToken
    ) external view returns (MessagingFee memory fee);

    /// @notice Send refund instructions to BNB. Restricted to `DESIS_ROLE`.
    /// @param seriesId Series identifier
    /// @param bidders Array of bidder addresses
    /// @param refundedAmounts Array of refund amounts per bidder
    /// @param paidAmounts Array of paid-out amounts per bidder
    /// @param extraOptions Additional LayerZero options
    /// @param fee Messaging fee
    /// @param refundAddress Address for fee refund
    /// @return receipt Messaging receipt
    function sendRefundInstructions(
        uint32 seriesId,
        address[] calldata bidders,
        uint128[] calldata refundedAmounts,
        uint128[] calldata paidAmounts,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable returns (MessagingReceipt memory receipt);

    /// @notice Quote fee for sending mark called.
    /// @param seriesId Series identifier
    /// @param extraOptions Additional LayerZero options
    /// @param payInLzToken Whether to pay fee in LZ token
    /// @return fee Messaging fee quote
    function quoteSendMarkCalled(uint32 seriesId, bytes calldata extraOptions, bool payInLzToken)
        external
        view
        returns (MessagingFee memory fee);

    /// @notice Send mark called message to BNB. Restricted to `INTEX_FACTORY_ROLE`.
    /// @dev The settlement deadline is derived locally on the destination chain
    ///      from the series `intexCallPeriod` and the moment markCalled is applied.
    /// @param seriesId Series identifier
    /// @param extraOptions Additional LayerZero options
    /// @param fee Messaging fee
    /// @param refundAddress Address for fee refund
    /// @return receipt Messaging receipt
    function sendMarkCalled(
        uint32 seriesId,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable returns (MessagingReceipt memory receipt);

    /// @notice Quote fee for sending mark qualified.
    /// @param seriesId Series identifier
    /// @param extraOptions Additional LayerZero options
    /// @param payInLzToken Whether to pay fee in LZ token
    /// @return fee Messaging fee quote
    function quoteSendMarkQualified(uint32 seriesId, bytes calldata extraOptions, bool payInLzToken)
        external
        view
        returns (MessagingFee memory fee);

    /// @notice Send mark qualified message to BNB, flipping the series to Qualified.
    /// @param seriesId Series identifier
    /// @param extraOptions Additional LayerZero options
    /// @param fee Messaging fee
    /// @param refundAddress Address for fee refund
    /// @return receipt Messaging receipt
    function sendMarkQualified(
        uint32 seriesId,
        bytes calldata extraOptions,
        MessagingFee calldata fee,
        address refundAddress
    ) external payable returns (MessagingReceipt memory receipt);
}
