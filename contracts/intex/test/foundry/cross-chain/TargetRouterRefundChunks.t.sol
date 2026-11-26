// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {ITargetRouter} from "@contracts/target/interfaces/ITargetRouter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @dev Escrow stand-in that charges each winner in the chunk its basis.
contract SummingEscrowAdapter {
    IERC20 public paymentToken;

    constructor(IERC20 token) {
        paymentToken = token;
    }

    function dayIntexAuctionAsset(uint32) external view returns (IERC20) {
        return paymentToken;
    }

    function chargeIntexAuctionAsset(uint32, address[] calldata winners, uint16, uint16, uint64, uint128 basis, bool)
        external
        pure
        returns (uint128 totalPaid)
    {
        totalPaid = uint128(basis * winners.length);
    }

    function auctionStatus(uint32) external pure returns (bool, bool, uint128) {
        return (false, false, 0);
    }

    function hasOutstandingLocks() external pure returns (bool) {
        return false;
    }
}

/// @dev Counts proceeds transfers and remembers the last amount.
contract CountingTokenBridge {
    IERC20 public token;
    uint256 public calls;

    constructor(IERC20 token_) {
        token = token_;
    }

    uint256 public lastAmount;

    function quoteSend(uint32, address, uint256, bytes calldata, uint256) external pure returns (uint256) {
        return 0.001 ether;
    }

    function sendAndCall(uint32, address, uint256 amount, bytes calldata, uint256) external payable returns (bytes32) {
        calls++;
        lastAmount = amount;
        return bytes32(uint256(1));
    }
}

