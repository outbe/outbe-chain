// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {ReferenceCurrencyPriceLib} from "./helpers/ReferenceCurrencyPriceLib.sol";

contract RecordingLockEscrow {
    uint128 public amount;
    uint32 public bidRate;
    uint16 public quantity;

    function lockFunds(uint32, address, uint128 amount_, uint32 bidRate_, uint16 quantity_) external {
        amount = amount_;
        bidRate = bidRate_;
        quantity = quantity_;
    }

    function hasOutstandingLocks() external pure returns (bool) {
        return false;
    }
}

/// @dev The reveal hands the escrow the terms the lock was computed from, not just the amount.
contract IntexAuctionLockTermsTest is Test {
    bytes32 internal constant REVEAL_BID_TYPEHASH = keccak256(
        "RevealBid(uint32 worldwideDay,address bidder,uint16 quantity,uint32 bidRate,uint16 issuanceCurrency,uint16 referenceCurrency)"
    );
    uint32 internal constant DAY = 20250115;
    uint128 internal constant PROMIS_LOAD_MINOR = 100_000 * 1e6;
    uint16 internal constant QUANTITY = 30;
    uint32 internal constant RATE = 80;

    IntexAuction internal auction;
    RecordingLockEscrow internal escrow;
    address internal admin = address(1);
    address internal relayer = address(2);
    uint256 internal bidderKey = 0x100;
    address internal bidder;

    function setUp() public {
        bidder = vm.addr(bidderKey);
        auction = DeployProxy.intexAuction(admin, relayer);
        escrow = new RecordingLockEscrow();
        vm.startPrank(admin);
        auction.grantRole(auction.RELAYER_ROLE(), relayer);
        auction.wire(address(escrow));
        vm.stopPrank();
    }

    function _signature() internal view returns (bytes memory) {
        bytes32 structHash = keccak256(abi.encode(REVEAL_BID_TYPEHASH, DAY, bidder, QUANTITY, RATE, 840, 840));
        bytes32 domain = keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"),
                keccak256(bytes("IntexAuction")),
                keccak256(bytes("1")),
                block.chainid,
                address(auction)
            )
        );
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(bidderKey, keccak256(abi.encodePacked("\x19\x01", domain, structHash)));
        return abi.encodePacked(r, s, v);
    }

    function test_TheRevealPassesTheBidTermsToTheEscrow() public {
        uint256 startTs = block.timestamp;
        vm.prank(relayer);
        auction.auctionStart(
            DAY,
            IIntexAuction.WorldwideDayState.Green,
            IIntexAuction.AuctionSchedule({
                commitEnd: uint32(startTs + 100), revealEnd: uint32(startTs + 200), issuanceEnd: uint32(startTs + 300)
            }),
            IIntexAuction.AuctionParams({
                promisLoadMinor: PROMIS_LOAD_MINOR,
                minIntexBidRate: 50,
                prices: ReferenceCurrencyPriceLib.onePriced(840, 1e6, 100, 200),
                callTrigger: IIntexAuction.IntexCallTrigger({callWindow: 0, callThreshold: 0, callNoticePeriod: 0}),
                minIntexBidQuantity: 1,
                commitBondMinor: 0
            })
        );

        bytes memory signature = _signature();
        vm.prank(bidder);
        auction.commitBid(DAY, keccak256(signature));
        vm.warp(startTs + 101);
        vm.prank(bidder);
        auction.revealBid(DAY, QUANTITY, RATE, 840, 840, uint64(block.chainid), signature);

        assertEq(escrow.bidRate(), RATE, "bid rate");
        assertEq(escrow.quantity(), QUANTITY, "quantity");
        assertEq(escrow.amount(), uint256(QUANTITY) * PROMIS_LOAD_MINOR * RATE / 1e6 * 1e12, "amount");
    }
}
