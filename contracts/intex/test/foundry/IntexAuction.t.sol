// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {MockAuctionEscrow} from "@test-mocks/MockAuctionEscrow.sol";

contract AuctionTest is Test {
    IntexAuction auction;
    MockAuctionEscrow escrow;

    address admin = address(1);
    address bridger = address(2);

    // Private keys for signing
    uint256 iba1PrivateKey = 0x100;
    uint256 iba2PrivateKey = 0x200;
    uint256 outsiderPrivateKey = 0x999;

    address iba1; // Derived from iba1PrivateKey
    address iba2; // Derived from iba2PrivateKey
    address outsider; // Derived from outsiderPrivateKey

    // EIP-712 typehash mirrors `IntexAuction.REVEAL_BID_TYPEHASH`.
    bytes32 internal constant REVEAL_BID_TYPEHASH =
        keccak256("RevealBid(uint32 seriesId,address bidder,uint16 quantity,uint32 bidRate)");

    uint32 internal constant RATE_SCALE = 1_000_000;
    // Strike basis: `_strike(ENTRY_PRICE, PROMIS_LOAD_MINOR) == RATE_SCALE`, so the escrow lock
    // reduces to `qty * rate` — keeping the rate test values small and exact.
    uint128 internal constant PROMIS_LOAD_MINOR = 100_000 * 1e18;
    uint64 internal constant ENTRY_PRICE = 1e13;

    // Schedule offsets relative to the auction-start timestamp.
    uint32 constant COMMIT_OFFSET = 100;
    uint32 constant REVEAL_OFFSET = 200;
    uint32 constant ISSUANCE_OFFSET = 300;

    function setUp() public {
        // Derive addresses from private keys
        iba1 = vm.addr(iba1PrivateKey);
        iba2 = vm.addr(iba2PrivateKey);
        outsider = vm.addr(outsiderPrivateKey);

        auction = DeployProxy.intexAuction(admin, bridger);
        escrow = new MockAuctionEscrow();

        vm.startPrank(admin);
        auction.grantRole(auction.RELAYER_ROLE(), bridger);
        auction.wire(address(escrow));
        vm.stopPrank();
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
    function _paramsEntry(uint32 minIntexBidRate, uint64 entryPrice, uint16 minIntexBidQuantity)
        internal
        pure
        returns (IIntexAuction.AuctionParams memory)
    {
        return IIntexAuction.AuctionParams({
            promisLoadMinor: PROMIS_LOAD_MINOR,
            minIntexBidRate: minIntexBidRate,
            entryPrice: entryPrice,
            floorPriceMinor: 100,
            callPriceMinor: 200,
            callTrigger: IIntexAuction.IntexCallTrigger({windowDays: 0, thresholdDays: 0, intexCallPeriod: 0}),
            minIntexBidQuantity: minIntexBidQuantity
        });
    }

    /// @dev Build auction params at the canonical entry price (strike == RATE_SCALE).
    function _params(uint32 minIntexBidRate, uint16 minIntexBidQuantity)
        internal
        pure
        returns (IIntexAuction.AuctionParams memory)
    {
        return _paramsEntry(minIntexBidRate, ENTRY_PRICE, minIntexBidQuantity);
    }

    /// @dev Create and start an auction as the relayer. The schedule is anchored to the
    ///      current `block.timestamp` via `_schedule()`.
    function _start(uint32 seriesId, uint32 minIntexBidRate, uint16 minIntexBidQuantity) internal {
        vm.prank(bridger);
        auction.auctionStart(seriesId, _schedule(), _params(minIntexBidRate, minIntexBidQuantity));
    }

    /// @dev Send the green-day signal and warp past `commitEnd` so the computed stage is
    ///      actually `RevealingBids` (stage is derived from the schedule + worldwide-day state).
    function _enterRevealStage(uint32 seriesId, uint256 startTs) internal {
        vm.prank(bridger);
        auction.startRevealingBidsStage(seriesId, true);
        vm.warp(startTs + COMMIT_OFFSET + 1);
    }

    /// @dev Build an EIP-712 reveal signature against the deployed `auction` instance and the
    ///      current `block.chainid`.
    function _createSignature(uint32 seriesId, address sender, uint16 qty, uint32 rate, uint256 privateKey)
        internal
        view
        returns (bytes memory)
    {
        bytes32 structHash = keccak256(abi.encode(REVEAL_BID_TYPEHASH, seriesId, sender, qty, rate));
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

    function _commit(uint32 seriesId, address bidder, uint16 qty, uint32 rate, uint256 privateKey) internal {
        bytes memory signature = _createSignature(seriesId, bidder, qty, rate, privateKey);
        bytes32 commitHash = keccak256(signature);
        vm.prank(bidder);
        auction.commitBid(seriesId, commitHash);
    }

    function _reveal(uint32 seriesId, address bidder, uint16 qty, uint32 rate, uint256 privateKey) internal {
        bytes memory signature = _createSignature(seriesId, bidder, qty, rate, privateKey);
        vm.prank(bidder);
        auction.revealBid(seriesId, qty, rate, uint64(block.chainid), signature);
    }

    function test_Lifecycle_FullFlow() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250115; // yyyymmdd format
        uint32 floor = 50;
        uint16 bidMinimumQuantity = 1;
        _start(seriesId, floor, bidMinimumQuantity);

        assertEq(uint8(auction.getAuctionStage(seriesId)), uint8(IIntexAuction.AuctionStage.CommittingBids));

        IIntexAuction.AuctionData memory info = auction.getAuctionInfo(seriesId);
        assertEq(info.params.minIntexBidRate, floor);
        assertEq(info.params.promisLoadMinor, PROMIS_LOAD_MINOR);

        _commit(seriesId, iba1, 30, 80, iba1PrivateKey);
        _commit(seriesId, iba2, 40, 70, iba2PrivateKey);

        assertTrue(auction.committedBidsByHash(seriesId, iba1) != bytes32(0));
        assertTrue(auction.committedBidsByHash(seriesId, iba2) != bytes32(0));

        _enterRevealStage(seriesId, startTs);
        assertEq(uint8(auction.getAuctionStage(seriesId)), uint8(IIntexAuction.AuctionStage.RevealingBids));

        _reveal(seriesId, iba1, 30, 80, iba1PrivateKey);
        _reveal(seriesId, iba2, 40, 70, iba2PrivateKey);

        // Strike == RATE_SCALE here, so the escrow lock is exactly `qty * rate`.
        assertEq(escrow.lockedFunds(seriesId, iba1), uint64(30) * 80);
        assertEq(escrow.lockedFunds(seriesId, iba2), uint64(40) * 70);

        (, IIntexAuction.SubmittedBidData[] memory bids) = auction.getAuctionDetails(seriesId);
        (, uint32 revealedBidsCount) = auction.auctionRunningCounts(seriesId);
        assertEq(revealedBidsCount, 2);
        assertEq(bids.length, 2);

        // Past-revealEnd clearing signal: schedule already closed reveal, signal only advances stage.
        vm.warp(startTs + REVEAL_OFFSET + 1);
        vm.prank(bridger);
        auction.startClearingStage(seriesId);
        assertEq(uint8(auction.getAuctionStage(seriesId)), uint8(IIntexAuction.AuctionStage.Issuance));

        vm.expectEmit(true, false, false, true);
        emit IIntexAuction.AuctionClearingExecuted(seriesId, 75, 100);
        vm.prank(bridger);
        auction.executeAuctionClearing(seriesId, 100, 75, 2);

        assertEq(uint8(auction.getAuctionStage(seriesId)), uint8(IIntexAuction.AuctionStage.Completed));
        IIntexAuction.AuctionData memory fin = auction.getAuctionInfo(seriesId);
        assertEq(fin.result.auctionClearingRate, 75);
        assertEq(fin.result.issuedIntexCount, 100);
        assertEq(fin.result.wonBidsCount, 2);
        assertEq(fin.params.promisLoadMinor, PROMIS_LOAD_MINOR);
        // issuedIntexLoadedPromis is derived on-chain as issuedIntexCount * promisLoadMinor.
        assertEq(fin.result.issuedIntexLoadedPromis, uint128(100) * PROMIS_LOAD_MINOR);
    }

    function test_CommitCancel_And_Reverts() public {
        uint32 seriesId = 20250116;
        _start(seriesId, 10, 1);

        // commit + cancel
        _commit(seriesId, iba1, 5, 11, iba1PrivateKey);
        assertTrue(auction.committedBidsByHash(seriesId, iba1) != bytes32(0));
        vm.prank(iba1);
        auction.cancelCommit(seriesId);
        assertEq(auction.committedBidsByHash(seriesId, iba1), bytes32(0));

        // cancel when no commit
        vm.expectRevert(IIntexAuction.BidNotFound.selector);
        vm.prank(iba1);
        auction.cancelCommit(seriesId);
    }

    function test_CommitBid_RevertsZeroCommitHash() public {
        // B5.8: a degenerate zero commitHash must not occupy a bid slot.
        uint32 seriesId = 20250117;
        _start(seriesId, 10, 1);

        vm.expectRevert(IIntexAuction.InvalidCommitHash.selector);
        vm.prank(iba1);
        auction.commitBid(seriesId, bytes32(0));
    }

    function test_CommitBid_RevertsAfterCommitEnd_WhileUnknown() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250120;
        _start(seriesId, 10, 1);

        // No green-day signal: worldwideDayState stays Unknown, so the derived stage stays
        // CommittingBids even past commitEnd. The explicit deadline gate must still reject.
        uint32 commitEnd = uint32(startTs + COMMIT_OFFSET);
        vm.warp(uint256(commitEnd)); // window is [start, commitEnd) → commitEnd itself is closed

        bytes memory signature = _createSignature(seriesId, iba1, 5, 11, iba1PrivateKey);
        bytes32 commitHash = keccak256(signature);
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.CommitWindowClosed.selector, commitEnd, commitEnd));
        vm.prank(iba1);
        auction.commitBid(seriesId, commitHash);
    }

    function test_CancelCommit_RevertsAfterCommitEnd_WhileUnknown() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250121;
        _start(seriesId, 10, 1);

        _commit(seriesId, iba1, 5, 11, iba1PrivateKey);
        assertTrue(auction.committedBidsByHash(seriesId, iba1) != bytes32(0));

        // Past commitEnd, signal still Unknown: a sealed commit must not be withdrawable.
        uint32 commitEnd = uint32(startTs + COMMIT_OFFSET);
        vm.warp(uint256(commitEnd));

        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.CommitWindowClosed.selector, commitEnd, commitEnd));
        vm.prank(iba1);
        auction.cancelCommit(seriesId);

        // The commit survives the rejected cancel.
        assertTrue(auction.committedBidsByHash(seriesId, iba1) != bytes32(0));
    }

    function test_CommitBid_AllowedAtLastSecondBeforeCommitEnd() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250122;
        _start(seriesId, 10, 1);

        // commitEnd - 1 is the last valid second of the window.
        vm.warp(uint256(startTs + COMMIT_OFFSET) - 1);
        _commit(seriesId, iba1, 5, 11, iba1PrivateKey);
        assertTrue(auction.committedBidsByHash(seriesId, iba1) != bytes32(0));
    }

    function test_Reveal_Reverts() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250117;
        _start(seriesId, 100, 1);

        _commit(seriesId, iba1, 10, 120, iba1PrivateKey);

        _enterRevealStage(seriesId, startTs);

        // wrong rate -> hash mismatch
        vm.expectRevert(IIntexAuction.RevealHashMismatch.selector);
        _reveal(seriesId, iba1, 10, 999, iba1PrivateKey);

        // ok reveal
        _reveal(seriesId, iba1, 10, 120, iba1PrivateKey);

        // double reveal
        vm.expectRevert(IIntexAuction.BidAlreadyRevealed.selector);
        _reveal(seriesId, iba1, 10, 120, iba1PrivateKey);
    }

    function test_Reveal_BelowFloor() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250118;
        _start(seriesId, 100, 1);

        // Commit a below-floor bid during commit stage
        _commit(seriesId, iba1, 10, 90, iba1PrivateKey);

        _enterRevealStage(seriesId, startTs);

        // below floor - try to reveal the below-floor bid
        vm.expectRevert(IIntexAuction.BidBelowMinIntexBidRate.selector);
        _reveal(seriesId, iba1, 10, 90, iba1PrivateKey);
    }

    function test_Reveal_BelowMinQuantity() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250140;
        // minIntexBidRate = 100, minIntexBidQuantity = 5
        _start(seriesId, 100, 5);

        // Commit a below-minimum-quantity bid (qty 4 < 5) at an above-floor rate.
        _commit(seriesId, iba1, 4, 120, iba1PrivateKey);

        _enterRevealStage(seriesId, startTs);

        // Quantity below the published minimum is rejected at reveal.
        vm.expectRevert(IIntexAuction.BidBelowMinIntexBidQuantity.selector);
        _reveal(seriesId, iba1, 4, 120, iba1PrivateKey);
    }

    function test_Reveal_AtMinQuantity_Succeeds() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250141;
        _start(seriesId, 100, 5);

        // Quantity exactly at the minimum is accepted (boundary is inclusive).
        _commit(seriesId, iba1, 5, 120, iba1PrivateKey);
        _enterRevealStage(seriesId, startTs);
        _reveal(seriesId, iba1, 5, 120, iba1PrivateKey);

        assertTrue(auction.revealedBidsByBidder(seriesId, iba1));
    }

    function test_Reveal_AmountOverflow() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250142;
        // Max entry price → max strike. With max qty and rate the lock product overflows uint64.
        vm.prank(bridger);
        auction.auctionStart(seriesId, _schedule(), _paramsEntry(1, type(uint64).max, 1));

        uint16 qty = type(uint16).max;
        uint32 rate = type(uint32).max;
        _commit(seriesId, iba1, qty, rate, iba1PrivateKey);
        _enterRevealStage(seriesId, startTs);

        // Overflow surfaces as a typed error, not a bare arithmetic Panic(0x11).
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.BidAmountOverflow.selector, qty, rate));
        _reveal(seriesId, iba1, qty, rate, iba1PrivateKey);
    }

    function test_Reveal_MaxAmount_NoOverflow_Succeeds() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250143;
        _start(seriesId, 1, 1);

        // Strike == RATE_SCALE → lock = qty * rate; the largest single-unit rate still fits uint64.
        uint16 qty = 1;
        uint32 rate = type(uint32).max;
        _commit(seriesId, iba1, qty, rate, iba1PrivateKey);
        _enterRevealStage(seriesId, startTs);
        _reveal(seriesId, iba1, qty, rate, iba1PrivateKey);

        assertTrue(auction.revealedBidsByBidder(seriesId, iba1));
        assertEq(escrow.lockedFunds(seriesId, iba1), uint64(rate));
    }

    function test_Reveal_WithoutCommit() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250119;
        _start(seriesId, 100, 1);

        _enterRevealStage(seriesId, startTs);

        // reveal without commit
        vm.expectRevert(IIntexAuction.BidNotFound.selector);
        _reveal(seriesId, iba2, 5, 120, iba2PrivateKey);
    }

    function test_RedDay_CancelsAuction() public {
        uint32 seriesId = 20250120;
        _start(seriesId, 1, 1);

        vm.prank(bridger);
        auction.startRevealingBidsStage(seriesId, false);
        assertEq(uint8(auction.getAuctionStage(seriesId)), uint8(IIntexAuction.AuctionStage.Cancelled));

        // any action should fail stage requirement
        bytes memory signature = _createSignature(seriesId, iba1, 1, 1, iba1PrivateKey);
        bytes32 commitHash = keccak256(signature);
        vm.expectRevert();
        vm.prank(iba1);
        auction.commitBid(seriesId, commitHash);
    }

    function test_Views_BySeriesId() public {
        uint32 seriesId = 20250121;
        _start(seriesId, 5, 1);

        IIntexAuction.AuctionData memory a = auction.getAuctionInfo(seriesId);
        assertEq(a.params.minIntexBidRate, 5);
        assertEq(uint8(a.worldwideDayState), uint8(IIntexAuction.WorldwideDayState.Unknown));

        (IIntexAuction.AuctionData memory b, IIntexAuction.SubmittedBidData[] memory bids) =
            auction.getAuctionDetails(seriesId);
        assertEq(b.params.promisLoadMinor, PROMIS_LOAD_MINOR);
        assertEq(bids.length, 0);
    }

    function test_Stage_TimingTransitions() public {
        uint32 seriesId = 20250122;
        _start(seriesId, 1, 1);

        IIntexAuction.AuctionData memory d = auction.getAuctionInfo(seriesId);
        assertEq(uint8(auction.getAuctionStage(seriesId)), uint8(IIntexAuction.AuctionStage.CommittingBids));

        // Late green-day signal: schedule already closed commit window, signal only flips state.
        vm.warp(d.schedule.commitEnd - 1);
        assertEq(uint8(auction.getAuctionStage(seriesId)), uint8(IIntexAuction.AuctionStage.CommittingBids));
        vm.warp(d.schedule.commitEnd + 1);
        vm.prank(bridger);
        auction.startRevealingBidsStage(seriesId, true);
        assertEq(uint8(auction.getAuctionStage(seriesId)), uint8(IIntexAuction.AuctionStage.RevealingBids));

        vm.warp(d.schedule.revealEnd + 1);
        assertEq(uint8(auction.getAuctionStage(seriesId)), uint8(IIntexAuction.AuctionStage.Issuance));
    }

    // --- Access Control Tests ---
    function test_AccessControl_Wire() public {
        vm.expectRevert();
        vm.prank(iba1);
        auction.wire(address(0xBEEF));

        vm.expectRevert();
        vm.prank(bridger);
        auction.wire(address(0xBEEF));
    }

    function test_AccessControl_AuctionStart() public {
        uint32 seriesId = 20250123;

        vm.expectRevert();
        vm.prank(admin);
        auction.auctionStart(seriesId, _schedule(), _params(10, 1));

        vm.expectRevert();
        vm.prank(iba1);
        auction.auctionStart(seriesId, _schedule(), _params(10, 1));
    }

    function test_AccessControl_StartRevealingBidsStage() public {
        uint32 seriesId = 20250124;
        _start(seriesId, 10, 1);

        vm.expectRevert();
        vm.prank(admin);
        auction.startRevealingBidsStage(seriesId, true);

        vm.expectRevert();
        vm.prank(iba1);
        auction.startRevealingBidsStage(seriesId, true);
    }

    function test_AccessControl_StartClearingStage() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250125;
        _start(seriesId, 10, 1);
        _enterRevealStage(seriesId, startTs);

        vm.expectRevert();
        vm.prank(admin);
        auction.startClearingStage(seriesId);

        vm.expectRevert();
        vm.prank(iba1);
        auction.startClearingStage(seriesId);
    }

    function test_AccessControl_ExecuteAuctionClearing() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250126;
        _start(seriesId, 10, 1);
        _enterRevealStage(seriesId, startTs);
        vm.prank(bridger);
        auction.startClearingStage(seriesId);

        vm.expectRevert();
        vm.prank(admin);
        auction.executeAuctionClearing(seriesId, 100, 75, 1);

        vm.expectRevert();
        vm.prank(iba1);
        auction.executeAuctionClearing(seriesId, 100, 75, 1);
    }

    // --- Clearing sanity-floor ---

    function test_ExecuteAuctionClearing_RevertsWonBidsExceedRevealed() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250127;
        uint32 floor = 50;
        _start(seriesId, floor, 1);
        _commit(seriesId, iba1, 30, 80, iba1PrivateKey);
        _commit(seriesId, iba2, 40, 70, iba2PrivateKey);
        _enterRevealStage(seriesId, startTs);
        _reveal(seriesId, iba1, 30, 80, iba1PrivateKey);
        _reveal(seriesId, iba2, 40, 70, iba2PrivateKey);
        vm.warp(startTs + REVEAL_OFFSET + 1);
        vm.prank(bridger);
        auction.startClearingStage(seriesId);

        // 2 bids revealed; a clearing claiming 3 winners is rejected.
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.WonBidsExceedRevealed.selector, uint32(3), uint32(2)));
        vm.prank(bridger);
        auction.executeAuctionClearing(seriesId, 100, 75, 3);
    }

    function test_ExecuteAuctionClearing_RevertsClearingRateBelowMin() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250128;
        uint32 floor = 50;
        _start(seriesId, floor, 1);
        _commit(seriesId, iba1, 30, 80, iba1PrivateKey);
        _enterRevealStage(seriesId, startTs);
        _reveal(seriesId, iba1, 30, 80, iba1PrivateKey);
        vm.warp(startTs + REVEAL_OFFSET + 1);
        vm.prank(bridger);
        auction.startClearingStage(seriesId);

        // Clearing rate below the configured minimum is rejected.
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.ClearingRateBelowMin.selector, uint64(floor - 1), floor));
        vm.prank(bridger);
        auction.executeAuctionClearing(seriesId, 100, floor - 1, 1);
    }

    /// @dev No-sale auction: Desis floors the clearing rate at `minIntexBidRate`, so a clearing
    ///      with zero winners still carries a non-zero rate. This must be accepted as a valid
    ///      result — the real invariant is `clearingRate >= minIntexBidRate`, NOT the (incorrect)
    ///      `clearingRate == 0 ⇔ winners == 0`. Guards against re-introducing that wrong rule.
    function test_ExecuteAuctionClearing_NoSale_ZeroWinnersAtFloor() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250144;
        uint32 floor = 50;
        _start(seriesId, floor, 1);
        _commit(seriesId, iba1, 30, 80, iba1PrivateKey);
        _enterRevealStage(seriesId, startTs);
        _reveal(seriesId, iba1, 30, 80, iba1PrivateKey);
        vm.warp(startTs + REVEAL_OFFSET + 1);
        vm.prank(bridger);
        auction.startClearingStage(seriesId);

        // Zero winners, zero issued, clearing rate at the floor (non-zero): a valid No-sale result.
        vm.expectEmit(true, false, false, true);
        emit IIntexAuction.AuctionClearingExecuted(seriesId, floor, 0);
        vm.prank(bridger);
        auction.executeAuctionClearing(seriesId, 0, floor, 0);

        assertEq(uint8(auction.getAuctionStage(seriesId)), uint8(IIntexAuction.AuctionStage.Completed));
        IIntexAuction.AuctionData memory fin = auction.getAuctionInfo(seriesId);
        assertEq(fin.result.wonBidsCount, 0);
        assertEq(fin.result.issuedIntexCount, 0);
        assertEq(fin.result.auctionClearingRate, floor);
        assertEq(fin.result.issuedIntexLoadedPromis, 0);
    }

    /// @dev No-sale with no supply: even when `minIntexBidRate > 0`, the clearing rate can be 0
    ///      (nothing was allocated because supply was exhausted/zero). It must still complete —
    ///      full refund is handled via REFUND_INSTRUCTIONS, nothing is issued — and NOT revert
    ///      `ZeroValue`/`ClearingRateBelowMin`. The `cleared` flag drives the Completed stage.
    function test_ExecuteAuctionClearing_NoSale_ZeroRate() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250145;
        uint32 floor = 50; // minIntexBidRate > 0
        _start(seriesId, floor, 1);
        _commit(seriesId, iba1, 30, 80, iba1PrivateKey);
        _enterRevealStage(seriesId, startTs);
        _reveal(seriesId, iba1, 30, 80, iba1PrivateKey);
        vm.warp(startTs + REVEAL_OFFSET + 1);
        vm.prank(bridger);
        auction.startClearingStage(seriesId);

        // issued=0, clearingRate=0, won=0 — accepted despite floor=50 (no supply was available).
        vm.expectEmit(true, false, false, true);
        emit IIntexAuction.AuctionClearingExecuted(seriesId, 0, 0);
        vm.prank(bridger);
        auction.executeAuctionClearing(seriesId, 0, 0, 0);

        assertEq(uint8(auction.getAuctionStage(seriesId)), uint8(IIntexAuction.AuctionStage.Completed));
        IIntexAuction.AuctionData memory fin = auction.getAuctionInfo(seriesId);
        assertEq(fin.result.auctionClearingRate, 0);
        assertEq(fin.result.issuedIntexCount, 0);
        assertEq(fin.result.wonBidsCount, 0);
        assertEq(fin.result.issuedIntexLoadedPromis, 0);

        // Idempotent: re-clearing a completed auction is rejected on the stage gate.
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexAuction.StageRequired.selector,
                IIntexAuction.AuctionStage.Issuance,
                IIntexAuction.AuctionStage.Completed
            )
        );
        vm.prank(bridger);
        auction.executeAuctionClearing(seriesId, 0, 0, 0);
    }

    // --- Validation Tests ---
    function test_AuctionStart_Validation() public {
        uint32 seriesId = 20250127;

        // Schedule with commitEnd in the past.
        IIntexAuction.AuctionSchedule memory pastSchedule = IIntexAuction.AuctionSchedule({
            commitEnd: uint32(block.timestamp),
            revealEnd: uint32(block.timestamp + 200),
            issuanceEnd: uint32(block.timestamp + 300)
        });
        vm.expectRevert(IIntexAuction.InvalidSchedule.selector);
        vm.prank(bridger);
        auction.auctionStart(seriesId, pastSchedule, _params(10, 1));

        // Schedule not strictly increasing (revealEnd <= commitEnd).
        IIntexAuction.AuctionSchedule memory nonIncreasing = IIntexAuction.AuctionSchedule({
            commitEnd: uint32(block.timestamp + 200),
            revealEnd: uint32(block.timestamp + 200),
            issuanceEnd: uint32(block.timestamp + 300)
        });
        vm.expectRevert(IIntexAuction.InvalidSchedule.selector);
        vm.prank(bridger);
        auction.auctionStart(seriesId, nonIncreasing, _params(10, 1));

        // Valid start succeeds.
        vm.prank(bridger);
        auction.auctionStart(seriesId, _schedule(), _params(10, 1));

        // AuctionAlreadyExists
        vm.expectRevert(IIntexAuction.AuctionAlreadyExists.selector);
        vm.prank(bridger);
        auction.auctionStart(seriesId, _schedule(), _params(10, 1));
    }

    function test_Wire_Validation() public {
        IntexAuction newAuction = DeployProxy.intexAuction(admin, bridger);
        vm.startPrank(admin);
        newAuction.grantRole(newAuction.RELAYER_ROLE(), bridger);

        // Zero escrow address
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.ZeroAddress.selector, "escrowContract"));
        newAuction.wire(address(0));

        vm.stopPrank();
    }

    function test_ExecuteAuctionClearing_Validation() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250128;
        _start(seriesId, 10, 1);
        _enterRevealStage(seriesId, startTs);
        // Reach the Issuance stage (time-derived: now >= revealEnd).
        vm.warp(startTs + REVEAL_OFFSET + 1);
        vm.prank(bridger);
        auction.startClearingStage(seriesId);

        // Zero auctionClearingRate
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.ZeroValue.selector, "auctionClearingRate"));
        vm.prank(bridger);
        auction.executeAuctionClearing(seriesId, 100, 0, 1);
    }

    function test_RevealBid_Validation() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250129;
        _start(seriesId, 10, 1);
        _commit(seriesId, iba1, 10, 20, iba1PrivateKey);
        _enterRevealStage(seriesId, startTs);

        // Zero quantity
        bytes memory sig = _createSignature(seriesId, iba1, 0, 20, iba1PrivateKey);
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.ZeroValue.selector, "quantity/bidRate"));
        vm.prank(iba1);
        auction.revealBid(seriesId, 0, 20, uint64(block.chainid), sig);

        // Zero bidRate
        sig = _createSignature(seriesId, iba1, 10, 0, iba1PrivateKey);
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.ZeroValue.selector, "quantity/bidRate"));
        vm.prank(iba1);
        auction.revealBid(seriesId, 10, 0, uint64(block.chainid), sig);

        // Wrong chainId — caller's chainId param does not match block.chainid -> WrongChain.
        uint64 wrongChainId = 999;
        sig = _createSignature(seriesId, iba1, 10, 20, iba1PrivateKey);
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.WrongChain.selector, block.chainid, uint256(wrongChainId)));
        vm.prank(iba1);
        auction.revealBid(seriesId, 10, 20, wrongChainId, sig);
    }

    function test_StartClearingStage_AlreadyClearing() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250130;
        _start(seriesId, 10, 1);
        _enterRevealStage(seriesId, startTs);
        vm.prank(bridger);
        auction.startClearingStage(seriesId);

        // Call again when already in the issuance stage: idempotent, re-emits the stage update.
        vm.expectEmit(true, false, false, true);
        emit IIntexAuction.AuctionStageUpdated(
            seriesId, IIntexAuction.AuctionStage.Issuance, uint32(block.timestamp), ""
        );
        vm.prank(bridger);
        auction.startClearingStage(seriesId);
    }

    function test_ViewFunctions_NotFound() public {
        uint32 nonExistentSeries = 99999999; // non-existent series

        vm.expectRevert(IIntexAuction.AuctionNotFound.selector);
        auction.getAuctionInfo(nonExistentSeries);

        vm.expectRevert(IIntexAuction.AuctionNotFound.selector);
        auction.getAuctionDetails(nonExistentSeries);

        vm.expectRevert(IIntexAuction.AuctionNotFound.selector);
        auction.getAuctionStage(nonExistentSeries);
    }

    function test_RevealBid_WrongSigner() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250131;
        _start(seriesId, 10, 1);
        _commit(seriesId, iba1, 10, 20, iba1PrivateKey);
        _enterRevealStage(seriesId, startTs);

        // Try to reveal with signature from iba1 but call from iba2
        // This will fail with BidNotFound because iba2 has no commit
        bytes memory sig = _createSignature(seriesId, iba1, 10, 20, iba1PrivateKey);
        vm.expectRevert(IIntexAuction.BidNotFound.selector);
        vm.prank(iba2);
        auction.revealBid(seriesId, 10, 20, uint64(block.chainid), sig);
    }

    /// @dev Reentrancy probe: arm the escrow mock to call back into `revealBid` during
    ///      `lockFunds`. With `nonReentrant` in place the inner call reverts with
    ///      `ReentrancyGuardReentrantCall`, which propagates and unwinds all state.
    ///      Removing `nonReentrant` from `revealBid` makes this test fail (reentry succeeds,
    ///      attacker double-records the bid) — i.e. it is a true red→green test of the guard.
    function test_RevealBid_reentrancyBlocked() public {
        uint256 startTs = block.timestamp;
        uint32 seriesId = 20250201;
        _start(seriesId, 10, 1);
        _commit(seriesId, iba1, 10, 20, iba1PrivateKey);
        _enterRevealStage(seriesId, startTs);

        bytes memory sig = _createSignature(seriesId, iba1, 10, 20, iba1PrivateKey);
        bytes memory reentrantCall =
            abi.encodeCall(IIntexAuction.revealBid, (seriesId, 10, 20, uint64(block.chainid), sig));
        escrow.armReentry(auction, reentrantCall);

        bytes4 reentrancyGuard = bytes4(keccak256("ReentrancyGuardReentrantCall()"));
        vm.expectRevert(reentrancyGuard);
        vm.prank(iba1);
        auction.revealBid(seriesId, 10, 20, uint64(block.chainid), sig);

        // Tx unwound: no reveal recorded, no bid pushed.
        assertFalse(auction.revealedBidsByBidder(seriesId, iba1));
        (, IIntexAuction.SubmittedBidData[] memory bids) = auction.getAuctionDetails(seriesId);
        assertEq(bids.length, 0);
        (, uint32 revealedBidsCount) = auction.auctionRunningCounts(seriesId);
        assertEq(revealedBidsCount, 0);
    }
}