/// A chain's refunds arrive as a run of chunks, and each chunk sends its own proceeds home. A chunk
/// that never lands holds back no other chunk's proceeds.
contract TargetRouterRefundChunksTest is CrossChainTest {
    uint32 internal constant OUTBE_CHAIN_ID = 2;
    uint32 internal constant DAY = 20_260_713;

    TargetRouter internal target;
    CountingTokenBridge internal tokenBridge;

    address internal outbeIca = makeAddr("outbeIca");

    function setUp() public {
        _setUpBridge();
        MockWCOEN wcoen = new MockWCOEN();
        SummingEscrowAdapter escrow = new SummingEscrowAdapter(IERC20(address(wcoen)));
        tokenBridge = new CountingTokenBridge(IERC20(address(wcoen)));

        target = DeployProxy.targetRouter(address(bridge), address(this), OUTBE_CHAIN_ID, address(tokenBridge));
        vm.deal(address(target), 10 ether);
        target.setDependencies(makeAddr("auction"), makeAddr("intex"), address(escrow));
        _wireOutbeIca(target, outbeIca);
    }

    function _deliverChunk(uint16 pageIndex, uint16 totalPages, uint128 paidAmount) internal {
        address[] memory winners = new address[](1);
        winners[0] = address(uint160(uint256(0xB1D) + pageIndex));
        bytes memory packet =
            BridgeMsgCodec.encodeIntexAuctionWinners(DAY, pageIndex, totalPages, 1_000_000, paidAmount, winners, 0, 0);
        _deliver(OUTBE_CHAIN_ID, outbeIca, address(target), packet);
    }

    function test_EachChunkSendsItsOwnProceeds() public {
        vm.expectEmit(false, true, true, true, address(target));
        emit ITargetRouter.IntexAuctionProceedsSent(bytes32(0), DAY, 0, 40e6);
        vm.expectEmit(true, true, false, true, address(target));
        emit ITargetRouter.IntexAuctionWinnersReceived(DAY, OUTBE_CHAIN_ID, 0, 2, 1);
        _deliverChunk(0, 2, 40e6);
        assertEq(tokenBridge.calls(), 1, "the first chunk routes at once");
        assertEq(tokenBridge.lastAmount(), 40e6);

        vm.expectEmit(false, true, true, true, address(target));
        emit ITargetRouter.IntexAuctionProceedsSent(bytes32(0), DAY, 1, 60e6);
        vm.expectEmit(true, true, false, true, address(target));
        emit ITargetRouter.IntexAuctionWinnersReceived(DAY, OUTBE_CHAIN_ID, 1, 2, 1);
        _deliverChunk(1, 2, 60e6);
        assertEq(tokenBridge.calls(), 2, "the second chunk routes its own part");
        assertEq(tokenBridge.lastAmount(), 60e6);
    }

    function test_RedeliveredChunkNeitherRoutesNorRecounts() public {
        _deliverChunk(0, 2, 40e6);
        _deliverChunk(0, 2, 40e6); // the bridge may deliver the same chunk twice
        assertEq(tokenBridge.calls(), 1, "a repeat must not route again");
        (uint16 seen,) = target.getIntexAuctionReturnState(DAY);
        assertEq(seen, 1, "a repeat must not be counted");
    }

    function test_AMissingChunkHoldsBackNoOtherProceeds() public {
        _deliverChunk(0, 3, 40e6);
        _deliverChunk(2, 3, 30e6);
        assertEq(tokenBridge.calls(), 2, "both landed chunks routed while chunk 1 is missing");
        assertEq(tokenBridge.lastAmount(), 30e6);

        _deliverChunk(1, 3, 60e6);
        assertEq(tokenBridge.calls(), 3, "the late chunk routes its own part");
        assertEq(tokenBridge.lastAmount(), 60e6);
        (uint16 seen, uint16 total) = target.getIntexAuctionReturnState(DAY);
        assertEq(seen, total, "the day completes");
    }

    function test_ChunksMayArriveInAnyOrder() public {
        _deliverChunk(1, 2, 60e6);
        _deliverChunk(0, 2, 40e6);
        assertEq(tokenBridge.calls(), 2);
        assertEq(tokenBridge.lastAmount(), 40e6);
    }

    function test_SingleChunkDayRoutesImmediately() public {
        _deliverChunk(0, 1, 25e6);
        assertEq(tokenBridge.calls(), 1);
        assertEq(tokenBridge.lastAmount(), 25e6);
    }

    function test_AChunkWithoutPaymentsRoutesNothing() public {
        _deliverChunk(0, 2, 0);
        assertEq(tokenBridge.calls(), 0);
    }

    /// This test drives the real adapter. The rule that matters is that a finalized day refuses
    /// everything after. That rule is exactly what the mock above cannot express.
    function test_TheRealEscrowSettlesEveryChunkOfADay() public {
        MockWCOEN token = new MockWCOEN();
        MockTheCompact compact = new MockTheCompact();
        EscrowAdapter escrow = DeployProxy.escrowAdapter(address(this), address(this));
        escrow.setDependencies(address(this), address(compact), address(token));
        compact.setResetPeriodSeconds(0);
        escrow.grantRole(escrow.RELAYER_ROLE(), address(target));
        escrow.grantRole(escrow.AUCTION_ROLE(), address(this));
        escrow.setProceedsRecipient(address(target));
        target.setDependencies(makeAddr("auction"), makeAddr("intex"), address(escrow));

        address[2] memory bidders = [makeAddr("early"), makeAddr("late")];
        for (uint256 i = 0; i < bidders.length; i++) {
            token.mint(bidders[i], 1e18);
            vm.prank(bidders[i]);
            token.approve(address(escrow), type(uint256).max);
            escrow.lockIntexAuctionAsset(DAY, bidders[i], 1e6, 1_000_000, 1);
        }

        _deliverChunkFor(bidders[0], 0, 2);
        _deliverChunkFor(bidders[1], 1, 2);

        assertTrue(
            escrow.getIntexAuctionBidLock(DAY, bidders[0]).status == IEscrowAdapter.LockStatus.Won,
            "first chunk settled"
        );
        assertTrue(
            escrow.getIntexAuctionBidLock(DAY, bidders[1]).status == IEscrowAdapter.LockStatus.Won,
            "second chunk settled too"
        );
    }

    function _deliverChunkFor(address winner, uint16 pageIndex, uint16 totalPages) internal {
        address[] memory winners = new address[](1);
        winners[0] = winner;
        _deliver(
            OUTBE_CHAIN_ID,
            outbeIca,
            address(target),
            BridgeMsgCodec.encodeIntexAuctionWinners(DAY, pageIndex, totalPages, 500_000, 1e6, winners, 0, 0)
        );
    }
}
