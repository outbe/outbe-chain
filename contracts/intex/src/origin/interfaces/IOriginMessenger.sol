// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title IOriginMessenger
/// @author Outbe
/// @notice Interface for the Outbe-side messenger. Sends auction/series messages to BNB and receives BIDS_BATCH
///         from BNB over the protocol-agnostic ERC-7786 bridge.
/// @dev All auction/series messages are keyed by `seriesId` (uint32). Outbound `send*` return the bridge `sendId`
///      and are funded either from `msg.value` or the contract's relay float (see {ERC7786MessengerBase}); `quote*`
///      return the native fee. Inbound delivery arrives via {ERC7786MessengerBase-receiveMessage}.
interface IOriginMessenger {
    // --- Events ---
    /// @notice Emitted when a BIDS_BATCH is received from BNB.
    /// @param srcChainId Source chainId the message was authenticated against.
    /// @param seriesId Series identifier.
    /// @param bidsCount Number of bids received.
    event BidsBatchReceived(uint32 indexed srcChainId, uint32 indexed seriesId, uint256 bidsCount);

    /// @notice Emitted when an auction stage message is sent to BNB.
    /// @param sendId Bridge send identifier.
    /// @param seriesId Series identifier.
    /// @param stageType Codec message type (start/reveal/clearing).
    event AuctionStageSent(bytes32 indexed sendId, uint32 indexed seriesId, uint8 stageType);

    /// @notice Emitted when an auction result is sent to BNB.
    /// @param sendId Bridge send identifier.
    /// @param seriesId Series identifier.
    /// @param issuedIntexCount Number of Intex units issued.
    /// @param clearingRate Uniform clearing rate (`1e6` fixed-point).
    event AuctionResultSent(
        bytes32 indexed sendId, uint32 indexed seriesId, uint32 issuedIntexCount, uint64 clearingRate
    );

    /// @notice Emitted when issuance instructions are sent to BNB.
    /// @param sendId Bridge send identifier.
    /// @param seriesId Series identifier.
    /// @param recipientsCount Number of recipients.
    event IssuanceInstructionsSent(bytes32 indexed sendId, uint32 indexed seriesId, uint256 recipientsCount);

    /// @notice Emitted when refund instructions are sent to BNB.
    /// @param sendId Bridge send identifier.
    /// @param seriesId Series identifier.
    /// @param instructionsCount Number of finalization instructions.
    event RefundInstructionsSent(bytes32 indexed sendId, uint32 indexed seriesId, uint256 instructionsCount);

    /// @notice Emitted when a mark-called message is sent to BNB.
    /// @param sendId Bridge send identifier.
    /// @param seriesId Series identifier.
    event MarkCalledSent(bytes32 indexed sendId, uint32 indexed seriesId);

    /// @notice Emitted when a mark-qualified message is sent to BNB.
    /// @param sendId Bridge send identifier.
    /// @param seriesId Series identifier.
    event MarkQualifiedSent(bytes32 indexed sendId, uint32 indexed seriesId);

    /// @notice Emitted when `_handleBidsBatch` auto-fires `Desis.clearAuction` for a `BidsReceived` series.
    /// @param seriesId Series identifier whose auction was auto-cleared.
    event ClearingAutoDispatched(uint32 indexed seriesId);

    /// @notice Emitted when the auto-fired `Desis.clearAuction` reverts; the bid intake is kept.
    /// @param seriesId Series identifier whose auto-clearing reverted.
    /// @param reason Raw revert bytes from the failed `clearAuction` call.
    event ClearingAutoDispatchFailed(uint32 indexed seriesId, bytes reason);

    /// @notice Emitted when `wire` updates the `desis` and `intexFactory` dependencies and rotates their roles.
    /// @param desisOld Previous `desis` (zero on first wiring).
    /// @param desisNew New `desis` granted `DESIS_ROLE`.
    /// @param intexFactoryOld Previous `intexFactory` (zero on first wiring).
    /// @param intexFactoryNew New `intexFactory` granted `INTEX_FACTORY_ROLE`.
    event DependenciesWired(address desisOld, address desisNew, address intexFactoryOld, address intexFactoryNew);

    /// @notice Emitted when `sweepNative` transfers native tokens out of the contract.
    /// @param to Recipient of the swept native balance.
    /// @param amount Amount of native tokens (wei) swept.
    event NativeSwept(address indexed to, uint256 amount);

    // --- Types ---
    /// @notice Auction stage start parameters grouped to keep the calldata layout resilient against stack limits.
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

    /// @notice Issuance instructions parameters grouped to keep the calldata layout resilient against stack limits.
    /// @dev `issuedIntexCount` is the auction-cleared cap that pins `mint`/`mintBatch` on the destination NFT
    ///      contract. Must equal the auction's cleared count.
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
    /// @param field Field name that contains zero address.
    error ZeroAddress(string field);
    /// @notice Array lengths do not match.
    error ArrayLengthMismatch();
    /// @notice Empty array provided.
    error EmptyArray();
    /// @notice Inbound BIDS_BATCH body-level `srcChainId` disagrees with the authenticated source chainId.
    /// @param origin Source chainId the bridge authenticated.
    /// @param body Source chainId claimed by the encoded body.
    error SrcChainIdBodyMismatch(uint32 origin, uint32 body);
    /// @notice Address wired as `desis` does not advertise `IDesis` via ERC-165 or is an EOA.
    /// @param wired Address that failed the interface probe.
    error InvalidDesisInterface(address wired);
    /// @notice Native-token sweep transfer failed.
    error NativeSweepFailed();
    /// @notice Native-token balance is insufficient for the requested sweep.
    /// @param available Current contract balance.
    /// @param requested Amount the admin attempted to sweep.
    error NativeBalanceInsufficient(uint256 available, uint256 requested);

