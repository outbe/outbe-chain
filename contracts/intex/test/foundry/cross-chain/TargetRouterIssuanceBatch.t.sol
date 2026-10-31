// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {OriginRouter} from "@contracts/origin/OriginRouter.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

/// A chain's share of a day is a set of series. The set arrives in as few messages as the cap
/// allows. A series repeated in another message is safe: the receiver creates it only if absent.
contract TargetRouterIssuanceBatchTest is CrossChainTest {
    uint32 internal constant OUTBE_CHAIN_ID = 2;
    uint32 internal constant DAY = 20_250_101;

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

    function _series(bytes14 seriesId)
        internal
        view
        returns (BridgeMsgCodec.IssuanceInstructionsPayload memory payload)
    {
        payload.seriesId = seriesId;
        payload.emissionDay = DAY;
        payload.issuedAt = uint32(block.timestamp);
        payload.issuedUnits = 1_000;
        payload.promisLoadMinor = 1_000;
        payload.entryPriceMinor = 100e6;
        payload.issuanceCurrency = 840;
        payload.referenceCurrency = 840;
        payload.callWindow = 30;
        payload.callThreshold = 5;
        payload.callPriceMinor = 200e6;
    }

    function _deliver(BridgeMsgCodec.IssuanceInstructionsPayload[] memory series) internal {
        _deliver(0, 1, series);
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

    function test_OneMessageCreatesEverySeriesItCarries() public {
        BridgeMsgCodec.IssuanceInstructionsPayload[] memory series = new BridgeMsgCodec.IssuanceInstructionsPayload[](3);
        series[0] = _series("20250101-USD-U");
        series[1] = _series("20250101-EUR-E");
        series[2] = _series("20250101-TRY-U");

        _deliver(series);

        assertTrue(intex.seriesExists("20250101-USD-U"), "dollar series");
        assertTrue(intex.seriesExists("20250101-EUR-E"), "euro series");
        assertTrue(intex.seriesExists("20250101-TRY-U"), "lira series");
        assertEq(intex.totalSupply(intex.issuedTokenId("20250101-USD-U")), 0, "the message mints nothing");
    }

    function test_ASeriesRepeatedInALaterMessageIsCreatedOnce() public {
        BridgeMsgCodec.IssuanceInstructionsPayload[] memory head = new BridgeMsgCodec.IssuanceInstructionsPayload[](1);
        head[0] = _series("20250101-USD-U");
        _deliver(0, 2, head);

        // Creating it again would revert, so the receiver must recognise that it already exists.
        _deliver(1, 2, head);

        assertTrue(intex.seriesExists("20250101-USD-U"));
        (uint16 seen, uint16 total) = router.issuanceChunks(DAY);
        assertEq(seen, 2, "both chunks applied");
        assertEq(total, 2);
    }

    function test_AFullMessageStaysUnderTheSendCeiling() public view {
        // The cap is a count, not bytes. So pin that the worst case it admits still fits the wire.
        uint256 seriesCount = BridgeMsgCodec.MAX_SERIES_PER_ISSUANCE;
        BridgeMsgCodec.IssuanceInstructionsPayload[] memory series =
            new BridgeMsgCodec.IssuanceInstructionsPayload[](seriesCount);
        for (uint256 s = 0; s < seriesCount; s++) {
            series[s] = _series(bytes14(uint112(uint256(0x3230323530313031) + s)));
        }

        uint256 encoded = BridgeMsgCodec.encodeIssuanceInstructions(DAY, 0, 1, series).length;
        assertLt(encoded, 10_000, "a full message must fit the bridge send ceiling");
    }
}
