// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {ITargetRouter} from "@contracts/target/interfaces/ITargetRouter.sol";
import {ERC7786MessengerBase} from "@contracts/shared/ERC7786MessengerBase.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {InboundReason} from "@contracts/shared/libs/InboundReason.sol";
import {IssuanceBatchLib} from "../helpers/IssuanceBatch.sol";

/// A day's series reach a chain as numbered chunks. The router acknowledges these without effect:
/// - a repeated chunk
/// - a chunk that disagrees with the day's run
/// - a series named under other terms
/// The last chunk completes the day.
contract TargetRouterIssuanceChunksTest is CrossChainTest {
    uint32 internal constant OUTBE_CHAIN_ID = 2;
    uint32 internal constant DAY = 20_250_101;
    bytes14 internal constant USD = "20250101-USD-U";
    bytes14 internal constant EUR = "20250101-EUR-E";

    TargetRouter internal router;
    IntexNFT1155 internal intex;
    address internal originSender = makeAddr("originSender");

    function setUp() public {
        _setUpBridge();
        intex = DeployProxy.intexNFT1155(address(this), address(this));
        router = DeployProxy.targetRouter(address(bridge), address(this), OUTBE_CHAIN_ID);

        router.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, originSender));
        router.wire(makeAddr("auction"), address(intex), makeAddr("escrow"));
        intex.grantRole(intex.RELAYER_ROLE(), address(router));
    }

    // --- helpers ---

    function _series(bytes14 seriesId)
        internal
        view
        returns (BridgeMsgCodec.IssuanceInstructionsPayload memory payload)
    {
        payload.seriesId = seriesId;
        payload.worldwideDay = DAY;
        payload.issuedAt = uint32(block.timestamp);
        payload.issuedUnits = 100;
        payload.promisLoadMinor = 1_000;
        payload.entryPriceMinor = 100e6;
        payload.issuanceCurrency = 840;
        payload.referenceCurrency = 840;
        payload.callWindow = 30;
        payload.callThreshold = 5;
        payload.callPriceMinor = 200e6;
    }

    function _deliver(uint16 chunkIndex, uint16 totalChunks, BridgeMsgCodec.IssuanceInstructionsPayload[] memory series)
        internal
    {
        _deliver(
            OUTBE_CHAIN_ID,
            originSender,
            address(router),
            BridgeMsgCodec.encodeIssuanceInstructions(DAY, chunkIndex, totalChunks, series)
        );
    }

    // --- repeats ---

    function test_ARepeatedChunkIsAcknowledgedAndNotCounted() public {
        _deliver(0, 1, IssuanceBatchLib.one(_series(USD)));
        assertTrue(intex.seriesExists(USD), "first delivery creates");

        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.InboundMessageIgnored(
            OUTBE_CHAIN_ID,
            BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS,
            bytes32(uint256(DAY) << 16),
            InboundReason.DUPLICATE
        );
        _deliver(0, 1, IssuanceBatchLib.one(_series(USD)));
        (uint16 seen, uint16 total) = router.issuanceChunks(DAY);
        assertEq(seen, 1, "repeat is not counted");
        assertEq(total, 1, "total unchanged");
    }

    // --- conflicts ---

    function test_AChunkDisagreeingOnTheTotalIsSkipped() public {
        _deliver(0, 2, IssuanceBatchLib.one(_series(USD)));

        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.InboundMessageIgnored(
            OUTBE_CHAIN_ID,
            BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS,
            bytes32((uint256(DAY) << 16) | 1),
            InboundReason.CONFLICT
        );
        _deliver(1, 3, IssuanceBatchLib.one(_series(EUR)));
        assertFalse(intex.seriesExists(EUR), "nothing of the conflicting chunk applied");
        (uint16 seen,) = router.issuanceChunks(DAY);
        assertEq(seen, 1, "conflicting chunk not counted");
    }

    function test_ASeriesNamedUnderOtherTermsSkipsTheWholeChunk() public {
        _deliver(0, 2, IssuanceBatchLib.one(_series(USD)));

        BridgeMsgCodec.IssuanceInstructionsPayload[] memory chunk = new BridgeMsgCodec.IssuanceInstructionsPayload[](2);
        chunk[0] = _series(EUR);
        chunk[1] = _series(USD);
        chunk[1].entryPriceMinor = 101e6; // disagrees with the series this chain already holds

        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.InboundMessageIgnored(
            OUTBE_CHAIN_ID,
            BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS,
            bytes32((uint256(DAY) << 16) | 1),
            InboundReason.CONFLICT
        );
        _deliver(1, 2, chunk);
        assertFalse(intex.seriesExists(EUR), "the healthy series of the chunk is not created either");
        assertFalse(router.issuanceChunkApplied(DAY, 1), "chunk stays unapplied so a corrected resend can land");
    }

    function test_ACorrectedResendOfASkippedChunkLands() public {
        _deliver(0, 2, IssuanceBatchLib.one(_series(USD)));
        BridgeMsgCodec.IssuanceInstructionsPayload[] memory chunk = new BridgeMsgCodec.IssuanceInstructionsPayload[](2);
        chunk[0] = _series(EUR);
        chunk[1] = _series(USD);
        chunk[1].entryPriceMinor = 101e6;
        _deliver(1, 2, chunk);
        assertFalse(intex.seriesExists(EUR), "conflict skipped");

        chunk[1].entryPriceMinor = _series(USD).entryPriceMinor;
        _deliver(1, 2, chunk);
        assertTrue(intex.seriesExists(EUR), "corrected chunk applied");
    }

    function test_ASeriesWithNoSupplyIsInvalidNotALoop() public {
        BridgeMsgCodec.IssuanceInstructionsPayload memory p = _series(USD);
        p.issuedUnits = 0;
        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.InboundMessageIgnored(
            OUTBE_CHAIN_ID, BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS, bytes32(uint256(DAY) << 16), InboundReason.INVALID
        );
        _deliver(0, 1, IssuanceBatchLib.one(p));
        assertFalse(intex.seriesExists(USD), "nothing created");
        assertFalse(router.issuanceChunkApplied(DAY, 0), "a corrected resend can still land");
    }

    function test_ADuplicateSeriesWithOtherTermsInsideOneChunkIsAConflict() public {
        BridgeMsgCodec.IssuanceInstructionsPayload[] memory chunk = new BridgeMsgCodec.IssuanceInstructionsPayload[](2);
        chunk[0] = _series(USD);
        chunk[1] = _series(USD);
        chunk[1].entryPriceMinor = 101e6;

        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.InboundMessageIgnored(
            OUTBE_CHAIN_ID,
            BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS,
            bytes32(uint256(DAY) << 16),
            InboundReason.CONFLICT
        );
        _deliver(0, 1, chunk);
        assertFalse(intex.seriesExists(USD), "nothing of the chunk applied");
    }

    function test_ASeriesNamedTwiceInsideOneChunkIsCreatedOnce() public {
        BridgeMsgCodec.IssuanceInstructionsPayload[] memory chunk = new BridgeMsgCodec.IssuanceInstructionsPayload[](2);
        chunk[0] = _series(USD);
        chunk[1] = _series(USD);
        _deliver(0, 1, chunk);
        assertTrue(intex.seriesExists(USD));
        assertTrue(router.issuanceChunkApplied(DAY, 0), "chunk applied");
    }

    // --- completeness ---

    function test_TheLastChunkCompletesTheDayWhateverTheOrder() public {
        vm.recordLogs();
        _deliver(2, 3, IssuanceBatchLib.one(_series(EUR)));
        _deliver(0, 3, IssuanceBatchLib.one(_series(USD)));
        assertEq(_countTopic(ITargetRouter.IssuanceCompleted.selector), 0, "not complete yet");

        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.IssuanceCompleted(DAY, 3);
        _deliver(1, 3, IssuanceBatchLib.one(_series(USD)));
        (uint16 seen, uint16 total) = router.issuanceChunks(DAY);
        assertEq(seen, 3, "every chunk counted");
        assertEq(total, 3, "total as declared");
    }

    function test_ASingleChunkCompletesTheDay() public {
        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.IssuanceCompleted(DAY, 1);
        _deliver(0, 1, IssuanceBatchLib.one(_series(USD)));
        assertTrue(intex.seriesExists(USD), "series created");
    }
}