    // --- Admin ---
    /// @notice Wire contract dependencies and grant the demand/supply roles.
    /// @param desis Desis contract — must advertise `IDesis` via ERC-165; granted `DESIS_ROLE`.
    /// @param intexFactory IntexFactory precompile; granted `INTEX_FACTORY_ROLE`.
    function wire(address desis, address intexFactory) external;

    /// @notice Register (or clear) the matching messenger on `chainId` as an ERC-7930 interoperable address.
    /// @param chainId Destination/source chainId.
    /// @param interop ERC-7930 interoperable address (empty to clear).
    function setRemoteMessenger(uint32 chainId, bytes calldata interop) external;

    /// @notice Sweep native tokens (the relay-funded float) from the contract to an admin recipient.
    /// @param to Recipient address (must be non-zero).
    /// @param amount Amount in wei to sweep; must be ≤ contract balance.
    function sweepNative(address payable to, uint256 amount) external;

    // --- Quote ---
    /// @notice Native fee to send auction stage start to BNB.
    function quoteSendAuctionStageStart(AuctionStageStartParams calldata params) external view returns (uint256 fee);
    /// @notice Native fee to send auction stage reveal to BNB.
    function quoteSendAuctionStageReveal(uint32 seriesId, bool isGreenDay) external view returns (uint256 fee);
    /// @notice Native fee to send auction stage clearing to BNB.
    function quoteSendAuctionStageClearing(uint32 seriesId) external view returns (uint256 fee);
    /// @notice Native fee to send auction result to BNB.
    function quoteSendAuctionResult(
        uint32 seriesId,
        uint32 issuedIntexCount,
        uint64 auctionClearingRate,
        uint32 wonBidsCount
    ) external view returns (uint256 fee);
    /// @notice Native fee to send issuance instructions to BNB.
    function quoteSendIssuanceInstructions(IssuanceInstructionsParams calldata params)
        external
        view
        returns (uint256 fee);
    /// @notice Native fee to send refund instructions to BNB.
    function quoteSendRefundInstructions(
        uint32 seriesId,
        address[] calldata bidders,
        uint128[] calldata refundedAmounts,
        uint128[] calldata paidAmounts
    ) external view returns (uint256 fee);
    /// @notice Native fee to send mark-called to BNB.
    function quoteSendMarkCalled(uint32 seriesId) external view returns (uint256 fee);
    /// @notice Native fee to send mark-qualified to BNB.
    function quoteSendMarkQualified(uint32 seriesId) external view returns (uint256 fee);

    // --- Send ---
    /// @notice Send auction stage start to BNB. Restricted to `DESIS_ROLE`.
    function sendAuctionStageStart(AuctionStageStartParams calldata params) external payable returns (bytes32 sendId);
    /// @notice Send auction stage reveal to BNB. Restricted to `DESIS_ROLE`.
    function sendAuctionStageReveal(uint32 seriesId, bool isGreenDay) external payable returns (bytes32 sendId);
    /// @notice Send auction stage clearing to BNB. Restricted to `DESIS_ROLE`.
    function sendAuctionStageClearing(uint32 seriesId) external payable returns (bytes32 sendId);
    /// @notice Send auction result to BNB. Restricted to `DESIS_ROLE`.
    function sendAuctionResult(
        uint32 seriesId,
        uint32 issuedIntexCount,
        uint64 auctionClearingRate,
        uint32 wonBidsCount
    ) external payable returns (bytes32 sendId);
    /// @notice Send issuance instructions to BNB. Restricted to `INTEX_FACTORY_ROLE`.
    function sendIssuanceInstructions(IssuanceInstructionsParams calldata params)
        external
        payable
        returns (bytes32 sendId);
    /// @notice Send refund instructions to BNB. Restricted to `DESIS_ROLE`.
    function sendRefundInstructions(
        uint32 seriesId,
        address[] calldata bidders,
        uint128[] calldata refundedAmounts,
        uint128[] calldata paidAmounts
    ) external payable returns (bytes32 sendId);
    /// @notice Send mark-called to BNB. Restricted to `INTEX_FACTORY_ROLE`.
    /// @dev The settlement deadline is derived on the destination chain from `intexCallPeriod`.
    function sendMarkCalled(uint32 seriesId) external payable returns (bytes32 sendId);
    /// @notice Send mark-qualified to BNB, flipping the series to Qualified. Restricted to `INTEX_FACTORY_ROLE`.
    function sendMarkQualified(uint32 seriesId) external payable returns (bytes32 sendId);
}
