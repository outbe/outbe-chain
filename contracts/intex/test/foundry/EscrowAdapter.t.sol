// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {IERC6909} from "@openzeppelin/contracts/interfaces/IERC6909.sol";
import {EscrowAdapter} from "@contracts/bnb/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/bnb/interfaces/IEscrowAdapter.sol";
import {IAllocator} from "@contracts/vendor/the-compact/interfaces/IAllocator.sol";
import {IVaultProvider} from "@contracts/vendor/outbe-vault/interfaces/IVaultProvider.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockERC20} from "@test-mocks/MockERC20.sol";
import {MockSettlementVault} from "@test-mocks/MockSettlementVault.sol";
import {MockVaultProvider} from "@test-mocks/MockVaultProvider.sol";

contract EscrowAdapterTest is Test {
    EscrowAdapter escrow;
    MockTheCompact compact;
    MockERC20 paymentToken;
    MockSettlementVault mockVault;
    MockVaultProvider provider;

    address admin = address(1);
    address bridger = address(2);
    address auction = address(3);
    address bidder1 = address(5);
    address bidder2 = address(6);
    address outsider = address(7);

    uint32 seriesId1 = 1;
    uint32 seriesId2 = 2;

    uint64 constant LOCK_AMOUNT = 1000 * 10 ** 6; // 1000 USDC

    /// @dev Stand-in for the inbound LZ packet GUID that carries refund instructions. Threaded
    ///      through `finalizeAuction`/`retryFinalize` into the emitted events.
    bytes32 constant GUID = bytes32(uint256(0xDEADBEEF));

    /// @dev Live ERC6909 balance held by the escrow in The Compact for the active lockId.
    function _liveCompactBalance() internal view returns (uint256) {
        return IERC6909(address(compact)).balanceOf(address(escrow), escrow.lockId());
    }

    function setUp() public {
        escrow = new EscrowAdapter(admin, bridger);
        compact = new MockTheCompact();
        paymentToken = new MockERC20("USD Coin", "USDC", 18);
        mockVault = new MockSettlementVault(address(paymentToken), "Mock Vault USDC", "mvUSDC", 18);
        provider = new MockVaultProvider();
        provider.addVault(mockVault);
        // Whitelist escrow as a permitted depositor on the provider (production: outbe-vault
        // owner calls `addLiquiditySource(escrow, IntexBidPrice)` post-deploy).
        provider.addLiquiditySource(address(escrow), IVaultProvider.LiquiditySource.IntexBidPrice);

        // Wire dependencies (no allow-list precondition anymore).
        vm.prank(admin);
        escrow.wire(auction, address(compact), address(provider), address(paymentToken));

        // Set reset period to 0 for immediate withdrawal in tests
        compact.setResetPeriodSeconds(0);

        // Fund bidders
        paymentToken.mint(bidder1, 10000 * 10 ** 6);
        paymentToken.mint(bidder2, 10000 * 10 ** 6);

        // Approve escrow to spend bidder tokens
        vm.prank(bidder1);
        paymentToken.approve(address(escrow), type(uint256).max);
        vm.prank(bidder2);
        paymentToken.approve(address(escrow), type(uint256).max);
    }

    // --- Constructor Tests ---
    function test_Constructor() public {
        EscrowAdapter newEscrow = new EscrowAdapter(admin, bridger);
        assertTrue(newEscrow.hasRole(newEscrow.DEFAULT_ADMIN_ROLE(), admin));
        assertTrue(newEscrow.hasRole(newEscrow.RELAYER_ROLE(), bridger));
    }

    function test_Constructor_ZeroAdmin() public {
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "defaultAdmin"));
        new EscrowAdapter(address(0), bridger);
    }

    function test_Constructor_ZeroBridger() public {
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "bridger"));
        new EscrowAdapter(admin, address(0));
    }

    // --- Wire Tests ---
    function test_Wire() public view {
        assertEq(escrow.intexAuctionContract(), auction);
        assertEq(address(escrow.compact()), address(compact));
        assertEq(address(escrow.vaultProvider()), address(provider));
        assertEq(address(escrow.paymentToken()), address(paymentToken));
        assertTrue(escrow.hasRole(escrow.AUCTION_ROLE(), auction));
        assertTrue(escrow.allocatorId() > 0);
    }

    function test_Wire_ZeroAuction() public {
        EscrowAdapter newEscrow = new EscrowAdapter(admin, bridger);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "intexAuction"));
        vm.prank(admin);
        newEscrow.wire(address(0), address(compact), address(provider), address(paymentToken));
    }

    function test_Wire_ZeroCompact() public {
        EscrowAdapter newEscrow = new EscrowAdapter(admin, bridger);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "compact"));
        vm.prank(admin);
        newEscrow.wire(auction, address(0), address(provider), address(paymentToken));
    }

    function test_Wire_ZeroVaultProvider() public {
        EscrowAdapter newEscrow = new EscrowAdapter(admin, bridger);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "vaultProvider"));
        vm.prank(admin);
        newEscrow.wire(auction, address(compact), address(0), address(paymentToken));
    }

    function test_Wire_ZeroPaymentToken() public {
        EscrowAdapter newEscrow = new EscrowAdapter(admin, bridger);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "paymentToken"));
        vm.prank(admin);
        newEscrow.wire(auction, address(compact), address(provider), address(0));
    }

    function test_Wire_EmitsWired_OnInitial() public {
        EscrowAdapter freshEscrow = new EscrowAdapter(admin, bridger);
        // Initial wire: every `*Old` field is the zero address.
        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.Wired(
            address(0),
            auction,
            address(0),
            address(compact),
            address(0),
            address(provider),
            address(0),
            address(paymentToken)
        );
        vm.prank(admin);
        freshEscrow.wire(auction, address(compact), address(provider), address(paymentToken));
    }

    function test_Wire_EmitsWired_OnRotation() public {
        // Rotate the auction address (no LiveLocksOutstanding constraint — no locks opened in setUp).
        // `escrow` was wired in setUp with (auction, compact, provider, paymentToken); only the
        // auction rotates, so its old value is non-zero and the rest carry their prior addresses.
        address newAuction = address(0xBEEF);
        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.Wired(
            auction,
            newAuction,
            address(compact),
            address(compact),
            address(provider),
            address(provider),
            address(paymentToken),
            address(paymentToken)
        );
        vm.prank(admin);
        escrow.wire(newAuction, address(compact), address(provider), address(paymentToken));
    }

    function test_Wire_OnlyAdmin() public {
        EscrowAdapter newEscrow = new EscrowAdapter(admin, bridger);
        vm.expectRevert();
        vm.prank(outsider);
        newEscrow.wire(auction, address(compact), address(provider), address(paymentToken));
    }

    // --- PaymentTokenAlias Tests ---
    function test_PaymentTokenAlias() public view {
        assertEq(escrow.PAYMENT_TOKEN_ALIAS(), 840);
    }

    // --- LockFunds Tests ---
    function test_LockFunds() public {
        uint256 balanceBefore = paymentToken.balanceOf(bidder1);

        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        // Check bidder balance decreased
        assertEq(paymentToken.balanceOf(bidder1), balanceBefore - LOCK_AMOUNT);

        // Check lock data
        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(seriesId1, bidder1);
        assertEq(lock.lockedAmount, LOCK_AMOUNT);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.Locked));
        assertTrue(lock.lockedAt > 0);

        // Check auction stats
        (bool hasLocks, bool isFinalized, uint64 totalLocked) = escrow.getAuctionStatus(seriesId1);
        assertTrue(hasLocks);
        assertFalse(isFinalized);
        assertEq(totalLocked, LOCK_AMOUNT);
        assertEq(_liveCompactBalance(), LOCK_AMOUNT);
    }

    function test_LockFunds_MultipleBidders() public {
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder2, LOCK_AMOUNT * 2);

        // Check auction stats
        (bool hasLocks, bool isFinalized, uint64 totalLocked) = escrow.getAuctionStatus(seriesId1);
        assertTrue(hasLocks);
        assertFalse(isFinalized);
        assertEq(totalLocked, LOCK_AMOUNT * 3);
        assertEq(_liveCompactBalance(), LOCK_AMOUNT * 3);
    }

    function test_LockFunds_ZeroBidder() public {
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "bidder"));
        vm.prank(auction);
        escrow.lockFunds(seriesId1, address(0), LOCK_AMOUNT);
    }

    function test_LockFunds_ZeroAmount() public {
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroValue.selector, "amount"));
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, 0);
    }

    /// @notice cheap sanity floor on `seriesId`. The `AUCTION_ROLE` gate already guarantees
    ///         a real series, but a zero id is obviously bogus and is rejected before any state write.
    function test_LockFunds_ZeroSeriesId() public {
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroValue.selector, "seriesId"));
        vm.prank(auction);
        escrow.lockFunds(0, bidder1, LOCK_AMOUNT);
    }

    function test_LockFunds_AlreadyLocked() public {
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        vm.expectRevert(IEscrowAdapter.BidAlreadyLocked.selector);
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);
    }

    function test_LockFunds_OnlyAuctionRole() public {
        vm.expectRevert();
        vm.prank(outsider);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        vm.expectRevert();
        vm.prank(admin);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        vm.expectRevert();
        vm.prank(bridger);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);
    }

    // --- FinalizeAuction Tests ---
    function test_FinalizeAuction_FullRefund() public {
        // Lock funds
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        uint256 bidderBalanceBefore = paymentToken.balanceOf(bidder1);

        // Finalize with full refund
        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: LOCK_AMOUNT, paidAmount: 0});

        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        // Check bidder received refund
        assertEq(paymentToken.balanceOf(bidder1), bidderBalanceBefore + LOCK_AMOUNT);

        // Check auction status
        (bool hasLocks, bool isFinalized, uint64 totalLocked) = escrow.getAuctionStatus(seriesId1);
        assertTrue(hasLocks); // Count stays, but amount is 0
        assertTrue(isFinalized);
        assertEq(totalLocked, 0);
        assertEq(_liveCompactBalance(), 0);

        // Check lock status
        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(seriesId1, bidder1);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.Finalized));
    }

    function test_FinalizeAuction_FullClaim() public {
        // Lock funds
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        uint256 vaultBalanceBefore = paymentToken.balanceOf(address(mockVault));

        // Finalize with full claim (winning bid)
        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: 0, paidAmount: LOCK_AMOUNT});

        vm.expectEmit(true, true, false, true);
        emit IEscrowAdapter.AuctionEscrowFinalized(GUID, seriesId1, 0, LOCK_AMOUNT, 1);

        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        // Check vault received funds
        assertEq(paymentToken.balanceOf(address(mockVault)), vaultBalanceBefore + LOCK_AMOUNT);

        // Check accounting cleared
        (, bool isFinalized, uint64 totalLocked) = escrow.getAuctionStatus(seriesId1);
        assertTrue(isFinalized);
        assertEq(totalLocked, 0);
    }

    function test_FinalizeAuction_PartialRefundAndClaim() public {
        // Lock funds
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        uint256 bidderBalanceBefore = paymentToken.balanceOf(bidder1);
        uint256 vaultBalanceBefore = paymentToken.balanceOf(address(mockVault));
        uint64 refundedAmount = LOCK_AMOUNT * 30 / 100; // 30% refund
        uint64 paidAmount = LOCK_AMOUNT - refundedAmount; // 70% claim

        // Finalize with partial refund and claim
        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] = IEscrowAdapter.FinalizationInstruction({
            bidder: bidder1, refundedAmount: refundedAmount, paidAmount: paidAmount
        });

        vm.expectEmit(true, true, false, true);
        emit IEscrowAdapter.AuctionEscrowFinalized(GUID, seriesId1, refundedAmount, paidAmount, 1);

        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        // Check balances
        assertEq(paymentToken.balanceOf(bidder1), bidderBalanceBefore + refundedAmount);
        assertEq(paymentToken.balanceOf(address(mockVault)), vaultBalanceBefore + paidAmount);
    }

    function test_FinalizeAuction_MultipleBidders() public {
        // Lock funds for multiple bidders
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder2, LOCK_AMOUNT * 2);

        // Finalize: bidder1 gets full refund, bidder2 gets a 50/50 split.
        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](2);
        instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: LOCK_AMOUNT, paidAmount: 0});
        instructions[1] = IEscrowAdapter.FinalizationInstruction({
            bidder: bidder2, refundedAmount: LOCK_AMOUNT, paidAmount: LOCK_AMOUNT
        });

        // totalRefunded = LOCK_AMOUNT (b1) + LOCK_AMOUNT (b2) = 2*LOCK_AMOUNT
        // totalPaid = 0 (b1) + LOCK_AMOUNT (b2) = LOCK_AMOUNT
        vm.expectEmit(true, true, false, true);
        emit IEscrowAdapter.AuctionEscrowFinalized(GUID, seriesId1, LOCK_AMOUNT * 2, LOCK_AMOUNT, 2);

        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        // All escrow drained for the series.
        (, bool isFinalized, uint64 totalLocked) = escrow.getAuctionStatus(seriesId1);
        assertTrue(isFinalized);
        assertEq(totalLocked, 0);
        assertEq(_liveCompactBalance(), 0);
    }

    function test_FinalizeAuction_EmptyInstructions() public {
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](0);

        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroValue.selector, "instructions"));
        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);
    }

    function test_FinalizeAuction_AlreadyFinalized() public {
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: LOCK_AMOUNT, paidAmount: 0});

        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        // Try to finalize again
        vm.expectRevert(IEscrowAdapter.AlreadyFinalized.selector);
        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);
    }

    function test_FinalizeAuction_ZeroBidder_EmitsBidderRefundFailed() public {
        // A zero-address bidder fails inside the per-bidder try/catch and emits BidderRefundFailed;
        // the outer call still succeeds (with zero totals because the single iteration failed).
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: address(0), refundedAmount: LOCK_AMOUNT, paidAmount: 0});

        vm.expectEmit(true, true, true, false);
        emit IEscrowAdapter.BidderRefundFailed(GUID, seriesId1, address(0), "");
        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        // bidder1's lock is still recoverable via retryFinalize (relayer) / claimRefund.
        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(seriesId1, bidder1);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.Locked));
    }

    function test_FinalizeAuction_LockNotActive_EmitsBidderRefundFailed() public {
        // Series has zero locks: the single instruction's bidder has no active lock, so the
        // per-bidder try/catch catches LockNotActive and emits BidderRefundFailed.
        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: LOCK_AMOUNT, paidAmount: 0});

        vm.expectEmit(true, true, true, false);
        emit IEscrowAdapter.BidderRefundFailed(GUID, seriesId1, bidder1, "");
        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);
    }

    function test_FinalizeAuction_OneFailure_OthersSucceed() public {
        // Two bidders: bidder1's instruction has an amount mismatch (fails), bidder2's is valid.
        // Fail-safe loop: bidder1 emits BidderRefundFailed, bidder2 finalizes normally.
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder2, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](2);
        instructions[0] = IEscrowAdapter.FinalizationInstruction({
            bidder: bidder1,
            refundedAmount: LOCK_AMOUNT / 2,
            paidAmount: LOCK_AMOUNT / 2 - 1 // mismatch — will fail
        });
        instructions[1] = IEscrowAdapter.FinalizationInstruction({
            bidder: bidder2,
            refundedAmount: LOCK_AMOUNT,
            paidAmount: 0 // full refund, valid
        });

        uint256 bidder2BalanceBefore = paymentToken.balanceOf(bidder2); // after lockFunds debit

        vm.expectEmit(true, true, true, false);
        emit IEscrowAdapter.BidderRefundFailed(GUID, seriesId1, bidder1, "");
        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        // bidder1's lock unchanged (still Locked); bidder2's finalized + refunded.
        assertEq(uint8(escrow.getBidLock(seriesId1, bidder1).status), uint8(IEscrowAdapter.LockStatus.Locked));
        assertEq(uint8(escrow.getBidLock(seriesId1, bidder2).status), uint8(IEscrowAdapter.LockStatus.Finalized));
        assertEq(paymentToken.balanceOf(bidder2), bidder2BalanceBefore + LOCK_AMOUNT);
    }

    function test_FinalizeAuction_AmountMismatch_EmitsBidderRefundFailed() public {
        // A bidder whose refund + payout doesn't match the locked amount fails inside the per-bidder
        // try/catch and emits BidderRefundFailed; the outer call still succeeds.
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] = IEscrowAdapter.FinalizationInstruction({
            bidder: bidder1,
            refundedAmount: LOCK_AMOUNT / 2,
            paidAmount: LOCK_AMOUNT / 2 - 1 // Missing 1 unit
        });

        vm.expectEmit(true, true, true, false);
        emit IEscrowAdapter.BidderRefundFailed(GUID, seriesId1, bidder1, "");
        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        // Lock remains active for recovery.
        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(seriesId1, bidder1);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.Locked));
    }

    function test_FinalizeAuction_AllFail_EmitsFinalizationNoOp() public {
        // Every instruction fails (here: amount mismatch on the only bidder) → zero settled. The
        // series is finalized but degenerate; FinalizationNoOp surfaces it instead of a silent no-op.
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: 0, paidAmount: LOCK_AMOUNT - 1});

        vm.expectEmit(true, false, false, true, address(escrow));
        emit IEscrowAdapter.FinalizationNoOp(seriesId1, 1);
        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);
    }

    function test_FinalizeAuction_OnlyBridgeRole() public {
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: LOCK_AMOUNT, paidAmount: 0});

        vm.expectRevert();
        vm.prank(outsider);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        vm.expectRevert();
        vm.prank(admin);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        vm.expectRevert();
        vm.prank(auction);
        escrow.finalizeAuction(seriesId1, GUID, instructions);
    }

    // --- IAllocator Tests ---
    function test_Attest_ValidLockId() public {
        // First lock some funds to set lockId
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        uint256 lockId = escrow.lockId();
        bytes4 result = escrow.attest(address(0), address(0), address(0), lockId, 0);
        assertEq(result, IAllocator.attest.selector);
    }

    function test_Attest_InvalidLockId() public {
        // First lock some funds to set lockId
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.UnexpectedLockId.selector, uint256(999)));
        escrow.attest(address(0), address(0), address(0), 999, 0);
    }

    function test_AuthorizeClaim_AlwaysReverts() public {
        uint256[2][] memory idsAndAmounts = new uint256[2][](0);
        vm.expectRevert(IEscrowAdapter.ClaimAuthorizationUnsupported.selector);
        escrow.authorizeClaim(bytes32(0), address(0), address(0), 0, 0, idsAndAmounts, "");
    }

    function test_IsClaimAuthorized_AlwaysFalse() public view {
        uint256[2][] memory idsAndAmounts = new uint256[2][](0);
        bool result = escrow.isClaimAuthorized(bytes32(0), address(0), address(0), 0, 0, idsAndAmounts, "");
        assertFalse(result);
    }

    // --- View Functions Tests ---
    function test_GetBidLock() public {
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(seriesId1, bidder1);
        assertEq(lock.lockedAmount, LOCK_AMOUNT);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.Locked));
    }

    function test_GetBidLock_NonExistent() public view {
        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(seriesId1, bidder1);
        assertEq(lock.lockedAmount, 0);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.None));
    }

    function test_GetAuctionStatus() public {
        // Before any locks
        (bool hasLocks, bool isFinalized, uint64 totalLocked) = escrow.getAuctionStatus(seriesId1);
        assertFalse(hasLocks);
        assertFalse(isFinalized);
        assertEq(totalLocked, 0);

        // After lock
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        (hasLocks, isFinalized, totalLocked) = escrow.getAuctionStatus(seriesId1);
        assertTrue(hasLocks);
        assertFalse(isFinalized);
        assertEq(totalLocked, LOCK_AMOUNT);
    }

    // --- SupportsInterface Tests ---
    function test_SupportsInterface() public view {
        assertTrue(escrow.supportsInterface(type(IAllocator).interfaceId));
    }

    // --- Events Tests ---
    function test_Events_FundsLocked() public {
        vm.expectEmit(true, true, false, true);
        emit IEscrowAdapter.FundsLocked(seriesId1, bidder1, LOCK_AMOUNT);

        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);
    }

    function test_Events_FundsRefunded() public {
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: LOCK_AMOUNT, paidAmount: 0});

        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.FundsRefunded(GUID, seriesId1, bidder1, LOCK_AMOUNT);

        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);
    }

    function test_Events_FundsClaimed() public {
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: 0, paidAmount: LOCK_AMOUNT});

        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.FundsClaimed(GUID, seriesId1, bidder1, LOCK_AMOUNT);

        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);
    }

    function test_Events_AuctionEscrowFinalized() public {
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] = IEscrowAdapter.FinalizationInstruction({
            bidder: bidder1, refundedAmount: LOCK_AMOUNT / 2, paidAmount: LOCK_AMOUNT / 2
        });

        vm.expectEmit(true, true, false, true);
        emit IEscrowAdapter.AuctionEscrowFinalized(GUID, seriesId1, LOCK_AMOUNT / 2, LOCK_AMOUNT / 2, 1);

        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);
    }

    // --- Payment Token Rotation Tests ---
    function test_Wire_RotatePaymentToken_RejectedWithLiveLocks() public {
        // Lock funds with the current paymentToken
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        // Rewire targeting a new token while locks are still in flight — must revert
        MockERC20 usdt = new MockERC20("Tether", "USDT", 6);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.LiveLocksOutstanding.selector, uint256(LOCK_AMOUNT)));
        vm.prank(admin);
        escrow.wire(auction, address(compact), address(provider), address(usdt));
    }

    function test_Wire_RotatePaymentToken_AllowedWhenNoLocks() public {
        // Swap active token when no locks are held.
        MockERC20 usdt = new MockERC20("Tether", "USDT", 6);
        vm.prank(admin);
        escrow.wire(auction, address(compact), address(provider), address(usdt));

        assertEq(address(escrow.paymentToken()), address(usdt));
    }

    function test_Wire_RewireSameTokenStaysAllowedWithLocks() public {
        // Active locks must not block re-wiring with the same token (e.g. updating the provider).
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        MockSettlementVault newMockVault =
            new MockSettlementVault(address(paymentToken), "New Mock Vault", "nmvUSDC", 18);
        MockVaultProvider newProvider = new MockVaultProvider();
        newProvider.addVault(newMockVault);
        vm.prank(admin);
        escrow.wire(auction, address(compact), address(newProvider), address(paymentToken));
        assertEq(address(escrow.vaultProvider()), address(newProvider));
    }

    // --- claimRefund ---

    function test_ClaimRefund_AfterDelay_Succeeds() public {
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        uint256 balanceBefore = paymentToken.balanceOf(bidder1);

        vm.warp(block.timestamp + escrow.REFUND_DELAY());

        // Permissionless caller (an outsider) triggers the refund; funds go to bidder1.
        // claimRefund is not LZ-triggered, so the emitted guid is the zero sentinel.
        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.FundsRefunded(bytes32(0), seriesId1, bidder1, LOCK_AMOUNT);
        vm.prank(outsider);
        escrow.claimRefund(seriesId1, bidder1);

        assertEq(paymentToken.balanceOf(bidder1), balanceBefore + LOCK_AMOUNT);
        assertEq(uint8(escrow.getBidLock(seriesId1, bidder1).status), uint8(IEscrowAdapter.LockStatus.Finalized));
    }

    function test_ClaimRefund_BeforeDelay_Reverts() public {
        uint32 lockedAt = uint32(block.timestamp);
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        // One second before the delay elapses.
        uint32 claimableAt = lockedAt + escrow.REFUND_DELAY();
        vm.warp(claimableAt - 1);

        vm.expectRevert(
            abi.encodeWithSelector(IEscrowAdapter.RefundNotYetClaimable.selector, claimableAt, claimableAt - 1)
        );
        escrow.claimRefund(seriesId1, bidder1);
    }

    function test_ClaimRefund_NotLocked_Reverts() public {
        // No lock exists for bidder1.
        vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
        escrow.claimRefund(seriesId1, bidder1);
    }

    function test_ClaimRefund_DoubleClaim_Reverts() public {
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);
        vm.warp(block.timestamp + escrow.REFUND_DELAY());

        escrow.claimRefund(seriesId1, bidder1);

        vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
        escrow.claimRefund(seriesId1, bidder1);
    }

    function test_ClaimRefund_ZeroBidder_Reverts() public {
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "bidder"));
        escrow.claimRefund(seriesId1, address(0));
    }

    function test_ClaimRefund_ForcedWithdrawalReturnsFalse_Reverts() public {
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);
        vm.warp(block.timestamp + escrow.REFUND_DELAY());

        // The Compact's forced withdrawal returns false (e.g. reset period not elapsed); the
        // adapter must surface this as the dedicated ForcedWithdrawalFailed, not a generic error.
        compact.setForcedWithdrawalShouldFail(true);

        vm.expectRevert(IEscrowAdapter.ForcedWithdrawalFailed.selector);
        escrow.claimRefund(seriesId1, bidder1);
    }

    function test_ClaimRefund_PostFinalize_RevertsWithin7d() public {
        // Lock, then finalize with a failing instruction (BidderRefundFailed leaves lock Locked).
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] = IEscrowAdapter.FinalizationInstruction({
            bidder: bidder1,
            refundedAmount: 0,
            paidAmount: LOCK_AMOUNT - 1 // mismatch, fails inside try/catch
        });
        uint32 finalizedAt = uint32(block.timestamp);
        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        // 72h after lockedAt — would unlock the pre-finalize window — but post-finalize 7d wins now.
        uint32 nowAt = finalizedAt + escrow.REFUND_DELAY();
        uint32 claimableAt = finalizedAt + escrow.POST_FINALIZE_REFUND_DELAY();
        vm.warp(nowAt);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.RefundNotYetClaimable.selector, claimableAt, nowAt));
        escrow.claimRefund(seriesId1, bidder1);
    }

    function test_ClaimRefund_PostFinalize_VaultStillDown_ParksRemainder() public {
        // Vault still down at claim time: the bidder is refunded their portion (never the full
        // principal), and the payout portion is parked in The Compact as RefundClaimed for later
        // permissionless settlement — the refund is not blocked by the vault failure.
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        uint64 refundPortion = LOCK_AMOUNT * 30 / 100;
        uint64 paidPortion = LOCK_AMOUNT - refundPortion;

        provider.setRevertOnDeposit(true); // payout deposit fails, but the split is valid
        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] = IEscrowAdapter.FinalizationInstruction({
            bidder: bidder1, refundedAmount: refundPortion, paidAmount: paidPortion
        });
        uint32 finalizedAt = uint32(block.timestamp);
        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);
        assertEq(uint8(escrow.getBidLock(seriesId1, bidder1).status), uint8(IEscrowAdapter.LockStatus.Locked));

        uint256 balanceBefore = paymentToken.balanceOf(bidder1);
        vm.warp(finalizedAt + escrow.POST_FINALIZE_REFUND_DELAY());

        // Vault is still down, so the in-claim settle fails and the remainder is parked.
        vm.expectEmit(true, true, false, true, address(escrow));
        emit IEscrowAdapter.VaultOwedUnsettled(seriesId1, bidder1, paidPortion);
        escrow.claimRefund(seriesId1, bidder1);

        // Only the refund portion is paid out; the payout portion is neither refunded nor lost.
        assertEq(paymentToken.balanceOf(bidder1), balanceBefore + refundPortion);
        assertEq(uint8(escrow.getBidLock(seriesId1, bidder1).status), uint8(IEscrowAdapter.LockStatus.RefundClaimed));
        // The parked payout portion is still accounted for in totalLocked and still in The Compact.
        (,, uint64 totalLocked) = escrow.getAuctionStatus(seriesId1);
        assertEq(totalLocked, paidPortion);
        assertEq(_liveCompactBalance(), paidPortion);
    }

    function test_ClaimRefund_PostFinalize_VaultHealthy_SettlesInOneTx() public {
        // Vault recovered by claim time: claimRefund refunds the bidder AND settles the payout
        // portion into the vault in the same transaction — no leftover state, no keeper needed.
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        uint64 refundPortion = LOCK_AMOUNT * 30 / 100;
        uint64 paidPortion = LOCK_AMOUNT - refundPortion;

        provider.setRevertOnDeposit(true); // payout deposit fails during finalize
        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] = IEscrowAdapter.FinalizationInstruction({
            bidder: bidder1, refundedAmount: refundPortion, paidAmount: paidPortion
        });
        uint32 finalizedAt = uint32(block.timestamp);
        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        // Vault recovers before the bidder claims.
        provider.setRevertOnDeposit(false);
        uint256 balanceBefore = paymentToken.balanceOf(bidder1);
        uint256 vaultBefore = paymentToken.balanceOf(address(mockVault));
        vm.warp(finalizedAt + escrow.POST_FINALIZE_REFUND_DELAY());

        vm.expectEmit(true, true, false, true, address(escrow));
        emit IEscrowAdapter.VaultOwedSettled(seriesId1, bidder1, paidPortion);
        escrow.claimRefund(seriesId1, bidder1);

        // Bidder refunded their portion; payout portion deposited into the vault; lock terminal.
        assertEq(paymentToken.balanceOf(bidder1), balanceBefore + refundPortion);
        assertEq(paymentToken.balanceOf(address(mockVault)), vaultBefore + paidPortion);
        assertEq(uint8(escrow.getBidLock(seriesId1, bidder1).status), uint8(IEscrowAdapter.LockStatus.Finalized));
        (,, uint64 totalLocked) = escrow.getAuctionStatus(seriesId1);
        assertEq(totalLocked, 0);
        assertEq(_liveCompactBalance(), 0);
    }

    function test_SettleVaultOwed_Permissionless_FinishesParkedSettle() public {
        // After a parked claim (vault was down), anyone can settle the payout portion once the
        // vault recovers — the amount and destination are fixed by stored state.
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        uint64 refundPortion = LOCK_AMOUNT * 30 / 100;
        uint64 paidPortion = LOCK_AMOUNT - refundPortion;

        provider.setRevertOnDeposit(true);
        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] = IEscrowAdapter.FinalizationInstruction({
            bidder: bidder1, refundedAmount: refundPortion, paidAmount: paidPortion
        });
        uint32 finalizedAt = uint32(block.timestamp);
        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        vm.warp(finalizedAt + escrow.POST_FINALIZE_REFUND_DELAY());
        escrow.claimRefund(seriesId1, bidder1); // parks the remainder (vault still down)
        assertEq(uint8(escrow.getBidLock(seriesId1, bidder1).status), uint8(IEscrowAdapter.LockStatus.RefundClaimed));

        // Vault recovers; a random caller (not the bidder, not the relayer) settles the remainder.
        provider.setRevertOnDeposit(false);
        uint256 vaultBefore = paymentToken.balanceOf(address(mockVault));

        vm.expectEmit(true, true, false, true, address(escrow));
        emit IEscrowAdapter.VaultOwedSettled(seriesId1, bidder1, paidPortion);
        vm.prank(outsider);
        escrow.settleVaultOwed(seriesId1, bidder1);

        assertEq(paymentToken.balanceOf(address(mockVault)), vaultBefore + paidPortion);
        assertEq(uint8(escrow.getBidLock(seriesId1, bidder1).status), uint8(IEscrowAdapter.LockStatus.Finalized));
        (,, uint64 totalLocked) = escrow.getAuctionStatus(seriesId1);
        assertEq(totalLocked, 0);
        assertEq(_liveCompactBalance(), 0);
    }

    function test_SettleVaultOwed_RevertsWhenNotRefundClaimed() public {
        // A lock that is not in RefundClaimed has no parked vault portion to settle.
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.NoPendingVaultOwed.selector, seriesId1, bidder1));
        escrow.settleVaultOwed(seriesId1, bidder1);
    }

    function test_ClaimRefund_PostFinalize_RevertsSplitNotRecorded() public {
        // An amount-mismatch failure records no valid split, so claimRefund cannot pay out — the
        // relayer must retryFinalize with a correct split.
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: 0, paidAmount: LOCK_AMOUNT - 1});
        uint32 finalizedAt = uint32(block.timestamp);
        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        vm.warp(finalizedAt + escrow.POST_FINALIZE_REFUND_DELAY());
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.SplitNotRecorded.selector, seriesId1, bidder1));
        escrow.claimRefund(seriesId1, bidder1);
    }

    function test_ClaimRefund_AfterRetry_RevertsLockNotActive() public {
        // Retry moves lock to Finalized; subsequent claimRefund must revert.
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] = IEscrowAdapter.FinalizationInstruction({
            bidder: bidder1,
            refundedAmount: 0,
            paidAmount: LOCK_AMOUNT - 1 // mismatch
        });
        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        // Relayer retries with the correct split.
        vm.prank(bridger);
        escrow.retryFinalize(
            seriesId1,
            GUID,
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: LOCK_AMOUNT, paidAmount: 0})
        );

        // 7d later, claimRefund must still revert (already Finalized).
        vm.warp(block.timestamp + escrow.POST_FINALIZE_REFUND_DELAY());
        vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
        escrow.claimRefund(seriesId1, bidder1);
    }

    // --- retryFinalize ---

    function test_RetryFinalize_HappyPath_AfterFailedIteration() public {
        // Two bidders: bidder1's initial finalize iteration fails (amount mismatch); bidder2 succeeds.
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder2, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](2);
        instructions[0] = IEscrowAdapter.FinalizationInstruction({
            bidder: bidder1,
            refundedAmount: LOCK_AMOUNT / 2,
            paidAmount: LOCK_AMOUNT / 2 - 1 // mismatch — will fail
        });
        instructions[1] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder2, refundedAmount: LOCK_AMOUNT, paidAmount: 0});

        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        // bidder1 stayed Locked; bidder2 finalized.
        assertEq(uint8(escrow.getBidLock(seriesId1, bidder1).status), uint8(IEscrowAdapter.LockStatus.Locked));

        // Relayer retries bidder1 with the correct split.
        IEscrowAdapter.FinalizationInstruction memory retryInst = IEscrowAdapter.FinalizationInstruction({
            bidder: bidder1, refundedAmount: LOCK_AMOUNT / 2, paidAmount: LOCK_AMOUNT - LOCK_AMOUNT / 2
        });

        uint256 bidder1BalanceBefore = paymentToken.balanceOf(bidder1);

        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.BidderRetried(GUID, seriesId1, bidder1, retryInst.refundedAmount, retryInst.paidAmount);
        vm.prank(bridger);
        escrow.retryFinalize(seriesId1, GUID, retryInst);

        assertEq(uint8(escrow.getBidLock(seriesId1, bidder1).status), uint8(IEscrowAdapter.LockStatus.Finalized));
        assertEq(paymentToken.balanceOf(bidder1), bidder1BalanceBefore + retryInst.refundedAmount);
    }

    function test_RetryFinalize_Reverts_BeforeFinalize() public {
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction memory inst =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: LOCK_AMOUNT, paidAmount: 0});

        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.NotFinalizedYet.selector, seriesId1));
        vm.prank(bridger);
        escrow.retryFinalize(seriesId1, GUID, inst);
    }

    function test_RetryFinalize_Reverts_OnAlreadyFinalizedLock() public {
        // Successful finalize moves lock to Finalized; retrying it reverts LockNotActive.
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: LOCK_AMOUNT, paidAmount: 0});

        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
        vm.prank(bridger);
        escrow.retryFinalize(seriesId1, GUID, instructions[0]);
    }

    function test_RetryFinalize_OnlyRelayer() public {
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: 0, paidAmount: LOCK_AMOUNT - 1});
        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, GUID, instructions);

        // Now bidder1 sits in Locked (the iteration failed on amount mismatch). Outsider can't retry.
        vm.expectRevert();
        vm.prank(outsider);
        escrow.retryFinalize(
            seriesId1,
            GUID,
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: LOCK_AMOUNT, paidAmount: 0})
        );
    }

    // --- GUID threading ---

    /// @dev A single finalize call must stamp the same inbound packet GUID onto every fund-movement
    ///      event it emits (FundsRefunded, FundsClaimed) and the summary (AuctionEscrowFinalized),
    ///      so an indexer can attribute the whole batch to one cross-chain packet.
    function test_GuidThreading_AllFinalizeEvents_CarryPacketGuid() public {
        bytes32 packet = keccak256("inbound-packet-A");

        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT); // refunded bidder
        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder2, LOCK_AMOUNT); // paid (winning) bidder

        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](2);
        instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: LOCK_AMOUNT, paidAmount: 0});
        instructions[1] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder2, refundedAmount: 0, paidAmount: LOCK_AMOUNT});

        // All three events must carry `packet` as the indexed guid (topic1).
        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.FundsRefunded(packet, seriesId1, bidder1, LOCK_AMOUNT);
        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.FundsClaimed(packet, seriesId1, bidder2, LOCK_AMOUNT);
        vm.expectEmit(true, true, false, true);
        emit IEscrowAdapter.AuctionEscrowFinalized(packet, seriesId1, LOCK_AMOUNT, LOCK_AMOUNT, 2);

        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, packet, instructions);
    }

    /// @dev A relayer retry is its own inbound packet: `retryFinalize` must stamp the retry's GUID
    ///      (not the original finalize GUID) onto its events, so a re-sent packet is independently
    ///      attributable. Proves the guid is the threaded argument, not an echoed constant.
    function test_GuidThreading_RetryCarriesItsOwnGuid() public {
        bytes32 originalPacket = keccak256("inbound-packet-original");
        bytes32 retryPacket = keccak256("inbound-packet-retry");

        vm.prank(auction);
        escrow.lockFunds(seriesId1, bidder1, LOCK_AMOUNT);

        // First finalize fails on an amount mismatch (lock stays Locked), stamped with originalPacket.
        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: 0, paidAmount: LOCK_AMOUNT - 1});
        vm.expectEmit(true, true, true, false);
        emit IEscrowAdapter.BidderRefundFailed(originalPacket, seriesId1, bidder1, "");
        vm.prank(bridger);
        escrow.finalizeAuction(seriesId1, originalPacket, instructions);

        // Relayer retries under a distinct packet GUID; the retry events must carry retryPacket.
        IEscrowAdapter.FinalizationInstruction memory fixInst =
            IEscrowAdapter.FinalizationInstruction({bidder: bidder1, refundedAmount: LOCK_AMOUNT, paidAmount: 0});
        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.FundsRefunded(retryPacket, seriesId1, bidder1, LOCK_AMOUNT);
        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.BidderRetried(retryPacket, seriesId1, bidder1, LOCK_AMOUNT, 0);
        vm.prank(bridger);
        escrow.retryFinalize(seriesId1, retryPacket, fixInst);
    }
}
