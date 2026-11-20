// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AuctionResultLib} from "../helpers/AuctionResultLib.sol";

import {MarkBatchLib} from "../helpers/MarkBatchLib.sol";
import {ReferenceCurrencyPriceLib} from "../helpers/ReferenceCurrencyPriceLib.sol";
import {Vm} from "forge-std/Vm.sol";
import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {ITargetRouter} from "@contracts/target/interfaces/ITargetRouter.sol";
import {OriginRouter} from "@contracts/origin/OriginRouter.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {IntexNFT1155Bridge} from "@contracts/shared/IntexNFT1155Bridge.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";
import {IssuanceBatchLib} from "../helpers/IssuanceBatch.sol";

/// @dev End-to-end traversal of the five `TargetRouter` inbound handlers. These handlers
///      previously had only codec-level round-trip coverage. Each test hand-builds a
///      `BridgeMsgCodec` packet and drives `lzReceive` from the endpoint address. Then it asserts
///      the downstream side-effect on the wired contract. This proves the full
///      receiveMessage -> dispatchInbound -> _handleX -> X path under the current
///      fail-don't-drop model.
contract TargetRouterInboundHandlersTest is CrossChainTest {
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant OUTBE_CHAIN_ID = 2;

    uint32 internal constant EMISSION_DAY = 20250101; // yyyymmdd - the auction day (root)
    bytes14 internal constant SERIES_ID = "20250101-USD-U";
    uint32 internal constant ISSUED_UNITS = 100;
    uint128 internal constant PROMIS_LOAD_MINOR = 1e6;
    uint64 internal constant ENTRY_PRICE = 100e6;
    uint16 internal constant REFERENCE_CURRENCY = 840;

    TargetRouter internal bnbRouter;
    OriginRouter internal outbeRouter;
    IntexAuction internal auction;
    IntexNFT1155 internal intex;
    EscrowAdapter internal escrow;
    IntexNFT1155Bridge internal nftBridge;
    MockTheCompact internal compact;
    MockWCOEN internal paymentToken;

    address internal admin = address(this);
    address internal bidder = address(0xB1);

    function setUp() public {
        _setUpBridge();

        intex = DeployProxy.intexNFT1155(admin, admin);
        auction = DeployProxy.intexAuction(admin, admin);

        bnbRouter = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        outbeRouter = DeployProxy.originRouter(address(bridge), admin);
        nftBridge = DeployProxy.intexNFT1155Bridge(address(intex), address(bridge), admin);

        escrow = DeployProxy.escrowAdapter(admin, admin);
        compact = new MockTheCompact();
        paymentToken = new MockWCOEN();
        escrow.setDependencies(admin, address(compact), address(paymentToken));
        compact.setResetPeriodSeconds(0);

        bnbRouter.registerRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(outbeRouter)));
        outbeRouter.registerRemoteMessenger(BNB_CHAIN_ID, _interop(BNB_CHAIN_ID, address(bnbRouter)));

        bnbRouter.setDependencies(address(auction), address(intex), address(escrow));

        // The router drives auction lifecycle (RELAYER_ROLE), creates IntexNFT1155 series
        // (ROUTER_ROLE), and finalizes escrow (RELAYER_ROLE). Each downstream contract gates
        // mutations on its own role which only the wired router should hold.
        auction.grantRole(auction.RELAYER_ROLE(), address(bnbRouter));
        intex.grantRole(intex.ROUTER_ROLE(), address(bnbRouter));
        escrow.grantRole(escrow.RELAYER_ROLE(), address(bnbRouter));

        // Bidder funds + approve so EscrowAdapter.lockIntexAuctionAsset works in the REFUND_INSTRUCTIONS test.
        paymentToken.mint(bidder, 10_000e6);
        vm.prank(bidder);
        paymentToken.approve(address(escrow), type(uint256).max);
    }

    // --- _handleAuctionResult: clearing data is persisted on the auction ---
    function test_handleAuctionResult_persistsClearingOnAuction() public {
        _seedAuction();
        // Push the auction into Issuance stage so completeAuction is accepted.
        vm.warp(block.timestamp + 1 days);
        vm.prank(address(bnbRouter));
        auction.startAuctionClearing(EMISSION_DAY);

        uint64 clearingPrice = 100e6;
        bytes memory packet =
            BridgeMsgCodec.encodeIntexAuctionResult(EMISSION_DAY, AuctionResultLib.day(ISSUED_UNITS, clearingPrice, 0));
        _deliver(packet);

        IIntexAuction.IntexAuctionResult memory result = auction.getAuctionInfo(EMISSION_DAY).result;
        assertEq(result.issuedUnits, ISSUED_UNITS, "issuedUnits persisted");
        assertEq(result.clearingRate, clearingPrice, "clearingPrice persisted");
    }

    // --- _handleAuctionStageCollectingBids: relays a BIDS_TOTALS marker, exactly once ---
    function test_handleAuctionStageCollectingBids_emitsBidsDone() public {
        _seedAuction();
        vm.warp(block.timestamp + 1 days);

        // No revealed bids: one empty BIDS_BATCH plus a BIDS_TOTALS(totalPages=1, totalBids=0).
        vm.expectEmit(false, true, false, true, address(bnbRouter));
        emit ITargetRouter.IntexAuctionBidsTotalsSent(bytes32(0), EMISSION_DAY, 1, 0);
        _deliver(BridgeMsgCodec.encodeIntexAuctionClearingStart(EMISSION_DAY));
    }

    function test_handleAuctionStageCollectingBids_idempotentOnRedelivery() public {
        _seedAuction();
        vm.warp(block.timestamp + 1 days);
        bytes memory packet = BridgeMsgCodec.encodeIntexAuctionClearingStart(EMISSION_DAY);
        _deliver(packet); // first CLEARING relays

        // A redelivered CLEARING must not relay the day again.
        vm.recordLogs();
        _deliver(packet);
        assertEq(_countLogs(ITargetRouter.IntexAuctionBidsTotalsSent.selector), 0, "no re-relay");
    }

    // --- Source: only Outbe drives a target router ---
    function test_aMessageFromAnotherChainIsRefused() public {
        uint32 otherChain = 77;
        address otherPeer = makeAddr("otherPeer");
        bnbRouter.registerRemoteMessenger(otherChain, _interop(otherChain, otherPeer));

        vm.expectRevert(abi.encodeWithSelector(ITargetRouter.UnexpectedSourceChain.selector, otherChain));
        _deliver(
            otherChain, otherPeer, address(bnbRouter), BridgeMsgCodec.encodeIntexAuctionClearingStart(EMISSION_DAY)
        );
    }

    // --- _handleMarkCalled: the mark is a pure state flip and never touches balances ---
    function test_handleMarkCalled_leavesBalancesUntouched() public {
        uint32 local = uint32(block.chainid);
        TargetRouter localRouter = DeployProxy.targetRouter(address(bridge), admin, local);
        address localOrigin = makeAddr("localOrigin");
        localRouter.registerRemoteMessenger(local, _interop(local, localOrigin));
        localRouter.setDependencies(address(auction), address(intex), address(escrow));
        intex.grantRole(intex.ROUTER_ROLE(), address(localRouter));

        _deliver(local, localOrigin, address(localRouter), _issuancePacket());
        intex.grantRole(intex.AUCTION_ROLE(), admin);
        intex.issueIntex(bidder, 5, SERIES_ID);

        uint256 tokenId = intex.issuedTokenId(SERIES_ID);
        assertEq(intex.balanceOf(bidder, tokenId), 5);

        _deliver(
            local,
            localOrigin,
            address(localRouter),
            BridgeMsgCodec.encodeIntexSeriesCallNotice(
                EMISSION_DAY, uint32(block.timestamp), MarkBatchLib.one(SERIES_ID)
            )
        );

        assertEq(uint8(intex.readData(SERIES_ID).state), uint8(IIntexNFT1155.IntexState.Called), "series Called");
        assertEq(intex.balanceOf(bidder, tokenId), 5, "balances untouched by the mark");
    }

    function _countLogs(bytes32 sig) internal returns (uint256 n) {
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].topics[0] == sig) n++;
        }
    }

    // --- _handleIssuanceInstructions: createSeries on local IntexNFT1155, no mint ---
    function test_handleIssuanceInstructions_createsSeriesOnly() public {
        BridgeMsgCodec.IntexSeriesPayload memory payload = BridgeMsgCodec.IntexSeriesPayload({
            seriesId: SERIES_ID,
            emissionDay: EMISSION_DAY,
            issuedAt: uint32(block.timestamp),
            issuedUnits: ISSUED_UNITS,
            promisLoadMinor: PROMIS_LOAD_MINOR,
            entryPriceMinor: ENTRY_PRICE,
            callNoticePeriod: 0,
            issuanceCurrency: 840,
            referenceCurrency: REFERENCE_CURRENCY,
            callWindow: 30,
            callThreshold: 5,
            callPriceMinor: 25e6,
            transitMargin: 1 hours
        });
        bytes memory packet = BridgeMsgCodec.encodeIntexSeries(EMISSION_DAY, 0, 1, IssuanceBatchLib.one(payload));
        _deliver(packet);

        assertTrue(intex.seriesExists(SERIES_ID), "series created");
        assertEq(intex.totalSupply(intex.issuedTokenId(SERIES_ID)), 0, "winners issue their own units");
    }

    function _issuancePacket() internal view returns (bytes memory) {
        return BridgeMsgCodec.encodeIntexSeries(
            EMISSION_DAY,
            0,
            1,
            IssuanceBatchLib.one(
                BridgeMsgCodec.IntexSeriesPayload({
                    seriesId: SERIES_ID,
                    emissionDay: EMISSION_DAY,
                    issuedAt: uint32(block.timestamp),
                    issuedUnits: ISSUED_UNITS,
                    promisLoadMinor: PROMIS_LOAD_MINOR,
                    entryPriceMinor: ENTRY_PRICE,
                    callNoticePeriod: 0,
                    issuanceCurrency: 840,
                    referenceCurrency: REFERENCE_CURRENCY,
                    callWindow: 30,
                    callThreshold: 5,
                    callPriceMinor: 25e6,
                    transitMargin: 1 hours
                })
            )
        );
    }

    /// @dev A day in flight finalizes on the escrow that took its locks, so the venue stays put.
    function test_wire_keepsTheVenueWhileTheEscrowHoldsLocks() public {
        paymentToken.mint(bidder, 1e18);
        vm.prank(admin);
        escrow.lockIntexAuctionAsset(EMISSION_DAY, bidder, 1e6, 1_000_000, 1);

        vm.expectRevert(ITargetRouter.EscrowHasLiveLocks.selector);
        bnbRouter.setDependencies(address(auction), address(intex), makeAddr("escrow2"));
        vm.expectRevert(ITargetRouter.EscrowHasLiveLocks.selector);
        bnbRouter.setDependencies(makeAddr("auction2"), address(intex), address(escrow));
        vm.expectRevert(ITargetRouter.EscrowHasLiveLocks.selector);
        bnbRouter.setDependencies(address(auction), makeAddr("intex2"), address(escrow));

        bnbRouter.setDependencies(address(auction), address(intex), address(escrow));
    }

    // --- _handleRefundInstructions: forwarded to EscrowAdapter.chargeIntexAuctionAsset ---
    // The winner's lock flips to Won.
    function test_handleRefundInstructions_finalizesEscrow() public {
        // Lock funds for the bidder so chargeIntexAuctionAsset's per-winner branch can land.
        paymentToken.mint(bidder, 1e18);
        vm.startPrank(admin);
        escrow.grantRole(escrow.AUCTION_ROLE(), admin);
        escrow.setProceedsRecipient(address(bnbRouter));
        escrow.lockIntexAuctionAsset(EMISSION_DAY, bidder, 1e6, 1_000_000, 1);
        vm.stopPrank();

        address[] memory winners = new address[](1);
        winners[0] = bidder;

        bytes memory packet = BridgeMsgCodec.encodeIntexAuctionWinners(EMISSION_DAY, 0, 1, 500_000, 1e6, winners, 0, 0);
        _deliver(packet);

        IEscrowAdapter.IntexAuctionBidLock memory lock = escrow.getIntexAuctionBidLock(EMISSION_DAY, bidder);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.Won), "lock advanced to Won via handler");
    }

    // --- _handleAuctionCancel: the day ends as a result of zero units does ---

    function _cancel() internal {
        _deliver(BridgeMsgCodec.encodeIntexAuctionCancel(EMISSION_DAY, 1));
    }

    function _lock() internal {
        paymentToken.mint(bidder, 1e18);
        vm.prank(admin);
        escrow.lockIntexAuctionAsset(EMISSION_DAY, bidder, 1e6, 1_000_000, 1);
    }

    function test_cancel_before_the_result_in_the_commit_stage() public {
        _seedAuction();
        _cancel();
        assertEq(uint8(auction.auctionStage(EMISSION_DAY)), uint8(IIntexAuction.AuctionStage.Canceled));
        (, bool finalized,) = escrow.auctionStatus(EMISSION_DAY);
        assertTrue(finalized, "the escrow closes the day");
    }

    function test_cancel_before_the_result_in_the_reveal_stage_returns_each_lock_at_once() public {
        _seedAuction();
        vm.warp(block.timestamp + 1 days);
        _lock();
        _cancel();

        (uint128 amount, uint32 claimableAt) = escrow.returnableAmount(EMISSION_DAY, bidder);
        assertGt(amount, 0, "the lock is owed");
        assertEq(claimableAt, 0, "at once, with no wait for the unfinalized delay");
    }

    function test_cancel_before_the_result_twice_is_a_duplicate() public {
        _seedAuction();
        _cancel();
        vm.expectEmit(false, true, false, true, address(bnbRouter));
        emit ITargetRouter.MessageIgnored(bytes32(0), OUTBE_CHAIN_ID, 1);
        _cancel();
    }

    function test_cancel_after_the_result_has_no_effect() public {
        _seedAuction();
        vm.warp(block.timestamp + 2 days);
        vm.startPrank(address(bnbRouter));
        auction.startAuctionClearing(EMISSION_DAY);
        auction.completeAuction(EMISSION_DAY, 0, 0, 0, 0, 0);
        vm.stopPrank();

        vm.expectEmit(false, true, false, true, address(bnbRouter));
        emit ITargetRouter.MessageIgnored(bytes32(0), OUTBE_CHAIN_ID, 2);
        _cancel();
        assertEq(uint8(auction.auctionStage(EMISSION_DAY)), uint8(IIntexAuction.AuctionStage.Completed));
    }

    function test_cancel_while_awaiting_the_result_makes_the_result_and_winners_obsolete() public {
        _seedAuction();
        vm.warp(block.timestamp + 1 days);
        _lock();
        vm.warp(block.timestamp + 1 days);
        vm.prank(address(bnbRouter));
        auction.startAuctionClearing(EMISSION_DAY);
        _cancel();
        (uint128 amount, uint32 claimableAt) = escrow.returnableAmount(EMISSION_DAY, bidder);
        assertGt(amount, 0, "the lock is owed");
        assertEq(claimableAt, 0, "at once");

        vm.expectEmit(false, true, false, true, address(bnbRouter));
        emit ITargetRouter.MessageIgnored(bytes32(0), OUTBE_CHAIN_ID, 2);
        _deliver(BridgeMsgCodec.encodeIntexAuctionResult(EMISSION_DAY, AuctionResultLib.day(0, 0, 0)));
        address[] memory winners = new address[](1);
        winners[0] = bidder;
        vm.expectEmit(false, true, false, true, address(bnbRouter));
        emit ITargetRouter.MessageIgnored(bytes32(0), OUTBE_CHAIN_ID, 2);
        _deliver(BridgeMsgCodec.encodeIntexAuctionWinners(EMISSION_DAY, 0, 1, 500_000, 1e6, winners, 0, 0));
        assertEq(uint8(auction.auctionStage(EMISSION_DAY)), uint8(IIntexAuction.AuctionStage.Canceled));
    }

    function test_cancel_before_the_start_makes_the_start_a_conflict() public {
        _cancel();
        assertEq(uint8(auction.auctionStage(EMISSION_DAY)), uint8(IIntexAuction.AuctionStage.Canceled));

        vm.expectEmit(false, true, false, true, address(bnbRouter));
        emit ITargetRouter.MessageIgnored(bytes32(0), OUTBE_CHAIN_ID, 3);
        _deliver(
            BridgeMsgCodec.encodeIntexAuctionStart(
                EMISSION_DAY,
                uint32(block.timestamp + 1 days),
                uint32(block.timestamp + 2 days),
                uint32(block.timestamp + 3 days),
                PROMIS_LOAD_MINOR,
                60e6,
                ReferenceCurrencyPriceLib.one(840, ENTRY_PRICE, ENTRY_PRICE),
                0,
                0,
                0,
                1,
                0
            )
        );
        assertEq(uint8(auction.auctionStage(EMISSION_DAY)), uint8(IIntexAuction.AuctionStage.Canceled));
    }

    // --- Helpers ---

    function _seedAuction() internal {
        // The schedule must be strictly increasing and commitEnd > block.timestamp.
        IIntexAuction.AuctionSchedule memory schedule = IIntexAuction.AuctionSchedule({
            commitEnd: uint32(block.timestamp + 1 days),
            revealEnd: uint32(block.timestamp + 2 days),
            issuanceEnd: uint32(block.timestamp + 3 days)
        });
        IIntexAuction.AuctionParams memory params = IIntexAuction.AuctionParams({
            promisLoadMinor: PROMIS_LOAD_MINOR,
            minBidRate: 60e6,
            prices: ReferenceCurrencyPriceLib.onePriced(840, ENTRY_PRICE, ENTRY_PRICE),
            callTrigger: IIntexAuction.IntexCallTrigger({callWindow: 0, callThreshold: 0, callNoticePeriod: 0}),
            minBidQuantity: 1,
            bidBondMinor: 0
        });
        vm.prank(address(bnbRouter));
        auction.startAuction(EMISSION_DAY, schedule, params);
    }

    function _seedSeriesOnIntex() internal {
        intex.createSeries(CreateSeriesLib.params(EMISSION_DAY, ISSUED_UNITS, 0));
    }

    function _deliver(bytes memory packet) internal {
        _deliver(OUTBE_CHAIN_ID, address(outbeRouter), address(bnbRouter), packet);
    }
}
