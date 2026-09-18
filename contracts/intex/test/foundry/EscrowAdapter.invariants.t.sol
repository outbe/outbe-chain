// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";

/// @dev Property test for the per-series escrow invariant:
///   sum bidLocks[worldwideDay][bidder].lockedAmount, status == Locked
///   + sum (lockedAmount - payment at the day's clearing terms), status == Won
///     == auctionEscrowState[worldwideDay].totalLocked
/// Holds across every state transition (lock, finalize, claim).
contract EscrowAdapterInvariantsTest is Test {
    EscrowAdapter escrow;
    MockTheCompact compact;
    MockWCOEN paymentToken;

    address admin = address(1);
    address bridger = address(2);
    address auction = address(3);
    address bidderA = address(5);
    address bidderB = address(6);
    address bidderC = address(7);

    uint32 s1 = 1;
    uint32 s2 = 2;

    uint128 constant BASIS = 1_000_000;
    uint32 constant CLEARING_RATE = 600_000;

    function setUp() public {
        escrow = DeployProxy.escrowAdapter(admin, bridger);
        compact = new MockTheCompact();
        paymentToken = new MockWCOEN();

        vm.prank(admin);
        escrow.wire(auction, address(compact), address(paymentToken));
        vm.prank(admin);
        escrow.setProceedsRecipient(bridger);
        compact.setResetPeriodSeconds(0);

        address[3] memory bidders = [bidderA, bidderB, bidderC];
        for (uint256 i = 0; i < bidders.length; i++) {
            paymentToken.mint(bidders[i], 10_000e18);
            vm.prank(bidders[i]);
            paymentToken.approve(address(escrow), type(uint256).max);
        }
    }

    function _assertSeriesInvariant(uint32 worldwideDay, address[3] memory bidders) internal view {
        uint128 sum = 0;
        for (uint256 i = 0; i < bidders.length; i++) {
            IEscrowAdapter.BidLock memory lock = escrow.getBidLock(worldwideDay, bidders[i]);
            if (lock.status == IEscrowAdapter.LockStatus.Locked) {
                sum += lock.lockedAmount;
            } else if (lock.status == IEscrowAdapter.LockStatus.Won) {
                sum += lock.lockedAmount - uint128(BridgeMsgCodec.escrowAmount(lock.quantity, BASIS, CLEARING_RATE));
            }
        }
        (,, uint128 totalLocked) = escrow.getAuctionStatus(worldwideDay);
        assertEq(sum, totalLocked, "per-series totalLocked drift");
    }

    function test_Invariant_HoldsAcrossLockFinalizeAndRefund() public {
        address[3] memory bidders = [bidderA, bidderB, bidderC];

        // Empty state.
        _assertSeriesInvariant(s1, bidders);
        _assertSeriesInvariant(s2, bidders);

        // Mixed locks across two series.
        _lock(s1, bidderA, 700_000, 1);
        _assertSeriesInvariant(s1, bidders);

        _lock(s1, bidderB, 900_000, 3);
        _assertSeriesInvariant(s1, bidders);

        _lock(s2, bidderC, CLEARING_RATE, 2);
        _assertSeriesInvariant(s1, bidders);
        _assertSeriesInvariant(s2, bidders);

        // Permissionless refund of one lock on s1 after the 72h safety window.
        vm.warp(block.timestamp + escrow.UNFINALIZED_REFUND_DELAY());
        escrow.claimRefund(s1, bidderA);
        _assertSeriesInvariant(s1, bidders);
        _assertSeriesInvariant(s2, bidders);

        // bidderB wins above the clearing rate and keeps its refund in escrow.
        _finalize(s1, bidderB);
        _assertSeriesInvariant(s1, bidders);
        _assertSeriesInvariant(s2, bidders);

        // bidderC wins at the clearing rate and pays its whole lock.
        _finalize(s2, bidderC);
        _assertSeriesInvariant(s1, bidders);
        _assertSeriesInvariant(s2, bidders);

        escrow.claimRefund(s1, bidderB);
        _assertSeriesInvariant(s1, bidders);
        _assertSeriesInvariant(s2, bidders);
    }

    function _lock(uint32 worldwideDay, address bidder, uint32 bidRate, uint16 quantity) internal {
        vm.prank(auction);
        escrow.lockFunds(
            worldwideDay, bidder, uint128(BridgeMsgCodec.escrowAmount(quantity, BASIS, bidRate)), bidRate, quantity
        );
    }

    function _finalize(uint32 worldwideDay, address winner) internal {
        address[] memory winners = new address[](1);
        winners[0] = winner;
        vm.prank(bridger);
        escrow.finalizeAuction(worldwideDay, bytes32(uint256(worldwideDay)), winners, 0, 0, CLEARING_RATE, BASIS, true);
    }

    /// @dev Sanity check that the invariant helper catches injected drift.
    function test_Invariant_CatchesInjectedDrift() public {
        address[3] memory bidders = [bidderA, bidderB, bidderC];

        _lock(s1, bidderA, 700_000, 1);

        // auctionEscrowState mapping slot lookup: keccak256(abi.encode(s1, baseSlot)).
        // We bump `totalLocked` (low 8 bytes of the packed slot) without touching bidLocks
        // to confirm the helper fires when the two sides diverge.
        bytes32 baseSlot = bytes32(_auctionEscrowStateSlot());
        bytes32 entrySlot = keccak256(abi.encode(uint256(s1), uint256(baseSlot)));
        bytes32 packed = vm.load(address(escrow), entrySlot);
        // Add 1 to the uint128 totalLocked field (low 64 bits).
        bytes32 corrupted = bytes32(uint256(packed) + 1);
        vm.store(address(escrow), entrySlot, corrupted);

        vm.expectRevert();
        this._externalAssertInvariant(s1, bidders);
    }

    function _externalAssertInvariant(uint32 worldwideDay, address[3] memory bidders) external view {
        _assertSeriesInvariant(worldwideDay, bidders);
    }

    /// @dev Storage slot of the `auctionEscrowState` mapping inside the contract's ERC-7201
    /// namespaced struct (`erc7201:outbe.intex.EscrowAdapter`). Field offset 6: four address/uint
    /// slots, the packed allocatorId+lockTag slot, then the bidLocks mapping precede it.
    function _auctionEscrowStateSlot() internal pure returns (uint256) {
        uint256 base = uint256(
            keccak256(abi.encode(uint256(keccak256("outbe.intex.EscrowAdapter")) - 1)) & ~bytes32(uint256(0xff))
        );
        return base + 6;
    }
}
