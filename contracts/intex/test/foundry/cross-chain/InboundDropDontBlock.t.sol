// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {MarkBatchLib} from "../helpers/MarkBatchLib.sol";
import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";

/// @title InboundRevertAndRedeliverTest
/// @notice Under ERC-7786 the routers no longer swallow a failed inbound (there is no ORDERED lane
///         to keep moving). A premature message is one whose on-chain prerequisite has not yet
///         landed. Such a message REVERTS, the bridge rolls back, and the transport redelivers it
///         later. Once the prerequisite lands, re-delivering the same message SUCCEEDS. This
///         preserves the old out-of-order resilience with the new revert-and-redeliver mechanism.
/// @dev Delivery comes from the Interchain Account of Outbe.
contract InboundRevertAndRedeliverTest is CrossChainTest {
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant OUTBE_CHAIN_ID = 2;

    uint32 internal constant SERIES_ID_DAY = 20250101;
    bytes14 internal constant SERIES_ID = "20250101-USD-U";

    IntexAuctionTargetRouter internal bnbRouter;
    IntexAuction internal auction;
    IntexNFT1155 internal intex;
    address internal admin = address(this);
    address internal outbeIca = makeAddr("outbeIca");

    function setUp() public {
        _setUpBridge();

        auction = DeployProxy.intexAuction(admin, admin);
        intex = DeployProxy.intexNFT1155(admin, admin);

        bnbRouter = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        _wireOutbeIca(bnbRouter, outbeIca);

        // TM drives the local Intex on callSeries.
        bnbRouter.setDependencies(address(auction), address(intex), admin);
        auction.grantRole(auction.RELAYER_ROLE(), address(bnbRouter));
        intex.grantRole(intex.ROUTER_ROLE(), address(bnbRouter));
    }

    function _deliverToTM(bytes memory packet) internal {
        _deliver(OUTBE_CHAIN_ID, outbeIca, address(bnbRouter), packet);
    }

    // ---------------------------------------------------------------
    // IntexAuctionTargetRouter - premature MARK_CALLED waits in the series' slot, then applies once the series lands
    // ---------------------------------------------------------------

    /// @notice MARK_CALLED for a series the BNB intex has never seen waits in its slot rather than
    ///         rejecting. A batch carries several series, and one missing series must not reject
    ///         the message.
    function test_TM_PrematureMarkCalled_Parks() public {
        bytes memory packet = BridgeMsgCodec.encodeIntexSeriesCallNotice(
            SERIES_ID_DAY, uint32(block.timestamp), MarkBatchLib.one(SERIES_ID)
        );
        _deliverToTM(packet);

        assertEq(
            bnbRouter.failedIntexSeriesCallNoticeApply(SERIES_ID),
            uint32(block.timestamp),
            "the mark waits for its series"
        );
    }

    /// @notice Once the prerequisite (the series) lands, applying the slotted mark flips the series to
    ///         Called. The out-of-order arrival resolves without a redelivery.
    function test_TM_ParkedMarkCalledFlushesAfterSeriesLands() public {
        bytes memory packet = BridgeMsgCodec.encodeIntexSeriesCallNotice(
            SERIES_ID_DAY, uint32(block.timestamp), MarkBatchLib.one(SERIES_ID)
        );

        // Premature: no series yet -> slotted.
        _deliverToTM(packet);

        // Prerequisite lands (the ISSUANCE that would have created the series).
        intex.openSeries(CreateSeriesLib.params(SERIES_ID_DAY, 10_000, 0));

        bnbRouter.retryIntexSeriesCallNoticeApply(SERIES_ID);

        IIntexNFT1155.SeriesData memory data = intex.getIntexSeries(SERIES_ID);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Called), "series flipped to Called on flush");
    }
}
