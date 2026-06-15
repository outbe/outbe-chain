// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {IntexAuction} from "@contracts/bnb/IntexAuction.sol";
import {IIntexAuction} from "@contracts/bnb/interfaces/IIntexAuction.sol";
import {MockAuctionEscrow} from "@test-mocks/MockAuctionEscrow.sol";

/// @dev Property tests for the reveal lock-amount overflow guard and the clearing sanity bounds.
contract IntexAuctionFuzzTest is Test {
    IntexAuction internal auction;
    MockAuctionEscrow internal escrow;

    address internal admin = address(1);
    address internal bridger = address(2);

    uint256 internal iba1Pk = 0x100;
    uint256 internal iba2Pk = 0x200;
    address internal iba1;
    address internal iba2;

    bytes32 internal constant REVEAL_BID_TYPEHASH =
        keccak256("RevealBid(uint32 seriesId,address bidder,uint16 quantity,uint64 bidPrice)");
    bytes32 internal constant EIP712_DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");

    uint32 internal constant COMMIT_OFFSET = 100;
    uint32 internal constant REVEAL_OFFSET = 200;
    uint32 internal constant ISSUANCE_OFFSET = 300;

    uint16 internal constant MIN_QTY = 1;
    uint64 internal constant MIN_PRICE = 10;
    uint128 internal constant PROMIS_LOAD_MINOR = 1000;

    function setUp() public {
        iba1 = vm.addr(iba1Pk);
        iba2 = vm.addr(iba2Pk);

        auction = new IntexAuction(admin, bridger);
        escrow = new MockAuctionEscrow();

        vm.startPrank(admin);
        auction.grantRole(auction.RELAYER_ROLE(), bridger);
        auction.wire(address(escrow));
        vm.stopPrank();
    }

    function test_Fuzz_RevealBid_OverflowGuardFiresAboveUint64Max(uint256 qSeed, uint256 pSeed) public {
        uint16 quantity = uint16(bound(qSeed, 2, type(uint16).max));
        uint64 lo = uint64(type(uint64).max / quantity) + 1;
        uint64 bidPrice = uint64(bound(pSeed, lo, type(uint64).max));
        assertGt(uint256(quantity) * bidPrice, type(uint64).max, "precondition: product overflows uint64");

        uint32 seriesId = 20260201;
        _start(seriesId);
        bytes memory sig = _signFor(iba1Pk, seriesId, iba1, quantity, bidPrice);
        _commit(seriesId, iba1, sig);
        _enterReveal(seriesId);

        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.BidAmountOverflow.selector, quantity, bidPrice));
        vm.prank(iba1);
        auction.revealBid(seriesId, quantity, bidPrice, uint64(block.chainid), sig);
    }

    function test_Fuzz_RevealBid_ValidProductLocksExactAmount(uint256 qSeed, uint256 pSeed) public {
        uint16 quantity = uint16(bound(qSeed, MIN_QTY, type(uint16).max));
        uint64 hi = uint64(type(uint64).max / quantity);
        uint64 bidPrice = uint64(bound(pSeed, MIN_PRICE, hi));
        uint64 product = uint64(uint256(quantity) * bidPrice);

        uint32 seriesId = 20260202;
        _start(seriesId);
        bytes memory sig = _signFor(iba1Pk, seriesId, iba1, quantity, bidPrice);
        _commit(seriesId, iba1, sig);
        _enterReveal(seriesId);

        vm.prank(iba1);
        auction.revealBid(seriesId, quantity, bidPrice, uint64(block.chainid), sig);

        assertEq(escrow.lockedFunds(seriesId, iba1), product, "locked amount must equal quantity * bidPrice");
    }

    function test_Fuzz_ExecuteClearing_BoundsMatchPredicate(uint32 issued, uint256 priceSeed, uint256 wonSeed) public {
        uint32 seriesId = 20260203;
        uint32 revealed = _setupIssuanceWithTwoReveals(seriesId);

        uint64 clearingPrice = uint64(bound(priceSeed, 0, type(uint64).max));
        uint32 wonBidsCount = uint32(bound(wonSeed, 0, revealed + 3));

        if (clearingPrice == 0) {
            vm.expectRevert(abi.encodeWithSelector(IIntexAuction.ZeroValue.selector, "auctionIntexClearingPrice"));
            vm.prank(bridger);
            auction.executeAuctionClearing(seriesId, issued, clearingPrice, wonBidsCount);
        } else if (wonBidsCount > revealed) {
            vm.expectRevert(
                abi.encodeWithSelector(IIntexAuction.WonBidsExceedRevealed.selector, wonBidsCount, revealed)
            );
            vm.prank(bridger);
            auction.executeAuctionClearing(seriesId, issued, clearingPrice, wonBidsCount);
        } else if (clearingPrice < MIN_PRICE) {
            vm.expectRevert(
                abi.encodeWithSelector(IIntexAuction.ClearingPriceBelowMin.selector, clearingPrice, MIN_PRICE)
            );
            vm.prank(bridger);
            auction.executeAuctionClearing(seriesId, issued, clearingPrice, wonBidsCount);
        } else {
            vm.prank(bridger);
            auction.executeAuctionClearing(seriesId, issued, clearingPrice, wonBidsCount);
            IIntexAuction.AuctionData memory a = auction.getAuctionInfo(seriesId);
            assertEq(a.result.issuedIntexCount, issued, "issuedIntexCount");
            assertEq(a.result.auctionIntexClearingPrice, clearingPrice, "clearingPrice");
            assertEq(a.result.wonBidsCount, wonBidsCount, "wonBidsCount");
        }
    }

    function test_ExecuteClearing_IssuedCountHasNoUpperBound() public {
        uint32 seriesId = 20260204;
        uint32 revealed = _setupIssuanceWithTwoReveals(seriesId);

        vm.prank(bridger);
        auction.executeAuctionClearing(seriesId, type(uint32).max, MIN_PRICE, revealed);

        IIntexAuction.AuctionData memory a = auction.getAuctionInfo(seriesId);
        assertEq(a.result.issuedIntexCount, type(uint32).max, "issuedIntexCount accepted unbounded");
    }

    // --- Helpers ---

    function _domainSeparator() internal view returns (bytes32) {
        return keccak256(
            abi.encode(
                EIP712_DOMAIN_TYPEHASH,
                keccak256(bytes("IntexAuction")),
                keccak256(bytes("1")),
                block.chainid,
                address(auction)
            )
        );
    }

    function _signFor(uint256 pk, uint32 seriesId, address bidder, uint16 qty, uint64 price)
        internal
        view
        returns (bytes memory)
    {
        bytes32 structHash = keccak256(abi.encode(REVEAL_BID_TYPEHASH, seriesId, bidder, qty, price));
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", _domainSeparator(), structHash));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(pk, digest);
        return abi.encodePacked(r, s, v);
    }

    function _start(uint32 seriesId) internal {
        IIntexAuction.AuctionSchedule memory schedule = IIntexAuction.AuctionSchedule({
            commitEnd: uint32(block.timestamp + COMMIT_OFFSET),
            revealEnd: uint32(block.timestamp + REVEAL_OFFSET),
            issuanceEnd: uint32(block.timestamp + ISSUANCE_OFFSET)
        });
        IIntexAuction.AuctionParams memory params = IIntexAuction.AuctionParams({
            promisLoadMinor: PROMIS_LOAD_MINOR,
            minIntexBidPrice: MIN_PRICE,
            costAmountMinor: 100,
            floorPriceMinor: 100,
            minIntexBidQuantity: MIN_QTY
        });
        vm.prank(bridger);
        auction.auctionStart(seriesId, schedule, params);
    }

    function _commit(uint32 seriesId, address bidder, bytes memory sig) internal {
        vm.prank(bidder);
        auction.commitBid(seriesId, keccak256(sig));
    }

    function _enterReveal(uint32 seriesId) internal {
        vm.prank(bridger);
        auction.startRevealingBidsStage(seriesId, true);
        vm.warp(block.timestamp + COMMIT_OFFSET + 1);
    }

    function _setupIssuanceWithTwoReveals(uint32 seriesId) internal returns (uint32 revealed) {
        _start(seriesId);
        bytes memory s1 = _signFor(iba1Pk, seriesId, iba1, 5, 50);
        bytes memory s2 = _signFor(iba2Pk, seriesId, iba2, 5, 50);
        _commit(seriesId, iba1, s1);
        _commit(seriesId, iba2, s2);
        _enterReveal(seriesId);
        vm.prank(iba1);
        auction.revealBid(seriesId, 5, 50, uint64(block.chainid), s1);
        vm.prank(iba2);
        auction.revealBid(seriesId, 5, 50, uint64(block.chainid), s2);
        vm.warp(block.timestamp + 120);
        return 2;
    }
}
