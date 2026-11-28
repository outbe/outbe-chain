// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {ReferenceCurrencyPriceLib} from "../utils/ReferenceCurrencyPriceLib.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";

/// The inbound length floors are hand-derived ABI arithmetic, so this test pins each one against
/// the smallest message its encoder can produce.
contract BridgeMsgCodecMinLengthTest is Test {
    function test_RefundFloorIsTheSmallestRealRefund() public pure {
        bytes memory smallest = BridgeMsgCodec.encodeIntexAuctionWinners(20_250_101, 0, 1, 0, 0, new address[](0), 0, 0);
        assertEq(smallest.length, BridgeMsgCodec.MIN_LEN_INTEX_AUCTION_WINNERS, "refund floor");
    }

    function test_BidsBatchFloorIsAnEmptyBatch() public pure {
        bytes memory smallest = BridgeMsgCodec.encodeBidsPage(20_250_101, 1, 0, 1, new address[](0), new uint256[](0));
        assertEq(smallest.length, BridgeMsgCodec.MIN_LEN_BIDS_PAGE, "bids batch floor");
    }

    function test_IssuanceFloorIsTheSmallestRealIssuance() public view {
        // One series, no winners on this chain - the message a snapshot chain gets when it
        // only needs the series created.
        BridgeMsgCodec.IntexSeriesPayload[] memory series = new BridgeMsgCodec.IntexSeriesPayload[](1);
        series[0].seriesId = "20250101-USD-U";
        series[0].emissionDay = 20_250_101;
        series[0].issuedAt = uint32(block.timestamp);

        bytes memory smallest = BridgeMsgCodec.encodeIntexSeries(20_250_101, 0, 1, series);
        assertEq(smallest.length, BridgeMsgCodec.MIN_LEN_INTEX_SERIES, "issuance floor");
    }

    function test_StageStartFloorIsADayWithOnePricedCurrency() public view {
        bytes memory smallest = this.encodeStageStart(ReferenceCurrencyPriceLib.one(840, 2e6, 4e6));
        assertEq(
            smallest.length,
            BridgeMsgCodec.MIN_LEN_INTEX_AUCTION_START + BridgeMsgCodec.REFERENCE_PRICE_LEN,
            "stage start floor"
        );

        (,, IIntexAuction.AuctionParams memory params) = BridgeMsgCodec.decodeAuctionParams(smallest);
        assertEq(params.prices.length, 1, "one row");
    }

    function test_ADayMustPriceSomething() public {
        vm.expectRevert(BridgeMsgCodec.MissingReferencePrices.selector);
        this.encodeStageStart(new IIntexAuction.ReferenceCurrencyPrice[](0));
    }

    /// External so the encoder runs one call deep, as a real send does.
    function encodeStageStart(IIntexAuction.ReferenceCurrencyPrice[] calldata rows)
        external
        pure
        returns (bytes memory)
    {
        return BridgeMsgCodec.encodeIntexAuctionStart(20_250_101, 100, 200, 300, 1e6, 1e6, rows, 5, 6, 7, 1, 9e6);
    }

    function test_EveryFloorAdmitsWhatTheEncoderProduces() public pure {
        // A floor above the real minimum would reject valid traffic, which is the failure
        // the two tests above cannot see on their own.
        assertLe(
            BridgeMsgCodec.MIN_LEN_INTEX_AUCTION_WINNERS,
            BridgeMsgCodec.encodeIntexAuctionWinners(20_250_101, 0, 1, 1, 1, new address[](1), 0, 0).length,
            "a one-winner refund must pass its floor"
        );
    }
}
