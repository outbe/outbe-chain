// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IEscrowAdapter} from "../../contracts/bnb/interfaces/IEscrowAdapter.sol";

/**
 * @title MockEscrowAdapter
 * @notice Mock implementation of IEscrowAdapter for testing. State is keyed by `seriesId`.
 */
contract MockEscrowAdapter is IEscrowAdapter {
    /// @notice ISO 4217 numeric alias of the payment-token class (840 = USD).
    uint16 public constant override PAYMENT_TOKEN_ALIAS = 840;

    // Escrow state, keyed by seriesId
    mapping(uint32 => mapping(address => BidLock)) private _bidLocks;
    mapping(uint32 => AuctionEscrowState) private _auctionEscrowState;

    // Mirrored production dependency, set by wire()
    address public mockPaymentToken;

    // --- Admin ---
    function wire(address, address, address, address _paymentToken) external override {
        mockPaymentToken = _paymentToken;
    }

    // --- Auction Integration ---
    function lockFunds(uint32 seriesId, address bidder, uint64 amount) external override {
        if (bidder == address(0)) revert ZeroAddress("bidder");
        if (amount == 0) revert ZeroValue("amount");
        if (_bidLocks[seriesId][bidder].status == LockStatus.Locked) revert BidAlreadyLocked();

        _bidLocks[seriesId][bidder] = BidLock({
            lockedAmount: amount,
            lockedAt: uint32(block.timestamp),
            status: LockStatus.Locked,
            failedRefund: 0,
            splitRecorded: false
        });

        AuctionEscrowState storage state = _auctionEscrowState[seriesId];
        state.totalLocked += amount;
        state.lockCount += 1;

        emit FundsLocked(seriesId, bidder, amount);
    }

    // --- Bridge Finalization ---
    function finalizeAuction(uint32 seriesId, bytes32 guid, FinalizationInstruction[] calldata instructions)
        external
        override
    {
        uint64 totalRefunded;
        uint64 totalPaid;

        for (uint256 i = 0; i < instructions.length; i++) {
            FinalizationInstruction calldata instr = instructions[i];
            BidLock storage lock = _bidLocks[seriesId][instr.bidder];

            if (lock.status != LockStatus.Locked) revert LockNotActive();

            lock.status = LockStatus.Finalized;
            totalRefunded += instr.refundedAmount;
            totalPaid += instr.paidAmount;

            if (instr.refundedAmount > 0) {
                emit FundsRefunded(guid, seriesId, instr.bidder, instr.refundedAmount);
            }
            if (instr.paidAmount > 0) {
                emit FundsClaimed(guid, seriesId, instr.bidder, instr.paidAmount);
            }
        }

        _auctionEscrowState[seriesId].finalized = true;

        emit AuctionEscrowFinalized(guid, seriesId, totalRefunded, totalPaid, uint32(instructions.length));
    }

    // --- Recovery ---
    function retryFinalize(uint32 seriesId, bytes32 guid, FinalizationInstruction calldata inst) external override {
        // Mock: no-op for tests that don't exercise the retry path.
        emit BidderRetried(guid, seriesId, inst.bidder, inst.refundedAmount, inst.paidAmount);
    }

    function claimRefund(uint32 seriesId, address bidder) external override {
        BidLock storage lock = _bidLocks[seriesId][bidder];
        if (lock.status != LockStatus.Locked) revert LockNotActive();

        uint64 amount = lock.lockedAmount;
        lock.status = LockStatus.Finalized;

        // Permissionless claim is not LZ-triggered: guid is the zero sentinel. See.
        emit FundsRefunded(bytes32(0), seriesId, bidder, amount);
    }

    function settleVaultOwed(uint32 seriesId, address bidder) external override {
        // Mock: no-op for tests that don't exercise the parked-vault-portion path.
        emit VaultOwedSettled(seriesId, bidder, 0);
    }

    // --- Views ---
    function getBidLock(uint32 seriesId, address bidder) external view override returns (BidLock memory lock) {
        return _bidLocks[seriesId][bidder];
    }

    function getAuctionStatus(uint32 seriesId)
        external
        view
        override
        returns (bool hasLocks, bool isFinalized, uint64 totalLocked)
    {
        AuctionEscrowState storage state = _auctionEscrowState[seriesId];
        return (state.lockCount > 0, state.finalized, state.totalLocked);
    }
}
