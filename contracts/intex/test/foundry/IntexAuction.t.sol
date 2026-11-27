// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ReferenceCurrencyPriceLib} from "./helpers/ReferenceCurrencyPriceLib.sol";
import {Test} from "forge-std/Test.sol";
import {Vm} from "forge-std/Vm.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {EscrowTimes} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {MockAuctionEscrow} from "@test-mocks/MockAuctionEscrow.sol";

contract AuctionTest is Test {
    uint16 internal constant ISSUANCE_CCY = 840;
    uint16 internal constant REFERENCE_CCY = 840;

    IntexAuction auction;
    MockAuctionEscrow escrow;

    address admin = address(1);
    address bridger = address(2);
    address constant INTEX = address(0x1E7E);

    // Private keys for signing
    uint256 iba1PrivateKey = 0x100;
    uint256 iba2PrivateKey = 0x200;
    uint256 outsiderPrivateKey = 0x999;

    address iba1; // Derived from iba1PrivateKey
    address iba2; // Derived from iba2PrivateKey
    address outsider; // Derived from outsiderPrivateKey

    // EIP-712 typehash mirrors `IntexAuction.REVEAL_BID_TYPEHASH`.
    bytes32 internal constant REVEAL_BID_TYPEHASH = keccak256(
        "RevealBid(uint32 emissionDay,address bidder,uint16 quantity,uint32 bidRate,uint16 issuanceCurrency,uint16 referenceCurrency)"
    );

    uint32 internal constant SCALE_1E6 = BridgeMsgCodec.SCALE_1E6;
    // WCOEN escrow: the per-Intex escrow basis is PROMIS_LOAD_MINOR (constant COEN), so the lock is
    // `qty * PROMIS_LOAD_MINOR * rate / 1e6`. ENTRY_PRICE feeds only floor/call now.
    uint128 internal constant PROMIS_LOAD_MINOR = 100_000 * 1e6;
    uint64 internal constant ENTRY_PRICE = 1e6;

    // Schedule offsets relative to the auction-start timestamp.
    uint32 constant COMMIT_OFFSET = 100;
    uint32 constant REVEAL_OFFSET = 200;
    uint32 constant ISSUANCE_OFFSET = 300;

    function setUp() public {
        // Derive addresses from private keys
        iba1 = vm.addr(iba1PrivateKey);
        iba2 = vm.addr(iba2PrivateKey);
        outsider = vm.addr(outsiderPrivateKey);

        escrow = new MockAuctionEscrow();
        auction = DeployProxy.intexAuction(admin, bridger, address(escrow), INTEX);
    }

    // --- Helpers ---

    /// @dev Build a valid, strictly-increasing, in-the-future schedule.
    function _schedule() internal view returns (IIntexAuction.AuctionSchedule memory) {
        return IIntexAuction.AuctionSchedule({
            commitEnd: uint32(block.timestamp + COMMIT_OFFSET),
            revealEnd: uint32(block.timestamp + REVEAL_OFFSET),
            issuanceEnd: uint32(block.timestamp + ISSUANCE_OFFSET)
        });
    }

    /// @dev Build auction params with the given minimum bid rate and entry price.
    function _paramsEntry(uint32 minBidRate, uint64 entryPrice, uint16 minBidQuantity)
        internal
        pure
        returns (IIntexAuction.AuctionParams memory)
    {
        return IIntexAuction.AuctionParams({
            promisLoadMinor: PROMIS_LOAD_MINOR,
            minBidRate: minBidRate,
            prices: ReferenceCurrencyPriceLib.onePriced(840, entryPrice, 200),
            callTrigger: IIntexAuction.IntexCallTrigger({callWindow: 0, callThreshold: 0, callNoticePeriod: 0}),
            minBidQuantity: minBidQuantity,
            bidBondMinor: 0
        });
    }

    /// @dev Build auction params at the canonical entry price.
    function _params(uint32 minBidRate, uint16 minBidQuantity)
        internal
        pure
        returns (IIntexAuction.AuctionParams memory)
    {
        return _paramsEntry(minBidRate, ENTRY_PRICE, minBidQuantity);
    }

    /// @dev Create and start a live-day auction as the relayer. The schedule is anchored to
    ///      the current `block.timestamp` via `_schedule()`.
    function _start(uint32 emissionDay, uint32 minBidRate, uint16 minBidQuantity) internal {
        vm.prank(bridger);
        auction.startAuction(emissionDay, _schedule(), _params(minBidRate, minBidQuantity));
    }

    /// @dev Warp past `commitEnd` so the timestamp-computed stage is `RevealingBids`.
    function _enterRevealStage(uint32, uint256 startTs) internal {
        vm.warp(startTs + COMMIT_OFFSET + 1);
    }

    /// @dev Build an EIP-712 reveal signature against the deployed `auction` instance and the
    ///      current `block.chainid`.
    function _createSignature(uint32 emissionDay, address sender, uint16 qty, uint32 rate, uint256 privateKey)
        internal
        view
        returns (bytes memory)
    {
        bytes32 structHash = keccak256(
            abi.encode(REVEAL_BID_TYPEHASH, emissionDay, sender, qty, rate, ISSUANCE_CCY, REFERENCE_CCY)
        );
        bytes32 domainSeparator = keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"),
                keccak256(bytes("IntexAuction")),
                keccak256(bytes("1")),
                block.chainid,
                address(auction)
            )
        );
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", domainSeparator, structHash));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(privateKey, digest);
        return abi.encodePacked(r, s, v);
    }

    function _commit(uint32 emissionDay, address bidder, uint16 qty, uint32 rate, uint256 privateKey) internal {
        bytes memory signature = _createSignature(emissionDay, bidder, qty, rate, privateKey);
        bytes32 commitHash = keccak256(signature);
        vm.prank(bidder);
        auction.commitIntexAuctionBid(emissionDay, commitHash);
    }

    function _reveal(uint32 emissionDay, address bidder, uint16 qty, uint32 rate, uint256 privateKey) internal {
        bytes memory signature = _createSignature(emissionDay, bidder, qty, rate, privateKey);
        vm.prank(bidder);
        auction.revealIntexAuctionBid(
            emissionDay, qty, rate, ISSUANCE_CCY, REFERENCE_CCY, uint64(block.chainid), signature
        );
    }

    /// @dev A day with no result fails at `commitEnd` plus the refund delay, when its first bid
    ///      can be taken back, and then takes no result.
    function test_ADayWithoutAResultFailsWhenItsFirstRefundOpens() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250116;
        _start(emissionDay, 50, 1);
        uint256 failAt = startTs + COMMIT_OFFSET + EscrowTimes.UNFINALIZED_RETURN_DELAY;

        vm.warp(failAt - 1);
        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.AwaitingResult));

        vm.warp(failAt);
        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.Failed));
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexAuction.StageRequired.selector,
                IIntexAuction.AuctionStage.AwaitingResult,
                IIntexAuction.AuctionStage.Failed
            )
        );
        vm.prank(bridger);
        auction.completeAuction(emissionDay, 0, 0, 0, 0, 0);
    }

    /// @dev One event for each fact of the day: one start, and one end that is either the completion or the cancel.
    function _assertDayEvents(Vm.Log[] memory logs, uint256 clearingStarts, uint256 completions, uint256 cancels)
        internal
        view
    {
        uint256[4] memory counts;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter != address(auction)) continue;
            bytes32 topic = logs[i].topics[0];
            if (topic == IIntexAuction.IntexAuctionStarted.selector) ++counts[0];
            if (topic == IIntexAuction.IntexAuctionClearingStarted.selector) ++counts[1];
            if (topic == IIntexAuction.IntexAuctionCompleted.selector) ++counts[2];
            if (topic == IIntexAuction.IntexAuctionCanceled.selector) ++counts[3];
        }
        assertEq(counts[0], 1, "one start");
        assertEq(counts[1], clearingStarts, "clearing starts");
        assertEq(counts[2], completions, "completions");
        assertEq(counts[3], cancels, "cancels");
    }

    function test_Lifecycle_FullFlow() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250115; // yyyymmdd format
        uint32 floor = 50;
        uint16 bidMinimumQuantity = 1;
        vm.recordLogs();
        _start(emissionDay, floor, bidMinimumQuantity);

        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.CommittingBids));

        IIntexAuction.AuctionData memory info = auction.getAuctionInfo(emissionDay);
        assertEq(info.params.minBidRate, floor);
        assertEq(info.params.promisLoadMinor, PROMIS_LOAD_MINOR);

        _commit(emissionDay, iba1, 30, 80, iba1PrivateKey);
        _commit(emissionDay, iba2, 40, 70, iba2PrivateKey);

        assertTrue(auction.committedIntexAuctionBidsByHash(emissionDay, iba1) != bytes32(0));
        assertTrue(auction.committedIntexAuctionBidsByHash(emissionDay, iba2) != bytes32(0));

        _enterRevealStage(emissionDay, startTs);
        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.RevealingBids));

        _reveal(emissionDay, iba1, 30, 80, iba1PrivateKey);
        _reveal(emissionDay, iba2, 40, 70, iba2PrivateKey);

        // The auction hands the escrow the day's basis; the escrow sizes the lock.
        assertEq(uint256(escrow.lockedFunds(emissionDay, iba1)), PROMIS_LOAD_MINOR);
        assertEq(uint256(escrow.lockedFunds(emissionDay, iba2)), PROMIS_LOAD_MINOR);

        (, IIntexAuction.SubmittedIntexAuctionBidData[] memory bids) = auction.getAuctionDetails(emissionDay);
        (, uint32 revealedBidsCount) = auction.auctionRunningCounts(emissionDay);
        assertEq(revealedBidsCount, 2);
        assertEq(bids.length, 2);

        // Past-revealEnd clearing signal. The schedule already closed reveal, and the signal
        // only advances the stage.
        vm.warp(startTs + REVEAL_OFFSET + 1);
        vm.prank(bridger);
        auction.startAuctionClearing(emissionDay);
        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.AwaitingResult));

        vm.expectEmit(true, false, false, true);
        emit IIntexAuction.IntexAuctionCompleted(emissionDay, 0, 2, 100, 75, 0);
        vm.prank(bridger);
        auction.completeAuction(emissionDay, 0, 2, 100, 75, 0);

        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.Completed));
        IIntexAuction.AuctionData memory fin = auction.getAuctionInfo(emissionDay);
        assertEq(fin.result.clearingRate, 75);
        assertEq(fin.result.issuedUnits, 100);
        assertEq(fin.result.wonBidsCount, 2);
        assertEq(fin.params.promisLoadMinor, PROMIS_LOAD_MINOR);
        // issuedPromisLoadMinor is derived on-chain as issuedUnits * promisLoadMinor.
        assertEq(fin.result.issuedPromisLoadMinor, uint128(100) * PROMIS_LOAD_MINOR);
        _assertDayEvents(vm.getRecordedLogs(), 1, 1, 0);
    }

    function test_CommitCancel_And_Reverts() public {
        uint32 emissionDay = 20250116;
        _start(emissionDay, 10, 1);

        // commit + cancel
        _commit(emissionDay, iba1, 5, 11, iba1PrivateKey);
        assertTrue(auction.committedIntexAuctionBidsByHash(emissionDay, iba1) != bytes32(0));
        vm.prank(iba1);
        auction.cancelIntexAuctionBid(emissionDay);
        assertEq(auction.committedIntexAuctionBidsByHash(emissionDay, iba1), bytes32(0));

        // cancel when no commit
        vm.expectRevert(IIntexAuction.IntexAuctionBidNotFound.selector);
        vm.prank(iba1);
        auction.cancelIntexAuctionBid(emissionDay);
    }

    function test_CommitBid_RevertsZeroCommitHash() public {
        // B5.8: a degenerate zero commitHash must not occupy a bid slot.
        uint32 emissionDay = 20250117;
        _start(emissionDay, 10, 1);

        vm.expectRevert(IIntexAuction.InvalidCommitHash.selector);
        vm.prank(iba1);
        auction.commitIntexAuctionBid(emissionDay, bytes32(0));
    }

    function test_CommitBid_RevertsAtCommitEnd() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250120;
        _start(emissionDay, 10, 1);

        // Window is [start, commitEnd): at commitEnd the computed stage is already RevealingBids.
        vm.warp(startTs + COMMIT_OFFSET);

        bytes memory signature = _createSignature(emissionDay, iba1, 5, 11, iba1PrivateKey);
        bytes32 commitHash = keccak256(signature);
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexAuction.StageRequired.selector,
                IIntexAuction.AuctionStage.CommittingBids,
                IIntexAuction.AuctionStage.RevealingBids
            )
        );
        vm.prank(iba1);
        auction.commitIntexAuctionBid(emissionDay, commitHash);
    }

    function test_CancelCommit_RevertsAtCommitEnd() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250121;
        _start(emissionDay, 10, 1);

        _commit(emissionDay, iba1, 5, 11, iba1PrivateKey);
        assertTrue(auction.committedIntexAuctionBidsByHash(emissionDay, iba1) != bytes32(0));

        // Past commitEnd a sealed commit must not be withdrawable.
        vm.warp(startTs + COMMIT_OFFSET);

        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexAuction.StageRequired.selector,
                IIntexAuction.AuctionStage.CommittingBids,
                IIntexAuction.AuctionStage.RevealingBids
            )
        );
        vm.prank(iba1);
        auction.cancelIntexAuctionBid(emissionDay);

        // The commit survives the rejected cancel.
        assertTrue(auction.committedIntexAuctionBidsByHash(emissionDay, iba1) != bytes32(0));
    }

    function test_CommitBid_AllowedAtLastSecondBeforeCommitEnd() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250122;
        _start(emissionDay, 10, 1);

        // commitEnd - 1 is the last valid second of the window.
        vm.warp(uint256(startTs + COMMIT_OFFSET) - 1);
        _commit(emissionDay, iba1, 5, 11, iba1PrivateKey);
        assertTrue(auction.committedIntexAuctionBidsByHash(emissionDay, iba1) != bytes32(0));
    }

    function test_Reveal_Reverts() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250117;
        _start(emissionDay, 100, 1);

        _commit(emissionDay, iba1, 10, 120, iba1PrivateKey);

        _enterRevealStage(emissionDay, startTs);

        // wrong rate -> hash mismatch
        vm.expectRevert(IIntexAuction.RevealHashMismatch.selector);
        _reveal(emissionDay, iba1, 10, 999, iba1PrivateKey);

        // ok reveal
        _reveal(emissionDay, iba1, 10, 120, iba1PrivateKey);

        // double reveal
        vm.expectRevert(IIntexAuction.IntexAuctionBidAlreadyRevealed.selector);
        _reveal(emissionDay, iba1, 10, 120, iba1PrivateKey);
    }

    function test_Reveal_BelowFloor() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250118;
        _start(emissionDay, 100, 1);

        // Commit a below-floor bid during commit stage
        _commit(emissionDay, iba1, 10, 90, iba1PrivateKey);

        _enterRevealStage(emissionDay, startTs);

        // below floor - try to reveal the below-floor bid
        vm.expectRevert(IIntexAuction.IntexAuctionBidBelowMinBidRate.selector);
        _reveal(emissionDay, iba1, 10, 90, iba1PrivateKey);
    }

    function test_Reveal_BelowMinQuantity() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250140;
        // minBidRate = 100, minBidQuantity = 5
        _start(emissionDay, 100, 5);

        // Commit a below-minimum-quantity bid (qty 4 < 5) at an above-floor rate.
        _commit(emissionDay, iba1, 4, 120, iba1PrivateKey);

        _enterRevealStage(emissionDay, startTs);

        // Quantity below the published minimum is rejected at reveal.
        vm.expectRevert(IIntexAuction.IntexAuctionBidBelowMinBidQuantity.selector);
        _reveal(emissionDay, iba1, 4, 120, iba1PrivateKey);
    }

    function test_Reveal_AtMinQuantity_Succeeds() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250141;
        _start(emissionDay, 100, 5);

        // Quantity exactly at the minimum is accepted (boundary is inclusive).
        _commit(emissionDay, iba1, 5, 120, iba1PrivateKey);
        _enterRevealStage(emissionDay, startTs);
        _reveal(emissionDay, iba1, 5, 120, iba1PrivateKey);

        assertTrue(auction.revealedIntexAuctionBidsByBidder(emissionDay, iba1));
    }

    function test_Reveal_AboveMaxRate() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250142;
        _start(emissionDay, 1, 1);

        uint32 rate = SCALE_1E6 + 1;
        _commit(emissionDay, iba1, 10, rate, iba1PrivateKey);
        _enterRevealStage(emissionDay, startTs);

        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.IntexAuctionBidRateAboveMax.selector, rate));
        _reveal(emissionDay, iba1, 10, rate, iba1PrivateKey);
    }

    function test_Reveal_AtMaxRate_Succeeds() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250143;
        _start(emissionDay, 1, 1);

        uint16 qty = 3;
        uint32 rate = SCALE_1E6;
        _commit(emissionDay, iba1, qty, rate, iba1PrivateKey);
        _enterRevealStage(emissionDay, startTs);
        _reveal(emissionDay, iba1, qty, rate, iba1PrivateKey);

        assertTrue(auction.revealedIntexAuctionBidsByBidder(emissionDay, iba1));
        assertEq(uint256(escrow.lockedFunds(emissionDay, iba1)), PROMIS_LOAD_MINOR);
    }

    function test_Reveal_RevertsNonIsoIssuanceCurrency() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250160;
        _start(emissionDay, 1, 1);

        _commit(emissionDay, iba1, 3, 120, iba1PrivateKey);
        _enterRevealStage(emissionDay, startTs);
        bytes memory signature = _createSignature(emissionDay, iba1, 3, 120, iba1PrivateKey);

        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.InvalidIssuanceCurrency.selector, uint16(975)));
        vm.prank(iba1);
        auction.revealIntexAuctionBid(emissionDay, 3, 120, 975, REFERENCE_CCY, uint64(block.chainid), signature);
    }

    function test_Reveal_WithoutCommit() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250119;
        _start(emissionDay, 100, 1);

        _enterRevealStage(emissionDay, startTs);

        // reveal without commit
        vm.expectRevert(IIntexAuction.IntexAuctionBidNotFound.selector);
        _reveal(emissionDay, iba2, 5, 120, iba2PrivateKey);
    }

    function test_CancelAuction_UnknownDay_StaysCancelled() public {
        uint32 emissionDay = 20250120;
        vm.prank(bridger);
        auction.cancelAuction(emissionDay, 1);
        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.Canceled));

        vm.expectRevert(IIntexAuction.AuctionCanceled.selector);
        vm.prank(bridger);
        auction.startAuction(emissionDay, _schedule(), _params(1, 1));

        vm.expectRevert(IIntexAuction.AuctionNotFound.selector);
        vm.prank(iba1);
        auction.commitIntexAuctionBid(emissionDay, keccak256("commit"));
    }

    function test_CancelAuction_CommitStage_ClosesCommits() public {
        uint32 emissionDay = 20250140;
        vm.recordLogs();
        _start(emissionDay, 1, 1);
        vm.prank(bridger);
        auction.cancelAuction(emissionDay, 1);
        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.Canceled));
        _assertDayEvents(vm.getRecordedLogs(), 0, 0, 1);

        bytes memory signature = _createSignature(emissionDay, iba1, 1, 1, iba1PrivateKey);
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexAuction.StageRequired.selector,
                IIntexAuction.AuctionStage.CommittingBids,
                IIntexAuction.AuctionStage.Canceled
            )
        );
        vm.prank(iba1);
        auction.commitIntexAuctionBid(emissionDay, keccak256(signature));
    }

    function test_CancelAuction_RevealStage_ClosesReveals() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250141;
        _start(emissionDay, 1, 1);
        _commit(emissionDay, iba1, 5, 20, iba1PrivateKey);
        _enterRevealStage(emissionDay, startTs);
        vm.prank(bridger);
        auction.cancelAuction(emissionDay, 1);

        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexAuction.StageRequired.selector,
                IIntexAuction.AuctionStage.RevealingBids,
                IIntexAuction.AuctionStage.Canceled
            )
        );
        _reveal(emissionDay, iba1, 5, 20, iba1PrivateKey);
    }

    function test_CancelAuction_RevertWhen_DayHasAResultOrIsCancelled() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250142;
        _start(emissionDay, 1, 1);
        _enterRevealStage(emissionDay, startTs);
        vm.warp(startTs + REVEAL_OFFSET + 1);
        vm.startPrank(bridger);
        auction.startAuctionClearing(emissionDay);
        auction.completeAuction(emissionDay, 0, 0, 0, 0, 0);
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexAuction.StageRequired.selector,
                IIntexAuction.AuctionStage.AwaitingResult,
                IIntexAuction.AuctionStage.Completed
            )
        );
        auction.cancelAuction(emissionDay, 1);

        uint32 other = emissionDay + 1;
        auction.cancelAuction(other, 1);
        vm.expectRevert(IIntexAuction.AuctionCanceled.selector);
        auction.cancelAuction(other, 1);
        vm.stopPrank();
    }

    function test_CancelAuction_RevertWhen_NotTargetRouter() public {
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.UnauthorizedCaller.selector, iba1));
        vm.prank(iba1);
        auction.cancelAuction(20250143, 1);
    }

    function test_Views_BySeriesId() public {
        uint32 emissionDay = 20250121;
        _start(emissionDay, 5, 1);

        IIntexAuction.AuctionData memory a = auction.getAuctionInfo(emissionDay);
        assertEq(a.params.minBidRate, 5);
        assertEq(uint8(a.dayState), uint8(IIntexAuction.DayState.Live));

        (IIntexAuction.AuctionData memory b, IIntexAuction.SubmittedIntexAuctionBidData[] memory bids) =
            auction.getAuctionDetails(emissionDay);
        assertEq(b.params.promisLoadMinor, PROMIS_LOAD_MINOR);
        assertEq(bids.length, 0);
    }

    function test_Stage_TimingTransitions() public {
        uint32 emissionDay = 20250122;
        _start(emissionDay, 1, 1);

        IIntexAuction.AuctionData memory d = auction.getAuctionInfo(emissionDay);
        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.CommittingBids));

        // No bridge message: the stage flips exactly at the stored schedule boundaries.
        vm.warp(d.schedule.commitEnd - 1);
        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.CommittingBids));
        vm.warp(d.schedule.commitEnd);
        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.RevealingBids));

        vm.warp(d.schedule.revealEnd - 1);
        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.RevealingBids));
        vm.warp(d.schedule.revealEnd);
        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.AwaitingResult));
    }

    // --- Access Control Tests ---
    function test_Constructor_SetsTheLinks() public view {
        assertEq(auction.targetRouter(), bridger);
        assertEq(address(auction.escrow()), address(escrow));
        assertEq(address(auction.intexContract()), INTEX);
        assertEq(auction.owner(), admin);
    }

    function test_Constructor_RejectsZeroLinks() public {
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.ZeroAddress.selector, "targetRouter"));
        new IntexAuction(address(0), address(escrow), INTEX);
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.ZeroAddress.selector, "escrow"));
        new IntexAuction(bridger, address(0), INTEX);
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.ZeroAddress.selector, "intexNFT1155"));
        new IntexAuction(bridger, address(escrow), address(0));
    }

    function test_RevealBid_FreesCommitSlot() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250230;
        _start(emissionDay, 50, 1);
        _commit(emissionDay, iba1, 30, 80, iba1PrivateKey);
        assertTrue(auction.committedIntexAuctionBidsByHash(emissionDay, iba1) != bytes32(0));

        _enterRevealStage(emissionDay, startTs);
        _reveal(emissionDay, iba1, 30, 80, iba1PrivateKey);

        assertEq(auction.committedIntexAuctionBidsByHash(emissionDay, iba1), bytes32(0));
    }

    function test_OnlyTheTargetRouter_AuctionStart() public {
        uint32 emissionDay = 20250123;

        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.UnauthorizedCaller.selector, admin));
        vm.prank(admin);
        auction.startAuction(emissionDay, _schedule(), _params(10, 1));

        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.UnauthorizedCaller.selector, iba1));
        vm.prank(iba1);
        auction.startAuction(emissionDay, _schedule(), _params(10, 1));
    }

    function test_OnlyTheTargetRouter_StartAwaitingResult() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250125;
        _start(emissionDay, 10, 1);
        _enterRevealStage(emissionDay, startTs);

        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.UnauthorizedCaller.selector, admin));
        vm.prank(admin);
        auction.startAuctionClearing(emissionDay);

        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.UnauthorizedCaller.selector, iba1));
        vm.prank(iba1);
        auction.startAuctionClearing(emissionDay);
    }

    function test_OnlyTheTargetRouter_ExecuteAuctionClearing() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250126;
        _start(emissionDay, 10, 1);
        _enterRevealStage(emissionDay, startTs);
        vm.prank(bridger);
        auction.startAuctionClearing(emissionDay);

        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.UnauthorizedCaller.selector, admin));
        vm.prank(admin);
        auction.completeAuction(emissionDay, 0, 1, 100, 75, 0);

        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.UnauthorizedCaller.selector, iba1));
        vm.prank(iba1);
        auction.completeAuction(emissionDay, 0, 1, 100, 75, 0);
    }

    // --- Clearing sanity-floor ---

    function test_ExecuteAuctionClearing_RevertsWonBidsExceedRevealed() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250127;
        uint32 floor = 50;
        _start(emissionDay, floor, 1);
        _commit(emissionDay, iba1, 30, 80, iba1PrivateKey);
        _commit(emissionDay, iba2, 40, 70, iba2PrivateKey);
        _enterRevealStage(emissionDay, startTs);
        _reveal(emissionDay, iba1, 30, 80, iba1PrivateKey);
        _reveal(emissionDay, iba2, 40, 70, iba2PrivateKey);
        vm.warp(startTs + REVEAL_OFFSET + 1);
        vm.prank(bridger);
        auction.startAuctionClearing(emissionDay);

        // 2 bids revealed; a clearing claiming 3 winners is rejected.
        vm.expectRevert(
            abi.encodeWithSelector(IIntexAuction.WonIntexAuctionBidsExceedRevealed.selector, uint32(3), uint32(2))
        );
        vm.prank(bridger);
        auction.completeAuction(emissionDay, 0, 3, 100, 75, 0);
    }

    function test_ExecuteAuctionClearing_RevertsPromisOverflow() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250199;
        uint32 floor = 50;

        IIntexAuction.AuctionParams memory params = _params(floor, 1);
        params.promisLoadMinor = type(uint128).max; // ceiling so the clearing product overflows uint128
        vm.prank(bridger);
        auction.startAuction(emissionDay, _schedule(), params);

        _enterRevealStage(emissionDay, startTs);
        vm.warp(startTs + REVEAL_OFFSET + 1);
        vm.prank(bridger);
        auction.startAuctionClearing(emissionDay);

        vm.expectRevert(
            abi.encodeWithSelector(IIntexAuction.IssuedPromisOverflow.selector, type(uint32).max, type(uint128).max)
        );
        vm.prank(bridger);
        auction.completeAuction(emissionDay, 0, 0, type(uint32).max, floor, 0);
    }

    function test_ExecuteAuctionClearing_RevertsClearingRateBelowMin() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250128;
        uint32 floor = 50;
        _start(emissionDay, floor, 1);
        _commit(emissionDay, iba1, 30, 80, iba1PrivateKey);
        _enterRevealStage(emissionDay, startTs);
        _reveal(emissionDay, iba1, 30, 80, iba1PrivateKey);
        vm.warp(startTs + REVEAL_OFFSET + 1);
        vm.prank(bridger);
        auction.startAuctionClearing(emissionDay);

        // Clearing rate below the configured minimum is rejected.
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.ClearingRateBelowMin.selector, uint64(floor - 1), floor));
        vm.prank(bridger);
        auction.completeAuction(emissionDay, 0, 1, 100, floor - 1, 0);
    }

    /// @dev No-sale auction: Desis floors the clearing rate at `minBidRate`, so a clearing
    ///      with zero winners still carries a non-zero rate. The auction must accept this as a
    ///      valid result. The real invariant is `clearingRate >= minBidRate`, NOT the
    ///      (incorrect) `clearingRate == 0 <=> winners == 0`. This test guards against
    ///      re-introducing that wrong rule.
    function test_ExecuteAuctionClearing_NoSale_ZeroWinnersAtFloor() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250144;
        uint32 floor = 50;
        _start(emissionDay, floor, 1);
        _commit(emissionDay, iba1, 30, 80, iba1PrivateKey);
        _enterRevealStage(emissionDay, startTs);
        _reveal(emissionDay, iba1, 30, 80, iba1PrivateKey);
        vm.warp(startTs + REVEAL_OFFSET + 1);
        vm.prank(bridger);
        auction.startAuctionClearing(emissionDay);

        // Zero winners, zero issued, clearing rate at the floor (non-zero): a valid No-sale result.
        vm.expectEmit(true, false, false, true);
        emit IIntexAuction.IntexAuctionCompleted(emissionDay, 0, 0, 0, floor, 0);
        vm.prank(bridger);
        auction.completeAuction(emissionDay, 0, 0, 0, floor, 0);

        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.Completed));
        IIntexAuction.AuctionData memory fin = auction.getAuctionInfo(emissionDay);
        assertEq(fin.result.wonBidsCount, 0);
        assertEq(fin.result.issuedUnits, 0);
        assertEq(fin.result.clearingRate, floor);
        assertEq(fin.result.issuedPromisLoadMinor, 0);
    }

    /// @dev No-sale with no supply: even when `minBidRate > 0`, the clearing rate can be 0
    ///      (nothing was allocated because supply was exhausted/zero). It must still complete and
    ///      NOT revert `ZeroValue`/`ClearingRateBelowMin`. The full refund uses
    ///      REFUND_INSTRUCTIONS, and nothing is issued. The `cleared` flag drives the Completed
    ///      stage.
    function test_ExecuteAuctionClearing_NoSale_ZeroRate() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250145;
        uint32 floor = 50; // minBidRate > 0
        vm.recordLogs();
        _start(emissionDay, floor, 1);
        _commit(emissionDay, iba1, 30, 80, iba1PrivateKey);
        _enterRevealStage(emissionDay, startTs);
        _reveal(emissionDay, iba1, 30, 80, iba1PrivateKey);
        vm.warp(startTs + REVEAL_OFFSET + 1);
        vm.prank(bridger);
        auction.startAuctionClearing(emissionDay);

        // issued=0, clearingRate=0, won=0 - accepted despite floor=50 (no supply was available).
        vm.expectEmit(true, false, false, true);
        emit IIntexAuction.IntexAuctionCompleted(emissionDay, 0, 0, 0, 0, 0);
        vm.prank(bridger);
        auction.completeAuction(emissionDay, 0, 0, 0, 0, 0);

        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.Completed));
        IIntexAuction.AuctionData memory fin = auction.getAuctionInfo(emissionDay);
        assertEq(fin.result.clearingRate, 0);
        assertEq(fin.result.issuedUnits, 0);
        assertEq(fin.result.wonBidsCount, 0);
        assertEq(fin.result.issuedPromisLoadMinor, 0);
        _assertDayEvents(vm.getRecordedLogs(), 1, 1, 0);

        // Idempotent: re-clearing a completed auction is rejected on the stage gate.
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexAuction.StageRequired.selector,
                IIntexAuction.AuctionStage.AwaitingResult,
                IIntexAuction.AuctionStage.Completed
            )
        );
        vm.prank(bridger);
        auction.completeAuction(emissionDay, 0, 0, 0, 0, 0);
    }

    // --- Validation Tests ---
    function test_AuctionStart_Validation() public {
        uint32 emissionDay = 20250127;

        // Schedule with commitEnd in the past.
        IIntexAuction.AuctionSchedule memory pastSchedule = IIntexAuction.AuctionSchedule({
            commitEnd: uint32(block.timestamp),
            revealEnd: uint32(block.timestamp + 200),
            issuanceEnd: uint32(block.timestamp + 300)
        });
        vm.expectRevert(IIntexAuction.InvalidSchedule.selector);
        vm.prank(bridger);
        auction.startAuction(emissionDay, pastSchedule, _params(10, 1));

        // Schedule not strictly increasing (revealEnd <= commitEnd).
        IIntexAuction.AuctionSchedule memory nonIncreasing = IIntexAuction.AuctionSchedule({
            commitEnd: uint32(block.timestamp + 200),
            revealEnd: uint32(block.timestamp + 200),
            issuanceEnd: uint32(block.timestamp + 300)
        });
        vm.expectRevert(IIntexAuction.InvalidSchedule.selector);
        vm.prank(bridger);
        auction.startAuction(emissionDay, nonIncreasing, _params(10, 1));

        // Valid start succeeds.
        vm.prank(bridger);
        auction.startAuction(emissionDay, _schedule(), _params(10, 1));

        // AuctionAlreadyExists
        vm.expectRevert(IIntexAuction.AuctionAlreadyExists.selector);
        vm.prank(bridger);
        auction.startAuction(emissionDay, _schedule(), _params(10, 1));
    }

    function test_ExecuteAuctionClearing_Validation() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250128;
        _start(emissionDay, 10, 1);
        _enterRevealStage(emissionDay, startTs);
        // Reach the Issuance stage (time-derived: now >= revealEnd).
        vm.warp(startTs + REVEAL_OFFSET + 1);
        vm.prank(bridger);
        auction.startAuctionClearing(emissionDay);

        // Zero clearingRate
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.ZeroValue.selector, "clearingRate"));
        vm.prank(bridger);
        auction.completeAuction(emissionDay, 0, 1, 100, 0, 0);
    }

    function test_RevealBid_Validation() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250129;
        _start(emissionDay, 10, 1);
        _commit(emissionDay, iba1, 10, 20, iba1PrivateKey);
        _enterRevealStage(emissionDay, startTs);

        // Zero quantity
        bytes memory sig = _createSignature(emissionDay, iba1, 0, 20, iba1PrivateKey);
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.ZeroValue.selector, "quantity/bidRate"));
        vm.prank(iba1);
        auction.revealIntexAuctionBid(emissionDay, 0, 20, ISSUANCE_CCY, REFERENCE_CCY, uint64(block.chainid), sig);

        // Zero bidRate
        sig = _createSignature(emissionDay, iba1, 10, 0, iba1PrivateKey);
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.ZeroValue.selector, "quantity/bidRate"));
        vm.prank(iba1);
        auction.revealIntexAuctionBid(emissionDay, 10, 0, ISSUANCE_CCY, REFERENCE_CCY, uint64(block.chainid), sig);

        // Wrong chainId - caller's chainId param does not match block.chainid -> WrongChain.
        uint64 wrongChainId = 999;
        sig = _createSignature(emissionDay, iba1, 10, 20, iba1PrivateKey);
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.WrongChain.selector, block.chainid, uint256(wrongChainId)));
        vm.prank(iba1);
        auction.revealIntexAuctionBid(emissionDay, 10, 20, ISSUANCE_CCY, REFERENCE_CCY, wrongChainId, sig);
    }

    function test_StartAwaitingResult_AlreadyClearing() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250130;
        _start(emissionDay, 10, 1);
        _enterRevealStage(emissionDay, startTs);
        vm.prank(bridger);
        auction.startAuctionClearing(emissionDay);

        // A second clearing start is idempotent and emits no second clearing event.
        vm.recordLogs();
        vm.prank(bridger);
        auction.startAuctionClearing(emissionDay);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i; i < logs.length; ++i) {
            assertTrue(logs[i].topics[0] != IIntexAuction.IntexAuctionClearingStarted.selector, "one clearing event");
        }
    }

    function test_clearing_start_after_the_reveal_end_emits_its_event_once() public {
        uint32 emissionDay = 20250131;
        _start(emissionDay, 10, 1);
        vm.warp(auction.getAuctionInfo(emissionDay).schedule.revealEnd + 1);
        assertEq(uint8(auction.auctionStage(emissionDay)), uint8(IIntexAuction.AuctionStage.AwaitingResult));

        vm.expectEmit(true, true, true, true, address(auction));
        emit IIntexAuction.IntexAuctionClearingStarted(emissionDay);
        vm.prank(bridger);
        auction.startAuctionClearing(emissionDay);

        vm.recordLogs();
        vm.prank(bridger);
        auction.startAuctionClearing(emissionDay);
        assertEq(vm.getRecordedLogs().length, 0, "a repeated clearing start emits nothing");
    }

    function test_ViewFunctions_NotFound() public {
        uint32 nonExistentSeries = 99999999; // non-existent series

        vm.expectRevert(IIntexAuction.AuctionNotFound.selector);
        auction.getAuctionInfo(nonExistentSeries);

        vm.expectRevert(IIntexAuction.AuctionNotFound.selector);
        auction.getAuctionDetails(nonExistentSeries);

        vm.expectRevert(IIntexAuction.AuctionNotFound.selector);
        auction.auctionStage(nonExistentSeries);
    }

    function test_RevealBid_WrongSigner() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250131;
        _start(emissionDay, 10, 1);
        _commit(emissionDay, iba1, 10, 20, iba1PrivateKey);
        _enterRevealStage(emissionDay, startTs);

        // Try to reveal with signature from iba1 but call from iba2
        // This will fail with IntexAuctionBidNotFound because iba2 has no commit
        bytes memory sig = _createSignature(emissionDay, iba1, 10, 20, iba1PrivateKey);
        vm.expectRevert(IIntexAuction.IntexAuctionBidNotFound.selector);
        vm.prank(iba2);
        auction.revealIntexAuctionBid(emissionDay, 10, 20, ISSUANCE_CCY, REFERENCE_CCY, uint64(block.chainid), sig);
    }

    /// @dev Reentrancy probe: arm the escrow mock to call back into `revealIntexAuctionBid` during
    ///      `lockIntexAuctionAsset`. With `nonReentrant` in place the inner call reverts with
    ///      `ReentrancyGuardReentrantCall`, which propagates and unwinds all state.
    ///      Without `nonReentrant` on `revealIntexAuctionBid`, this test fails (reentry succeeds, and the
    ///      attacker double-records the bid). It is a true red->green test of the guard.
    function test_RevealBid_reentrancyBlocked() public {
        uint256 startTs = block.timestamp;
        uint32 emissionDay = 20250201;
        _start(emissionDay, 10, 1);
        _commit(emissionDay, iba1, 10, 20, iba1PrivateKey);
        _enterRevealStage(emissionDay, startTs);

        bytes memory sig = _createSignature(emissionDay, iba1, 10, 20, iba1PrivateKey);
        bytes memory reentrantCall = abi.encodeCall(
            IIntexAuction.revealIntexAuctionBid,
            (emissionDay, 10, 20, ISSUANCE_CCY, REFERENCE_CCY, uint64(block.chainid), sig)
        );
        escrow.armReentry(auction, reentrantCall);

        bytes4 reentrancyGuard = bytes4(keccak256("ReentrancyGuardReentrantCall()"));
        vm.expectRevert(reentrancyGuard);
        vm.prank(iba1);
        auction.revealIntexAuctionBid(emissionDay, 10, 20, ISSUANCE_CCY, REFERENCE_CCY, uint64(block.chainid), sig);

        // Tx unwound: no reveal recorded, no bid pushed.
        assertFalse(auction.revealedIntexAuctionBidsByBidder(emissionDay, iba1));
        (, IIntexAuction.SubmittedIntexAuctionBidData[] memory bids) = auction.getAuctionDetails(emissionDay);
        assertEq(bids.length, 0);
        (, uint32 revealedBidsCount) = auction.auctionRunningCounts(emissionDay);
        assertEq(revealedBidsCount, 0);
    }
}
