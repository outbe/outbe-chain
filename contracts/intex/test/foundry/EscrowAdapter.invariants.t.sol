// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {EscrowAdapter} from "@contracts/bnb/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/bnb/interfaces/IEscrowAdapter.sol";
import {IVaultProvider} from "@contracts/vendor/outbe-vault/interfaces/IVaultProvider.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockERC20} from "@test-mocks/MockERC20.sol";
import {MockSettlementVault} from "@test-mocks/MockSettlementVault.sol";
import {MockVaultProvider} from "@test-mocks/MockVaultProvider.sol";

/// @dev Property test for the per-series escrow invariant:
///   Σ bidLocks[seriesId][bidder].lockedAmount, status == Locked
///     == auctionEscrowState[seriesId].totalLocked
/// Holds across every state transition (lock, finalize, emergency refund).
contract EscrowAdapterInvariantsTest is Test {
    EscrowAdapter escrow;
    MockTheCompact compact;
    MockERC20 paymentToken;
    MockSettlementVault mockVault;
    MockVaultProvider provider;

    address admin = address(1);
    address bridger = address(2);
    address auction = address(3);
    address bidderA = address(5);
    address bidderB = address(6);
    address bidderC = address(7);

    uint32 s1 = 1;
    uint32 s2 = 2;

    uint64 constant LOCK_A = 100 * 10 ** 6;
    uint64 constant LOCK_B = 250 * 10 ** 6;
    uint64 constant LOCK_C = 75 * 10 ** 6;

    function setUp() public {
        escrow = new EscrowAdapter(admin, bridger);
        compact = new MockTheCompact();
        paymentToken = new MockERC20("USD Coin", "USDC", 6);
        mockVault = new MockSettlementVault(address(paymentToken), "Mock Vault USDC", "mvUSDC", 6);
        provider = new MockVaultProvider();
        provider.addVault(mockVault);
        provider.addLiquiditySource(address(escrow), IVaultProvider.LiquiditySource.IntexBidPrice);

        vm.prank(admin);
        escrow.wire(auction, address(compact), address(provider), address(paymentToken));
        compact.setResetPeriodSeconds(0);

        address[3] memory bidders = [bidderA, bidderB, bidderC];
        for (uint256 i = 0; i < bidders.length; i++) {
            paymentToken.mint(bidders[i], 10_000 * 10 ** 6);
            vm.prank(bidders[i]);
            paymentToken.approve(address(escrow), type(uint256).max);
        }
    }

    function _assertSeriesInvariant(uint32 seriesId, address[3] memory bidders) internal view {
        uint64 sum = 0;
        for (uint256 i = 0; i < bidders.length; i++) {
            IEscrowAdapter.BidLock memory lock = escrow.getBidLock(seriesId, bidders[i]);
            if (lock.status == IEscrowAdapter.LockStatus.Locked) {
                sum += lock.lockedAmount;
            } else if (lock.status == IEscrowAdapter.LockStatus.RefundClaimed) {
                // Refund paid, payout portion still parked in The Compact pending settleVaultOwed.
                sum += lock.lockedAmount - lock.failedRefund;
            }
        }
        (,, uint64 totalLocked) = escrow.getAuctionStatus(seriesId);
        assertEq(sum, totalLocked, "per-series totalLocked drift");
    }

    function test_Invariant_HoldsAcrossLockFinalizeAndRefund() public {
        address[3] memory bidders = [bidderA, bidderB, bidderC];

        // Empty state.
        _assertSeriesInvariant(s1, bidders);
        _assertSeriesInvariant(s2, bidders);

        // Mixed locks across two series.
        vm.prank(auction);
        escrow.lockFunds(s1, bidderA, LOCK_A);
        _assertSeriesInvariant(s1, bidders);

        vm.prank(auction);
        escrow.lockFunds(s1, bidderB, LOCK_B);
        _assertSeriesInvariant(s1, bidders);

        vm.prank(auction);
        escrow.lockFunds(s2, bidderC, LOCK_C);
        _assertSeriesInvariant(s1, bidders);
        _assertSeriesInvariant(s2, bidders);

        // Permissionless refund of one lock on s1 after the 72h safety window.
        vm.warp(block.timestamp + escrow.REFUND_DELAY());
        escrow.claimRefund(s1, bidderA);
        _assertSeriesInvariant(s1, bidders);
        _assertSeriesInvariant(s2, bidders);

        // Finalize the remaining s1 lock with a partial split.
        IEscrowAdapter.FinalizationInstruction[] memory s1Instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        s1Instructions[0] = IEscrowAdapter.FinalizationInstruction({
            bidder: bidderB, refundedAmount: LOCK_B / 2, paidAmount: LOCK_B - LOCK_B / 2
        });
        vm.prank(bridger);
        escrow.finalizeAuction(s1, bytes32(uint256(0x5151)), s1Instructions);
        _assertSeriesInvariant(s1, bidders);
        _assertSeriesInvariant(s2, bidders);

        // Finalize s2 with a full claim.
        IEscrowAdapter.FinalizationInstruction[] memory s2Instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        s2Instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidderC, refundedAmount: 0, paidAmount: LOCK_C});
        vm.prank(bridger);
        escrow.finalizeAuction(s2, bytes32(uint256(0x5252)), s2Instructions);
        _assertSeriesInvariant(s1, bidders);
        _assertSeriesInvariant(s2, bidders);
    }

    /// @dev Sanity check that the invariant helper catches injected drift.
    function test_Invariant_CatchesInjectedDrift() public {
        address[3] memory bidders = [bidderA, bidderB, bidderC];

        vm.prank(auction);
        escrow.lockFunds(s1, bidderA, LOCK_A);

        // auctionEscrowState mapping slot lookup: keccak256(abi.encode(s1, baseSlot)).
        // We bump `totalLocked` (low 8 bytes of the packed slot) without touching bidLocks
        // to confirm the helper fires when the two sides diverge.
        bytes32 baseSlot = bytes32(_auctionEscrowStateSlot());
        bytes32 entrySlot = keccak256(abi.encode(uint256(s1), uint256(baseSlot)));
        bytes32 packed = vm.load(address(escrow), entrySlot);
        // Add 1 to the uint64 totalLocked field (low 64 bits).
        bytes32 corrupted = bytes32(uint256(packed) + 1);
        vm.store(address(escrow), entrySlot, corrupted);

        vm.expectRevert();
        this._externalAssertInvariant(s1, bidders);
    }

    function _externalAssertInvariant(uint32 seriesId, address[3] memory bidders) external view {
        _assertSeriesInvariant(seriesId, bidders);
    }

    /// @dev Storage slot of `auctionEscrowState` mapping in `EscrowAdapter`. See
    /// `forge inspect contracts/bnb/EscrowAdapter.sol:EscrowAdapter storage-layout`.
    /// AccessControl `_roles` occupies slot 0; OZ 5 ReentrancyGuard uses ERC-7201 namespaced
    /// storage and consumes no contract-relative slots.
    function _auctionEscrowStateSlot() internal pure returns (uint256) {
        return 8;
    }
}
