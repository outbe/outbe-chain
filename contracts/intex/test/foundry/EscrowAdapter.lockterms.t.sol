// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";

contract EscrowAdapterLockTermsTest is Test {
    bytes32 internal constant STORAGE_SLOT = 0x9dc6707131c30ec20e38ebcfbc4641faad640e3439439d400ea9dd2fe8f83a00;

    EscrowAdapter internal escrow;
    MockTheCompact internal compact;
    MockWCOEN internal token;

    address internal admin = address(1);
    address internal relayer = address(2);
    address internal auction = address(3);
    address internal bidder = address(5);

    uint32 internal constant DAY = 1;
    uint128 internal constant LOCK = 1000e18;

    function setUp() public {
        escrow = DeployProxy.escrowAdapter(admin, relayer);
        compact = new MockTheCompact();
        token = new MockWCOEN();
        compact.setResetPeriodSeconds(0);
        vm.prank(admin);
        escrow.wire(auction, address(compact), address(token));

        token.mint(bidder, LOCK);
        vm.prank(bidder);
        token.approve(address(escrow), type(uint256).max);
    }

    function test_TheLockKeepsTheBidRateAndQuantity() public {
        vm.prank(auction);
        escrow.lockFunds(DAY, bidder, LOCK, 800_000, 30);

        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(DAY, bidder);
        assertEq(lock.bidRate, 800_000, "bid rate");
        assertEq(lock.quantity, 30, "quantity");
        assertEq(lock.lockedAmount, LOCK, "amount");
    }

    /// @dev Rate and quantity fill the lock's first word; the refund split keeps its own second word, so a lock
    ///      written before the upgrade reads its old fields where they were.
    function test_TheTermsSitInTheFirstWordAndLeaveTheSecondAlone() public {
        vm.prank(auction);
        escrow.lockFunds(DAY, bidder, LOCK, 800_000, 30);

        bytes32 dayMap = keccak256(abi.encode(uint256(DAY), uint256(STORAGE_SLOT) + 5));
        uint256 first = uint256(vm.load(address(escrow), keccak256(abi.encode(bidder, dayMap))));
        bytes32 second = vm.load(address(escrow), bytes32(uint256(keccak256(abi.encode(bidder, dayMap))) + 1));

        assertEq(uint128(first), LOCK, "amount in the low 16 bytes");
        assertEq(uint8(first >> 160), uint8(IEscrowAdapter.LockStatus.Locked), "status after the timestamp");
        assertEq(uint32(first >> 168), 800_000, "bid rate after the status");
        assertEq(uint16(first >> 200), 30, "quantity after the bid rate");
        assertEq(second, bytes32(0), "the split word is untouched");
    }

    function test_AZeroBidRateIsRejected() public {
        vm.prank(auction);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroValue.selector, "bidRate"));
        escrow.lockFunds(DAY, bidder, LOCK, 0, 30);
    }

    function test_AZeroQuantityIsRejected() public {
        vm.prank(auction);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroValue.selector, "quantity"));
        escrow.lockFunds(DAY, bidder, LOCK, 800_000, 0);
    }
}
