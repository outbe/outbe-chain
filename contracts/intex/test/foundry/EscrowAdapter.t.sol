// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {IERC6909} from "@openzeppelin/contracts/interfaces/IERC6909.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {IAllocator} from "@contracts/vendor/the-compact/interfaces/IAllocator.sol";
import {IntexUnits} from "@contracts/shared/libs/IntexUnits.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";

contract EscrowAdapterTest is Test {
    EscrowAdapter escrow;
    MockTheCompact compact;
    MockWCOEN paymentToken;

    address admin = address(1);
    address bridger = address(2);
    address auction = address(3);
    address bidder1 = address(5);
    address bidder2 = address(6);
    address outsider = address(7);
    address proceedsRecipient = address(8);

    uint32 worldwideDay1 = 1;
    uint32 worldwideDay2 = 2;

    uint128 constant LOCK_AMOUNT = 1000 * 10 ** 6;
    uint128 constant BASIS = 1_000_000;
    uint32 constant CLEARING_RATE = 600_000;

    /// @dev Stand-in for the inbound bridge message id that carries refund instructions. Threaded
    ///      through `finalizeAuction` into the emitted events.
    bytes32 constant RECEIVE_ID = bytes32(uint256(0xDEADBEEF));

    /// @dev Live ERC6909 balance held by the escrow in The Compact for the active lockId.
    function _liveCompactBalance() internal view returns (uint256) {
        return IERC6909(address(compact)).balanceOf(address(escrow), escrow.lockId());
    }

    function setUp() public {
        escrow = DeployProxy.escrowAdapter(admin, bridger);
        compact = new MockTheCompact();
        paymentToken = new MockWCOEN();

        // Wire dependencies (no allow-list precondition anymore).
        vm.prank(admin);
        escrow.wire(auction, address(compact), address(paymentToken));
        vm.prank(admin);
        escrow.setProceedsRecipient(proceedsRecipient);

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
        EscrowAdapter newEscrow = DeployProxy.escrowAdapter(admin, bridger);
        assertTrue(newEscrow.hasRole(newEscrow.DEFAULT_ADMIN_ROLE(), admin));
        assertTrue(newEscrow.hasRole(newEscrow.RELAYER_ROLE(), bridger));
    }

    function test_Constructor_ZeroAdmin() public {
        EscrowAdapter impl = new EscrowAdapter();
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "defaultAdmin"));
        new ERC1967Proxy(address(impl), abi.encodeCall(EscrowAdapter.initialize, (address(0))));
    }

    // --- Wire Tests ---
    function test_Wire() public view {
        assertEq(escrow.intexAuctionContract(), auction);
        assertEq(address(escrow.compact()), address(compact));
        assertEq(address(escrow.paymentToken()), address(paymentToken));
        assertTrue(escrow.hasRole(escrow.AUCTION_ROLE(), auction));
        assertTrue(escrow.allocatorId() > 0);
    }

    function test_Wire_ResetsAllocatorOnCompactRotation() public {
        uint96 allocatorBefore = escrow.allocatorId();
        bytes12 lockTagBefore = escrow.lockTag();
        assertTrue(allocatorBefore > 0);

        // Rotate to a new Compact (setUp opened no locks). Bump its counter so a fresh
        // registration yields a distinct allocatorId.
        MockTheCompact compact2 = new MockTheCompact();
        compact2.setResetPeriodSeconds(0);
        compact2.__registerAllocator(address(0xDEAD), "");

        vm.prank(admin);
        escrow.wire(auction, address(compact2), address(paymentToken));

        assertTrue(escrow.allocatorId() != allocatorBefore);
        assertTrue(escrow.lockTag() != lockTagBefore);
    }

    function test_HasOutstandingLocks_ReflectsLockState() public {
        assertFalse(escrow.hasOutstandingLocks());
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);
        assertTrue(escrow.hasOutstandingLocks());
    }

    function test_Wire_ZeroAuction() public {
        EscrowAdapter newEscrow = DeployProxy.escrowAdapter(admin, bridger);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "intexAuction"));
        vm.prank(admin);
        newEscrow.wire(address(0), address(compact), address(paymentToken));
    }

    function test_Wire_ZeroCompact() public {
        EscrowAdapter newEscrow = DeployProxy.escrowAdapter(admin, bridger);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "compact"));
        vm.prank(admin);
        newEscrow.wire(auction, address(0), address(paymentToken));
    }

    function test_Wire_ZeroPaymentToken() public {
        EscrowAdapter newEscrow = DeployProxy.escrowAdapter(admin, bridger);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "paymentToken"));
        vm.prank(admin);
        newEscrow.wire(auction, address(compact), address(0));
    }

    function test_Wire_EmitsWired_OnInitial() public {
        EscrowAdapter freshEscrow = DeployProxy.escrowAdapter(admin, bridger);
        // Initial wire: every `*Old` field is the zero address.
        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.Wired(address(0), auction, address(0), address(compact), address(0), address(paymentToken));
        vm.prank(admin);
        freshEscrow.wire(auction, address(compact), address(paymentToken));
    }

    function test_Wire_EmitsWired_OnRotation() public {
        // Rotate the auction address; the asset stays, so no version is retired.
        // `escrow` was wired in setUp with (auction, compact, paymentToken); only the auction
        // rotates, so its old value is non-zero and the rest carry their prior addresses.
        address newAuction = address(0xBEEF);
        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.Wired(
            auction, newAuction, address(compact), address(compact), address(paymentToken), address(paymentToken)
        );
        vm.prank(admin);
        escrow.wire(newAuction, address(compact), address(paymentToken));
    }

    function test_Wire_OnlyAdmin() public {
        EscrowAdapter newEscrow = DeployProxy.escrowAdapter(admin, bridger);
        vm.expectRevert();
        vm.prank(outsider);
        newEscrow.wire(auction, address(compact), address(paymentToken));
    }

    // --- LockFunds Tests ---
    function test_LockFunds() public {
        uint256 balanceBefore = paymentToken.balanceOf(bidder1);

        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);

        // Check bidder balance decreased
        assertEq(paymentToken.balanceOf(bidder1), balanceBefore - LOCK_AMOUNT);

        // Check lock data
        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(worldwideDay1, bidder1);
        assertEq(lock.lockedAmount, LOCK_AMOUNT);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.Locked));
        assertTrue(lock.lockedAt > 0);

        // Check auction stats
        (bool hasLocks, bool isFinalized, uint128 totalLocked) = escrow.getAuctionStatus(worldwideDay1);
        assertTrue(hasLocks);
        assertFalse(isFinalized);
        assertEq(totalLocked, LOCK_AMOUNT);
        assertEq(_liveCompactBalance(), LOCK_AMOUNT);
    }

    function test_LockFunds_MultipleBidders() public {
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);

        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder2, LOCK_AMOUNT * 2, 1_000_000, 1);

        // Check auction stats
        (bool hasLocks, bool isFinalized, uint128 totalLocked) = escrow.getAuctionStatus(worldwideDay1);
        assertTrue(hasLocks);
        assertFalse(isFinalized);
        assertEq(totalLocked, LOCK_AMOUNT * 3);
        assertEq(_liveCompactBalance(), LOCK_AMOUNT * 3);
    }

    function test_LockFunds_ZeroBidder() public {
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "bidder"));
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, address(0), LOCK_AMOUNT, 1_000_000, 1);
    }

    function test_LockFunds_ZeroAmount() public {
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroValue.selector, "amount"));
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, 0, 1_000_000, 1);
    }

    /// @notice cheap sanity floor on `worldwideDay`. The `AUCTION_ROLE` gate already guarantees
    ///         a real series, but a zero id is obviously bogus and is rejected before any state write.
    function test_LockFunds_ZeroWorldwideDay() public {
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroValue.selector, "worldwideDay"));
        vm.prank(auction);
        escrow.lockFunds(0, bidder1, LOCK_AMOUNT, 1_000_000, 1);
    }

    function test_LockFunds_AlreadyLocked() public {
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);

        vm.expectRevert(IEscrowAdapter.BidAlreadyLocked.selector);
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);
    }

    function test_LockFunds_OnlyAuctionRole() public {
        vm.expectRevert();
        vm.prank(outsider);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);

        vm.expectRevert();
        vm.prank(admin);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);

        vm.expectRevert();
        vm.prank(bridger);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);
    }

    // --- FinalizeAuction Tests ---

    /// @dev Lock a bid the way the auction sizes it, so the day's clearing terms can work out its payment.
    function _lockBid(address bidder, uint32 bidRate, uint16 quantity) internal returns (uint128 amount) {
        amount = uint128(IntexUnits.escrowAmount(quantity, BASIS, bidRate));
        paymentToken.mint(bidder, amount);
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder, amount, bidRate, quantity);
    }

    function _paidAtClearing(uint16 quantity) internal pure returns (uint128) {
        return uint128(IntexUnits.escrowAmount(quantity, BASIS, CLEARING_RATE));
    }

    function _winners(address a) internal pure returns (address[] memory winners) {
        winners = new address[](1);
        winners[0] = a;
    }

    function _winners(address a, address b) internal pure returns (address[] memory winners) {
        winners = new address[](2);
        winners[0] = a;
        winners[1] = b;
    }

    function _finalize(address[] memory winners) internal returns (uint128) {
        vm.prank(bridger);
        return escrow.finalizeAuction(worldwideDay1, RECEIVE_ID, winners, 0, 0, CLEARING_RATE, BASIS, true);
    }

    function test_FinalizeAuction_LoserIsLeftToClaim() public {
        uint128 locked = _lockBid(bidder1, 500_000, 1);
        uint256 bidderBalanceBefore = paymentToken.balanceOf(bidder1);

        _finalize(new address[](0));

        assertEq(paymentToken.balanceOf(bidder1), bidderBalanceBefore, "nothing is pushed to a loser");
        (bool hasLocks, bool isFinalized, uint128 totalLocked) = escrow.getAuctionStatus(worldwideDay1);
        assertTrue(hasLocks);
        assertTrue(isFinalized);
        assertEq(totalLocked, locked);
        assertEq(_liveCompactBalance(), locked);

        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(worldwideDay1, bidder1);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.Locked));
    }

    // --- a bidder the finalized day never named ---

    // Finalize the day naming bidder2 only, who pays its whole lock; bidder1 lost and is left Locked.
    function _finalizeOmittingBidder1() internal returns (uint128 locked1) {
        locked1 = _lockBid(bidder1, 500_000, 1);
        _lockBid(bidder2, CLEARING_RATE, 1);
        _finalize(_winners(bidder2));
    }

    function test_OmittedBidder_RecoversFullPrincipal() public {
        uint128 locked = _finalizeOmittingBidder1();
        uint256 balBefore = paymentToken.balanceOf(bidder1);

        escrow.claimRefund(worldwideDay1, bidder1); // permissionless, and at once

        assertEq(paymentToken.balanceOf(bidder1), balBefore + locked, "full principal refunded");
        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(worldwideDay1, bidder1);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.None), "lock deleted");
        (,, uint128 totalLocked) = escrow.getAuctionStatus(worldwideDay1);
        assertEq(totalLocked, 0, "totalLocked cleared (bidder2 at finalize, bidder1 on claim)");

        vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
        escrow.claimRefund(worldwideDay1, bidder1);
    }

    function test_FinalizeAuction_FullClaim() public {
        uint128 locked = _lockBid(bidder1, CLEARING_RATE, 3);
        uint256 recipientBalanceBefore = paymentToken.balanceOf(proceedsRecipient);

        vm.expectEmit(true, true, false, true);
        emit IEscrowAdapter.AuctionEscrowFinalized(RECEIVE_ID, worldwideDay1, 0, locked, 1);
        uint128 routed = _finalize(_winners(bidder1));

        // Proceeds handed to the configured recipient for cross-chain routing, not the caller.
        assertEq(routed, locked);
        assertEq(paymentToken.balanceOf(proceedsRecipient), recipientBalanceBefore + locked);
        assertEq(paymentToken.balanceOf(bridger), 0);

        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(worldwideDay1, bidder1);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.None), "nothing left to claim");
        (, bool isFinalized, uint128 totalLocked) = escrow.getAuctionStatus(worldwideDay1);
        assertTrue(isFinalized);
        assertEq(totalLocked, 0);
    }

    function test_FinalizeAuction_PartialRefundAndClaim() public {
        uint128 locked = _lockBid(bidder1, 900_000, 2);
        uint128 paid = _paidAtClearing(2);
        uint256 bidderBalanceBefore = paymentToken.balanceOf(bidder1);
        uint256 recipientBalanceBefore = paymentToken.balanceOf(proceedsRecipient);

        vm.expectEmit(true, true, false, true);
        emit IEscrowAdapter.AuctionEscrowFinalized(RECEIVE_ID, worldwideDay1, locked - paid, paid, 1);
        _finalize(_winners(bidder1));

        assertEq(paymentToken.balanceOf(proceedsRecipient), recipientBalanceBefore + paid, "payment routed");
        assertEq(paymentToken.balanceOf(bidder1), bidderBalanceBefore, "the refund waits for its claim");
        assertEq(uint8(escrow.getBidLock(worldwideDay1, bidder1).status), uint8(IEscrowAdapter.LockStatus.Won));
        (,, uint128 totalLocked) = escrow.getAuctionStatus(worldwideDay1);
        assertEq(totalLocked, locked - paid, "only the refund stays held");

        vm.prank(outsider);
        escrow.claimRefund(worldwideDay1, bidder1);

        assertEq(paymentToken.balanceOf(bidder1), bidderBalanceBefore + locked - paid, "refund claimed at once");
        (,, totalLocked) = escrow.getAuctionStatus(worldwideDay1);
        assertEq(totalLocked, 0);
        assertEq(_liveCompactBalance(), 0);
    }

    function test_FinalizeAuction_MultipleBidders() public {
        uint128 locked1 = _lockBid(bidder1, 900_000, 1);
        uint128 locked2 = _lockBid(bidder2, 700_000, 4);
        uint128 paid = _paidAtClearing(1) + _paidAtClearing(4);

        vm.expectEmit(true, true, false, true);
        emit IEscrowAdapter.AuctionEscrowFinalized(RECEIVE_ID, worldwideDay1, locked1 + locked2 - paid, paid, 2);
        _finalize(_winners(bidder1, bidder2));

        escrow.claimRefund(worldwideDay1, bidder1);
        escrow.claimRefund(worldwideDay1, bidder2);

        (, bool isFinalized, uint128 totalLocked) = escrow.getAuctionStatus(worldwideDay1);
        assertTrue(isFinalized);
        assertEq(totalLocked, 0);
        assertEq(_liveCompactBalance(), 0);
    }

    function test_FinalizeAuction_EmptyChunkClosesTheDay() public {
        uint128 locked = _lockBid(bidder1, 500_000, 1);

        vm.prank(bridger);
        uint128 routed = escrow.finalizeAuction(worldwideDay1, RECEIVE_ID, new address[](0), 0, 0, 0, 0, true);

        assertEq(routed, 0);
        (, bool isFinalized, uint128 totalLocked) = escrow.getAuctionStatus(worldwideDay1);
        assertTrue(isFinalized, "a chain with no winners still closes its day");
        assertEq(totalLocked, locked);
    }

    function test_FinalizeAuction_AlreadyFinalized() public {
        _lockBid(bidder1, 900_000, 1);
        _finalize(_winners(bidder1));

        address[] memory winners = _winners(bidder1);
        vm.expectRevert(IEscrowAdapter.AlreadyFinalized.selector);
        vm.prank(bridger);
        escrow.finalizeAuction(worldwideDay1, RECEIVE_ID, winners, 0, 0, CLEARING_RATE, BASIS, true);
    }

    function test_FinalizeAuction_ZeroBidder_EmitsBidderRefundFailed() public {
        // A zero-address winner holds no lock: it is skipped and the call still succeeds.
        _lockBid(bidder1, 900_000, 1);

        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.BidderRefundFailed(
            RECEIVE_ID, worldwideDay1, address(0), abi.encodeWithSelector(IEscrowAdapter.LockNotActive.selector)
        );
        _finalize(_winners(address(0)));

        // bidder1's lock is still recoverable by the bidder through claimRefund.
        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(worldwideDay1, bidder1);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.Locked));
    }

    function test_FinalizeAuction_LockNotActive_EmitsBidderRefundFailed() public {
        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.BidderRefundFailed(
            RECEIVE_ID, worldwideDay1, bidder1, abi.encodeWithSelector(IEscrowAdapter.LockNotActive.selector)
        );
        _finalize(_winners(bidder1));
    }

    function test_FinalizeAuction_OneFailure_OthersSucceed() public {
        // bidder1 bid under the clearing rate, so its payment would exceed its lock; bidder2 is a valid winner.
        uint128 locked1 = _lockBid(bidder1, 500_000, 1);
        uint128 locked2 = _lockBid(bidder2, 900_000, 1);

        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.BidderRefundFailed(
            RECEIVE_ID,
            worldwideDay1,
            bidder1,
            abi.encodeWithSelector(IEscrowAdapter.PaymentExceedsLock.selector, locked1, uint256(_paidAtClearing(1)))
        );
        _finalize(_winners(bidder1, bidder2));

        assertEq(uint8(escrow.getBidLock(worldwideDay1, bidder1).status), uint8(IEscrowAdapter.LockStatus.Locked));
        assertEq(uint8(escrow.getBidLock(worldwideDay1, bidder2).status), uint8(IEscrowAdapter.LockStatus.Won));
        (,, uint128 totalLocked) = escrow.getAuctionStatus(worldwideDay1);
        assertEq(totalLocked, locked1 + locked2 - _paidAtClearing(1));
    }

    function test_FinalizeAuction_PaymentExceedsLock_EmitsBidderRefundFailed() public {
        uint128 locked = _lockBid(bidder1, 500_000, 1);

        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.BidderRefundFailed(
            RECEIVE_ID,
            worldwideDay1,
            bidder1,
            abi.encodeWithSelector(IEscrowAdapter.PaymentExceedsLock.selector, locked, uint256(_paidAtClearing(1)))
        );
        _finalize(_winners(bidder1));

        // Lock remains active for recovery.
        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(worldwideDay1, bidder1);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.Locked));
    }

    function test_FinalizeAuction_AllFail_EmitsFinalizationNoOp() public {
        _lockBid(bidder1, 500_000, 1);

        vm.expectEmit(true, false, false, true, address(escrow));
        emit IEscrowAdapter.FinalizationNoOp(worldwideDay1, 1);
        _finalize(_winners(bidder1));
    }

    function test_FinalizeAuction_OnlyBridgeRole() public {
        _lockBid(bidder1, 900_000, 1);
        address[] memory winners = _winners(bidder1);

        vm.expectRevert();
        vm.prank(outsider);
        escrow.finalizeAuction(worldwideDay1, RECEIVE_ID, winners, 0, 0, CLEARING_RATE, BASIS, true);

        vm.expectRevert();
        vm.prank(admin);
        escrow.finalizeAuction(worldwideDay1, RECEIVE_ID, winners, 0, 0, CLEARING_RATE, BASIS, true);

        vm.expectRevert();
        vm.prank(auction);
        escrow.finalizeAuction(worldwideDay1, RECEIVE_ID, winners, 0, 0, CLEARING_RATE, BASIS, true);
    }

    // --- IAllocator Tests ---
    function test_Attest_ValidLockId() public {
        // First lock some funds to set lockId
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);

        uint256 lockId = escrow.lockId();
        bytes4 result = escrow.attest(address(0), address(0), address(0), lockId, 0);
        assertEq(result, IAllocator.attest.selector);
    }

    function test_Attest_InvalidLockId() public {
        // First lock some funds to set lockId
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);

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
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);

        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(worldwideDay1, bidder1);
        assertEq(lock.lockedAmount, LOCK_AMOUNT);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.Locked));
    }

    function test_GetBidLock_NonExistent() public view {
        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(worldwideDay1, bidder1);
        assertEq(lock.lockedAmount, 0);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.None));
    }

    function test_GetAuctionStatus() public {
        // Before any locks
        (bool hasLocks, bool isFinalized, uint128 totalLocked) = escrow.getAuctionStatus(worldwideDay1);
        assertFalse(hasLocks);
        assertFalse(isFinalized);
        assertEq(totalLocked, 0);

        // After lock
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);

        (hasLocks, isFinalized, totalLocked) = escrow.getAuctionStatus(worldwideDay1);
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
        emit IEscrowAdapter.FundsLocked(worldwideDay1, bidder1, LOCK_AMOUNT);

        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);
    }

    function test_Events_FundsRefunded() public {
        uint128 locked = _lockBid(bidder1, 900_000, 1);
        _finalize(_winners(bidder1));

        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.FundsRefunded(bytes32(0), worldwideDay1, bidder1, locked - _paidAtClearing(1));
        escrow.claimRefund(worldwideDay1, bidder1);
    }

    function test_Events_AuctionEscrowFinalized() public {
        uint128 locked = _lockBid(bidder1, 800_000, 5);
        uint128 paid = _paidAtClearing(5);

        vm.expectEmit(true, true, false, true);
        emit IEscrowAdapter.AuctionEscrowFinalized(RECEIVE_ID, worldwideDay1, locked - paid, paid, 1);
        _finalize(_winners(bidder1));
    }

    // --- Payment Token Rotation Tests ---
    function test_Wire_RotatePaymentToken_AllowedWhenNoLocks() public {
        // Swap active token when no locks are held.
        MockWCOEN rotated = new MockWCOEN();
        vm.prank(admin);
        escrow.wire(auction, address(compact), address(rotated));

        assertEq(address(escrow.paymentToken()), address(rotated));
    }

    function test_Wire_RewireSameTokenStaysAllowedWithLocks() public {
        // Active locks must not block re-wiring with the same token (e.g. rotating the auction).
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);

        address newAuction = address(0xBEEF);
        vm.prank(admin);
        escrow.wire(newAuction, address(compact), address(paymentToken));
        assertEq(escrow.intexAuctionContract(), newAuction);
    }

    // --- claimRefund ---

    function test_ClaimRefund_AfterDelay_Succeeds() public {
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);

        uint256 balanceBefore = paymentToken.balanceOf(bidder1);

        vm.warp(block.timestamp + escrow.UNFINALIZED_REFUND_DELAY());

        // Permissionless caller (an outsider) triggers the refund; funds go to bidder1.
        // claimRefund is not bridge-triggered, so the emitted receiveId is the zero sentinel.
        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.FundsRefunded(bytes32(0), worldwideDay1, bidder1, LOCK_AMOUNT);
        vm.prank(outsider);
        escrow.claimRefund(worldwideDay1, bidder1);

        assertEq(paymentToken.balanceOf(bidder1), balanceBefore + LOCK_AMOUNT);
        assertEq(uint8(escrow.getBidLock(worldwideDay1, bidder1).status), uint8(IEscrowAdapter.LockStatus.None));
    }

    function test_ClaimRefund_BeforeDelay_Reverts() public {
        uint32 lockedAt = uint32(block.timestamp);
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);

        // One second before the delay elapses.
        uint32 claimableAt = lockedAt + escrow.UNFINALIZED_REFUND_DELAY();
        vm.warp(claimableAt - 1);

        vm.expectRevert(
            abi.encodeWithSelector(IEscrowAdapter.RefundNotYetClaimable.selector, claimableAt, claimableAt - 1)
        );
        escrow.claimRefund(worldwideDay1, bidder1);
    }

    function test_ClaimRefund_NotLocked_Reverts() public {
        // No lock exists for bidder1.
        vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
        escrow.claimRefund(worldwideDay1, bidder1);
    }

    function test_ClaimRefund_DoubleClaim_Reverts() public {
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);
        vm.warp(block.timestamp + escrow.UNFINALIZED_REFUND_DELAY());

        escrow.claimRefund(worldwideDay1, bidder1);

        vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
        escrow.claimRefund(worldwideDay1, bidder1);
    }

    function test_ClaimRefund_ZeroBidder_Reverts() public {
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "bidder"));
        escrow.claimRefund(worldwideDay1, address(0));
    }

    function test_ClaimRefund_ForcedWithdrawalReturnsFalse_Reverts() public {
        vm.prank(auction);
        escrow.lockFunds(worldwideDay1, bidder1, LOCK_AMOUNT, 1_000_000, 1);
        vm.warp(block.timestamp + escrow.UNFINALIZED_REFUND_DELAY());

        // The Compact's forced withdrawal returns false (e.g. reset period not elapsed); the
        // adapter must surface this as the dedicated ForcedWithdrawalFailed, not a generic error.
        compact.setForcedWithdrawalShouldFail(true);

        vm.expectRevert(IEscrowAdapter.ForcedWithdrawalFailed.selector);
        escrow.claimRefund(worldwideDay1, bidder1);
    }

    function test_ClaimRefund_FinalizedDay_NeedsNoDelay() public {
        uint128 locked = _lockBid(bidder1, 500_000, 1);
        _finalize(new address[](0));

        uint256 balanceBefore = paymentToken.balanceOf(bidder1);
        escrow.claimRefund(worldwideDay1, bidder1);

        assertEq(paymentToken.balanceOf(bidder1) - balanceBefore, locked, "claimed before the unfinalized delay");
    }

    /// @dev A winner the chunk had to skip keeps its whole lock, and the closed day owes it back in full.
    function test_ClaimRefund_SkippedWinner_RefundsFullPrincipal() public {
        uint128 locked = _lockBid(bidder1, 500_000, 1);
        _finalize(_winners(bidder1));

        uint256 balanceBefore = paymentToken.balanceOf(bidder1);
        escrow.claimRefund(worldwideDay1, bidder1);

        assertEq(paymentToken.balanceOf(bidder1) - balanceBefore, locked, "full principal returned");
        (,, uint128 totalLocked) = escrow.getAuctionStatus(worldwideDay1);
        assertEq(totalLocked, 0, "the day's total releases with it");
    }

    // --- message-id threading ---

    /// @dev A finalize call stamps its inbound bridge message id onto every event it emits, so an indexer can
    ///      attribute the whole chunk to one cross-chain packet.
    function test_GuidThreading_AllFinalizeEvents_CarryPacketGuid() public {
        bytes32 packet = keccak256("inbound-packet-A");
        uint128 locked = _lockBid(bidder2, 900_000, 1);
        uint128 paid = _paidAtClearing(1);
        address[] memory winners = _winners(bidder1, bidder2);

        vm.expectEmit(true, true, true, true);
        emit IEscrowAdapter.BidderRefundFailed(
            packet, worldwideDay1, bidder1, abi.encodeWithSelector(IEscrowAdapter.LockNotActive.selector)
        );
        vm.expectEmit(true, true, false, true);
        emit IEscrowAdapter.AuctionEscrowFinalized(packet, worldwideDay1, locked - paid, paid, 2);

        vm.prank(bridger);
        escrow.finalizeAuction(worldwideDay1, packet, winners, 0, 0, CLEARING_RATE, BASIS, true);
    }
}
