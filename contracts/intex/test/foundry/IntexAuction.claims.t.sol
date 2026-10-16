// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";
import {RevertingERC1155Receiver} from "@test-mocks/RevertingERC1155Receiver.sol";
import {CreateSeriesLib} from "./helpers/CreateSeriesLib.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {ReferenceCurrencyPriceLib} from "./helpers/ReferenceCurrencyPriceLib.sol";

/// @dev Bidders claim through the auction. A winner issues its own units: the auction reads the bid,
///      the escrow closes the win, and the collection mints. A refund clears the bid once nothing reads it.
contract IntexAuctionClaimsTest is Test {
    bytes32 internal constant REVEAL_BID_TYPEHASH = keccak256(
        "RevealBid(uint32 worldwideDay,address bidder,uint16 quantity,uint32 bidRate,uint16 issuanceCurrency,uint16 referenceCurrency)"
    );
    uint32 internal constant DAY = 20250101;
    uint16 internal constant USD = 840;
    uint128 internal constant BASIS = 1_000_000;
    uint64 internal constant CLEARING_RATE = 600_000;
    uint32 internal constant RATE = 900_000;

    IntexAuction internal auction;
    EscrowAdapter internal escrow;
    IntexNFT1155 internal intex;
    MockWCOEN internal token;
    address internal admin = address(1);
    address internal relayer = address(2);
    uint256 internal aliceKey = 0xA11CE;
    uint256 internal bobKey = 0xB0B;
    address internal alice;
    address internal bob;
    uint256 internal startTs;

    function setUp() public {
        alice = vm.addr(aliceKey);
        bob = vm.addr(bobKey);
        intex = DeployProxy.intexNFT1155(admin, relayer);
        escrow = DeployProxy.escrowAdapter(admin, relayer);
        auction = DeployProxy.intexAuction(admin, relayer);
        MockTheCompact compact = new MockTheCompact();
        compact.setResetPeriodSeconds(0);
        token = new MockWCOEN();

        vm.startPrank(admin);
        escrow.wire(address(auction), address(compact), address(token));
        escrow.setProceedsRecipient(makeAddr("proceeds"));
        auction.wire(address(escrow));
        auction.setIntexContract(address(intex));
        intex.grantRole(intex.AUCTION_ROLE(), address(auction));
        vm.stopPrank();

        startTs = block.timestamp;
        vm.prank(relayer);
        auction.auctionStart(
            DAY,
            IIntexAuction.WorldwideDayState.Green,
            IIntexAuction.AuctionSchedule({
                commitEnd: uint32(startTs + 100), revealEnd: uint32(startTs + 200), issuanceEnd: uint32(startTs + 300)
            }),
            IIntexAuction.AuctionParams({
                promisLoadMinor: BASIS,
                minIntexBidRate: 1,
                prices: ReferenceCurrencyPriceLib.onePriced(USD, 1e6, 100, 200),
                callTrigger: IIntexAuction.IntexCallTrigger({callWindow: 0, callThreshold: 0, callNoticePeriod: 0}),
                minIntexBidQuantity: 1,
                commitBondMinor: 0
            })
        );
    }

    function _signature(uint256 key, uint16 quantity) internal view returns (bytes memory) {
        bytes32 structHash = keccak256(abi.encode(REVEAL_BID_TYPEHASH, DAY, vm.addr(key), quantity, RATE, USD, USD));
        bytes32 domain = keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"),
                keccak256(bytes("IntexAuction")),
                keccak256(bytes("1")),
                block.chainid,
                address(auction)
            )
        );
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, keccak256(abi.encodePacked("\x19\x01", domain, structHash)));
        return abi.encodePacked(r, s, v);
    }

    /// @dev Commit and reveal both bids, then move past the reveal window.
    function _bid(uint16 aliceQuantity, uint16 bobQuantity) internal {
        bytes memory aliceSig = _signature(aliceKey, aliceQuantity);
        bytes memory bobSig = _signature(bobKey, bobQuantity);
        vm.prank(alice);
        auction.commitBid(DAY, keccak256(aliceSig));
        vm.prank(bob);
        auction.commitBid(DAY, keccak256(bobSig));
        vm.warp(startTs + 101);
        _reveal(alice, aliceQuantity, aliceSig);
        _reveal(bob, bobQuantity, bobSig);
        vm.warp(startTs + 201);
    }

    function _reveal(address who, uint16 quantity, bytes memory signature) internal {
        token.mint(who, BridgeMsgCodec.escrowAmount(quantity, BASIS, RATE));
        vm.startPrank(who);
        token.approve(address(escrow), type(uint256).max);
        auction.revealBid(DAY, quantity, RATE, USD, USD, uint64(block.chainid), signature);
        vm.stopPrank();
    }

    function _createSeries(uint32 callNoticePeriod) internal {
        vm.prank(relayer);
        intex.createSeries(CreateSeriesLib.params(DAY, 100, callNoticePeriod));
    }

    function _finalize(address[] memory winners, uint16 partialIndex, uint16 partialWon) internal {
        vm.prank(relayer);
        escrow.finalizeAuction(DAY, bytes32(0), winners, partialIndex, partialWon, CLEARING_RATE, BASIS, true);
    }

    function _winner(address who) internal pure returns (address[] memory winners) {
        winners = new address[](1);
        winners[0] = who;
    }

    function _units(address who) internal view returns (uint256) {
        return intex.balanceOf(who, intex.issuedTokenId(CreateSeriesLib.seriesId(DAY)));
    }

    function _change(uint16 quantity) internal pure returns (uint256) {
        return BridgeMsgCodec.escrowAmount(quantity, BASIS, RATE)
            - BridgeMsgCodec.escrowAmount(quantity, BASIS, CLEARING_RATE);
    }

    function test_AWinnerIssuesItsUnitsAndTakesItsChange() public {
        _bid(3, 1);
        _createSeries(0);
        _finalize(_winner(alice), 0, 0);

        vm.expectEmit(true, true, true, true, address(auction));
        emit IIntexAuction.WinIssued(DAY, alice, CreateSeriesLib.seriesId(DAY), 3);
        vm.prank(bob);
        auction.issueIntex(DAY, alice);

        assertEq(_units(alice), 3, "the units won");
        assertEq(token.balanceOf(alice), _change(3), "the change");
        assertEq(uint8(escrow.getBidLock(DAY, alice).status), uint8(IEscrowAdapter.LockStatus.None), "lock gone");
        assertFalse(auction.revealedBidsByBidder(DAY, alice), "bid gone");
    }

    function test_APartialWinnerIssuesItsShare() public {
        _bid(1, 5);
        _createSeries(0);
        address[] memory winners = new address[](2);
        winners[0] = alice;
        winners[1] = bob;
        _finalize(winners, 1, 2);

        auction.issueIntex(DAY, bob);
        assertEq(_units(bob), 2);
    }

    function test_ALoserHasNothingToIssue() public {
        _bid(3, 1);
        _createSeries(0);
        _finalize(_winner(alice), 0, 0);

        vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
        auction.issueIntex(DAY, bob);
    }

    function test_AWinIssuesOnce() public {
        _bid(3, 1);
        _createSeries(0);
        _finalize(_winner(alice), 0, 0);
        auction.issueIntex(DAY, alice);

        vm.expectRevert(IIntexAuction.BidNotFound.selector);
        auction.issueIntex(DAY, alice);
        assertEq(_units(alice), 3);
    }

    function test_AWinWaitsForItsSeries() public {
        _bid(3, 1);
        _finalize(_winner(alice), 0, 0);

        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.SeriesNotCreated.selector, DAY, USD, USD));
        auction.issueIntex(DAY, alice);

        _createSeries(0);
        auction.issueIntex(DAY, alice);
        assertEq(_units(alice), 3);
    }

    function test_TheChangeCanBeRefundedBeforeTheUnitsAreIssued() public {
        _bid(3, 1);
        _finalize(_winner(alice), 0, 0);
        auction.claimRefund(DAY, alice);
        assertEq(token.balanceOf(alice), _change(3));

        _createSeries(0);
        auction.issueIntex(DAY, alice);
        assertEq(_units(alice), 3);
        assertEq(token.balanceOf(alice), _change(3), "the change is paid once");
    }

    function test_ACalledSeriesStillIssuesBeforeItsDeadline() public {
        _bid(3, 1);
        _createSeries(1 days);
        _finalize(_winner(alice), 0, 0);
        vm.prank(relayer);
        intex.markCalled(CreateSeriesLib.seriesId(DAY), uint32(block.timestamp));

        auction.issueIntex(DAY, alice);
        assertEq(_units(alice), 3);
    }

    function test_AnExpiredSeriesClosesTheWinWithoutUnits() public {
        _bid(3, 1);
        _createSeries(0);
        _finalize(_winner(alice), 0, 0);
        vm.prank(relayer);
        intex.markCalled(CreateSeriesLib.seriesId(DAY), uint32(block.timestamp));
        vm.warp(block.timestamp + 1);
        assertEq(uint8(intex.readData(CreateSeriesLib.seriesId(DAY)).state), uint8(IIntexNFT1155.IntexState.Expired));

        vm.expectEmit(true, true, true, true, address(auction));
        emit IIntexAuction.WinExpired(DAY, alice, CreateSeriesLib.seriesId(DAY), 3);
        auction.issueIntex(DAY, alice);

        assertEq(_units(alice), 0, "nothing issued");
        assertEq(token.balanceOf(alice), _change(3), "the change still returns");
        assertEq(uint8(escrow.getBidLock(DAY, alice).status), uint8(IEscrowAdapter.LockStatus.None), "lock gone");
        assertFalse(auction.revealedBidsByBidder(DAY, alice), "bid gone");
    }

    /// @dev A bidder that cannot hold the units loses nothing else: its change stays refundable.
    function test_AWinnerThatRejectsTheUnitsKeepsItsChange() public {
        _bid(3, 1);
        _createSeries(0);
        _finalize(_winner(alice), 0, 0);
        vm.etch(alice, address(new RevertingERC1155Receiver()).code);
        RevertingERC1155Receiver(alice).setReject(true);

        vm.expectRevert(bytes("reject-1155"));
        auction.issueIntex(DAY, alice);

        auction.claimRefund(DAY, alice);
        assertEq(token.balanceOf(alice), _change(3));
        assertTrue(auction.revealedBidsByBidder(DAY, alice), "the win waits for a holder");
    }

    function test_AnIssuedBidCannotBeRevealedAgain() public {
        bytes memory aliceSig = _signature(aliceKey, 3);
        vm.prank(alice);
        auction.commitBid(DAY, keccak256(aliceSig));
        vm.warp(startTs + 101);
        _reveal(alice, 3, aliceSig);
        _createSeries(0);
        _finalize(_winner(alice), 0, 0);
        auction.issueIntex(DAY, alice);

        vm.prank(alice);
        vm.expectRevert(IIntexAuction.BidNotFound.selector);
        auction.revealBid(DAY, 3, RATE, USD, USD, uint64(block.chainid), aliceSig);
    }

    function test_TheRelayerRoleCannotIssue() public {
        _createSeries(0);
        address router = makeAddr("router");
        vm.startPrank(admin);
        intex.grantRole(intex.RELAYER_ROLE(), router);
        vm.stopPrank();

        vm.prank(router);
        vm.expectRevert();
        intex.issueIntex(alice, 1, CreateSeriesLib.seriesId(DAY));
    }

    function test_ALoserRefundsAndClearsItsBid() public {
        _bid(3, 1);
        _finalize(_winner(alice), 0, 0);

        vm.prank(alice);
        auction.claimRefund(DAY, bob);

        assertEq(token.balanceOf(bob), BridgeMsgCodec.escrowAmount(1, BASIS, RATE), "the whole principal");
        assertFalse(auction.revealedBidsByBidder(DAY, bob), "bid gone");
        assertEq(uint8(escrow.getBidLock(DAY, bob).status), uint8(IEscrowAdapter.LockStatus.None), "lock gone");
    }

    function test_AWinnerRefundKeepsItsBidForTheIssue() public {
        _bid(3, 1);
        _finalize(_winner(alice), 0, 0);

        auction.claimRefund(DAY, alice);
        assertEq(token.balanceOf(alice), _change(3));
        assertTrue(auction.revealedBidsByBidder(DAY, alice), "the bid waits for the issue");
    }

    /// @dev Before the day finalizes, the bids relay may still read the bid, so a refund leaves it.
    function test_AnOpenDayRefundKeepsTheBidUntilTheDayFinalizes() public {
        _bid(3, 1);
        vm.warp(block.timestamp + escrow.UNFINALIZED_REFUND_DELAY());

        auction.claimRefund(DAY, bob);
        assertEq(token.balanceOf(bob), BridgeMsgCodec.escrowAmount(1, BASIS, RATE), "the whole principal");
        assertTrue(auction.revealedBidsByBidder(DAY, bob), "the bid stays");
        IIntexAuction.SubmittedBidData[] memory bids = auction.revealedBidsSlice(DAY, 0, 2);
        assertEq(bids[1].bidderAddress, bob, "the relay still reads the real bid");
        assertEq(bids[1].intexQuantity, 1);

        vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
        auction.claimRefund(DAY, bob);

        _finalize(new address[](0), 0, 0);
        auction.claimRefund(DAY, bob);
        assertFalse(auction.revealedBidsByBidder(DAY, bob), "cleared once the day finalized");
        assertEq(token.balanceOf(bob), BridgeMsgCodec.escrowAmount(1, BASIS, RATE), "paid once");
    }

    function test_TheAuctionClearsABidTheEscrowValvePaidOut() public {
        _bid(3, 1);
        _finalize(_winner(alice), 0, 0);
        vm.warp(block.timestamp + escrow.REFUND_ABANDON_DELAY());
        escrow.claimAbandonedRefund(DAY, bob);

        auction.claimRefund(DAY, bob);
        assertFalse(auction.revealedBidsByBidder(DAY, bob), "bid gone");
        assertEq(token.balanceOf(bob), BridgeMsgCodec.escrowAmount(1, BASIS, RATE), "paid once");
    }

    function test_TheRevealedBidIsReadableUntilItIsCleared() public {
        _bid(3, 1);
        IIntexAuction.SubmittedBidData memory bid = auction.revealedBidOf(DAY, alice);
        assertEq(bid.bidderAddress, alice);
        assertEq(bid.intexQuantity, 3);
        assertEq(bid.issuanceCurrency, USD);

        _createSeries(0);
        _finalize(_winner(alice), 0, 0);
        auction.issueIntex(DAY, alice);
        assertEq(auction.revealedBidOf(DAY, alice).bidderAddress, address(0), "cleared with the win");
    }

    function test_ARefundNeedsABid() public {
        vm.expectRevert(IIntexAuction.BidNotFound.selector);
        auction.claimRefund(DAY, makeAddr("stranger"));
    }

    function test_GasOfTheClaims() public {
        _bid(3, 1);
        _createSeries(0);
        _finalize(_winner(alice), 0, 0);

        uint256 before = gasleft();
        auction.claimRefund(DAY, bob);
        emit log_named_uint("auction_claim_refund_loser", before - gasleft());

        before = gasleft();
        auction.issueIntex(DAY, alice);
        emit log_named_uint("auction_issue_intex_with_change", before - gasleft());
    }

    function test_GasOfAnIssueWithoutChange() public {
        _bid(3, 1);
        _createSeries(0);
        _finalize(_winner(alice), 0, 0);
        auction.claimRefund(DAY, alice);

        uint256 before = gasleft();
        auction.issueIntex(DAY, alice);
        emit log_named_uint("auction_issue_intex_without_change", before - gasleft());
    }
}
