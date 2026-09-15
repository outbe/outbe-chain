// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {ReferenceCurrencyPriceLib} from "../helpers/ReferenceCurrencyPriceLib.sol";
import {OriginRouter} from "@contracts/origin/OriginRouter.sol";
import {IOriginRouter} from "@contracts/origin/interfaces/IOriginRouter.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";

/// @dev Supplies a settable number of revealed bids and takes both stage flips without effect.
contract LoopStub {
    uint256 public count;

    function setCount(uint256 n) external {
        count = n;
    }

    function auctionStart(
        uint32,
        IIntexAuction.WorldwideDayState,
        IIntexAuction.AuctionSchedule calldata,
        IIntexAuction.AuctionParams calldata
    ) external {}

    function startClearingStage(uint32) external {}

    function getAuctionStage(uint32) external pure returns (IIntexAuction.AuctionStage) {
        return IIntexAuction.AuctionStage.Issuance;
    }

    function revealedBidsCount(uint32) external view returns (uint256) {
        return count;
    }

    function revealedBidsSlice(uint32, uint256 offset, uint256 limit)
        external
        view
        returns (IIntexAuction.SubmittedBidData[] memory slice)
    {
        uint256 length = count;
        if (offset >= length) return new IIntexAuction.SubmittedBidData[](0);
        uint256 end = offset + limit;
        if (end > length) end = length;
        slice = new IIntexAuction.SubmittedBidData[](end - offset);
        for (uint256 i = 0; i < slice.length; ++i) {
            slice[i] = IIntexAuction.SubmittedBidData({
                bidderAddress: address(uint160(0xBEEF + offset + i)),
                intexBidRate: 100e6,
                intexQuantity: 1,
                timestamp: uint32(block.timestamp),
                issuanceCurrency: 840,
                referenceCurrency: 840
            });
        }
    }
}

/// @notice Outbe as its own target: the bridge delivers inside the sending transaction, so a relay round
///         answered inline would come back into `receiveMessage` while it still holds its re-entry guard.
contract LoopbackRelayRoundTest is CrossChainTest {
    uint32 internal constant DAY = 20_260_801;

    OriginRouter internal origin;
    TargetRouter internal target;
    LoopStub internal stub;
    address internal desis;
    address internal admin = address(this);
    uint32 internal self;

    function setUp() public {
        _setUpBridge();
        bridge.setAutoDeliver(true);
        bridge.setEnforceGasAttribute(true);
        self = uint32(block.chainid);

        origin = DeployProxy.originRouter(address(bridge), admin);
        desis = address(new MockDesis());
        origin.wire(desis, makeAddr("factory"));

        target = DeployProxy.targetRouter(address(bridge), admin, self);
        stub = new LoopStub();
        target.wire(address(stub), makeAddr("intex"), makeAddr("escrow"));

        origin.setRemoteMessenger(self, _interop(self, address(target)));
        origin.addTarget(self);
        target.setRemoteMessenger(self, _interop(self, address(origin)));

        vm.deal(address(origin), 100 ether);
        vm.deal(address(target), 100 ether);

        IOriginRouter.AuctionStageStartParams memory p;
        p.prices = ReferenceCurrencyPriceLib.one(840, 1, 2, 3);
        p.worldwideDay = DAY;
        p.dayState = 1;
        vm.prank(desis);
        origin.sendAuctionStageStart(p);
        assertEq(origin.parkedMessageCount(), 0, "the start leg must land");
    }

    /// @dev A day too heavy for one round parks its next round rather than answering inline, and the parked
    ///      entry - resent in a transaction of its own, as the drain trigger does - finishes the day.
    function test_AnUnfinishedLoopbackDayParksItsNextRound() public {
        stub.setCount(130);

        vm.prank(desis);
        origin.sendAuctionStageClearing(DAY, self, 4_500_000);

        (uint16 nextBatch, uint16 totalBatches, bool done) = target.bidsRelay(DAY);
        assertEq(totalBatches, 3, "130 bids span three chunks");
        assertFalse(done, "one round cannot carry the day");
        assertGt(nextBatch, 0, "the round moved");
        assertLt(nextBatch, totalBatches, "and left chunks behind");

        assertEq(origin.parkedMessageCount(), 1, "the next round is parked, not lost");
        IOriginRouter.ParkedMessage memory parked = origin.parkedMessage(0);
        assertEq(uint8(parked.payload[1]), BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING, "and it is a clearing round");
        assertEq(parked.dstChainId, self, "addressed to the chain that asked");

        // The drain trigger resends it in its own transaction, where no guard is held.
        origin.resendParkedMessage(0);
        (uint16 afterBatch,, bool doneAfter) = target.bidsRelay(DAY);
        assertTrue(afterBatch > nextBatch || doneAfter, "the parked round carried the day on");
    }
}
