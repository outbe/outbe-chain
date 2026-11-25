// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IIntexAuctionTargetRouter} from "@contracts/target/interfaces/IIntexAuctionTargetRouter.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {InboundReason} from "@contracts/shared/libs/InboundReason.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";

/// @dev Auction stand-in whose every day reads as failed.
contract FailedDayAuction {
    function auctionStage(uint32) external pure returns (IIntexAuction.AuctionStage) {
        return IIntexAuction.AuctionStage.Failed;
    }
}

/// @dev Escrow stand-in that counts finalizations and can be told the day is closed.
contract ClosableEscrowAdapter {
    IERC20 public paymentToken;
    bool public closed;
    uint256 public finalizations;

    constructor(IERC20 token) {
        paymentToken = token;
    }

    function close() external {
        closed = true;
    }

    function chargeIntexAuctionAsset(uint32, address[] calldata, uint16, uint16, uint64, uint128, bool)
        external
        returns (uint128)
    {
        finalizations++;
        return 0;
    }

    function auctionStatus(uint32) external view returns (bool, bool, uint128) {
        return (false, closed, 0);
    }
}

/// The router acknowledges without effect a refund chunk that was already applied, or that lands
/// after the escrow closed the day. The chunk does not bounce back to the transport.
contract IntexAuctionTargetRouterRefundAckTest is CrossChainTest {
    uint32 internal constant OUTBE_CHAIN_ID = 2;
    uint32 internal constant DAY = 20_260_713;

    IntexAuctionTargetRouter internal target;
    ClosableEscrowAdapter internal escrow;
    address internal outbeIca = makeAddr("outbeIca");

    function setUp() public {
        _setUpBridge();
        target = DeployProxy.targetRouter(address(bridge), address(this), OUTBE_CHAIN_ID);
        escrow = new ClosableEscrowAdapter(IERC20(address(new MockWCOEN())));
        target.setDependencies(makeAddr("auction"), makeAddr("intex"), address(escrow));
        _wireOutbeIca(target, outbeIca);
    }

    function _deliverChunk(uint16 pageIndex, uint16 totalPages) internal {
        address[] memory winners = new address[](1);
        winners[0] = makeAddr("bidder");
        bytes memory packet =
            BridgeMsgCodec.encodeIntexAuctionWinners(DAY, pageIndex, totalPages, 600_000, 1e6, winners, 0, 0);
        _deliver(OUTBE_CHAIN_ID, outbeIca, address(target), packet);
    }

    function _expectIgnored(uint16 pageIndex, uint8 reason) internal {
        vm.expectEmit(true, true, false, true, address(target));
        emit IIntexAuctionTargetRouter.IntexAuctionMessageIgnored(BridgeMsgCodec.MSG_INTEX_AUCTION_WINNERS, DAY, reason);
    }

    function test_AChunkForAFailedDayIsObsolete() public {
        IntexAuctionTargetRouter failedTarget = DeployProxy.targetRouter(address(bridge), address(this), OUTBE_CHAIN_ID);
        failedTarget.setDependencies(address(new FailedDayAuction()), makeAddr("intex"), address(escrow));
        _wireOutbeIca(failedTarget, outbeIca);
        target = failedTarget;

        _expectIgnored(0, InboundReason.OBSOLETE);
        _deliverChunk(0, 1);
        assertEq(escrow.finalizations(), 0, "a failed day settles nothing");
    }

    function test_ARepeatedChunkIsADuplicate() public {
        _deliverChunk(0, 2);
        _expectIgnored(0, InboundReason.DUPLICATE);
        _deliverChunk(0, 2);
        assertEq(escrow.finalizations(), 1, "settled once");
    }

    function test_AChunkClaimingAnotherTotalCannotCloseTheDayEarly() public {
        _deliverChunk(1, 2); // fixes the day's total at 2
        _expectIgnored(0, InboundReason.CONFLICT);
        _deliverChunk(0, 1);
        assertEq(escrow.finalizations(), 1, "the conflicting chunk never reaches the escrow");
        (uint16 seen, uint16 total) = target.getIntexAuctionReturnState(DAY);
        assertEq(seen, 1, "not counted");
        assertEq(total, 2, "the first chunk's total stands");

        _deliverChunk(0, 2);
        (seen,) = target.getIntexAuctionReturnState(DAY);
        assertEq(seen, 2, "day completed by the corrected chunk");
    }

    function test_AChunkAfterTheEscrowClosedTheDayIsObsolete() public {
        _deliverChunk(0, 2);
        escrow.close();
        _expectIgnored(1, InboundReason.OBSOLETE);
        _deliverChunk(1, 2);
        assertEq(escrow.finalizations(), 1, "the late chunk never reaches the escrow");
    }
}
