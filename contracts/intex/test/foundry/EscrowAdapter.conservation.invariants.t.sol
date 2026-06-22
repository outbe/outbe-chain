// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {IVaultProvider} from "@contracts/vendor/outbe-vault/interfaces/IVaultProvider.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockERC20} from "@test-mocks/MockERC20.sol";
import {MockSettlementVault} from "@test-mocks/MockSettlementVault.sol";
import {MockVaultProvider} from "@test-mocks/MockVaultProvider.sol";

/// @dev Randomized actions against EscrowAdapter across several concurrent series.
contract EscrowConservationHandler is Test {
    EscrowAdapter internal escrow;
    address internal auction;
    address internal bridger;
    address[] internal bidders;
    uint32[] internal seriesIds;

    constructor(
        EscrowAdapter _escrow,
        address _auction,
        address _bridger,
        address[] memory _bidders,
        uint32[] memory _seriesIds
    ) {
        escrow = _escrow;
        auction = _auction;
        bridger = _bridger;
        bidders = _bidders;
        seriesIds = _seriesIds;
    }

    function _series(uint256 seed) internal view returns (uint32) {
        return seriesIds[bound(seed, 0, seriesIds.length - 1)];
    }

    function _bidder(uint256 seed) internal view returns (address) {
        return bidders[bound(seed, 0, bidders.length - 1)];
    }

    function lock(uint256 seriesSeed, uint256 bidderSeed, uint64 amountSeed) external {
        uint64 amount = uint64(bound(amountSeed, 1, 1_000_000e6));
        vm.prank(auction);
        try escrow.lockFunds(_series(seriesSeed), _bidder(bidderSeed), amount) {} catch {}
    }

    function finalize(uint256 seriesSeed, uint256 bidderSeed, uint64 refundSeed) external {
        uint32 s = _series(seriesSeed);
        address b = _bidder(bidderSeed);
        IEscrowAdapter.BidLock memory l = escrow.getBidLock(s, b);
        uint64 refunded = l.lockedAmount == 0 ? 0 : uint64(bound(refundSeed, 0, l.lockedAmount));
        IEscrowAdapter.FinalizationInstruction[] memory ins = new IEscrowAdapter.FinalizationInstruction[](1);
        ins[0] = IEscrowAdapter.FinalizationInstruction({
            bidder: b, refundedAmount: refunded, paidAmount: l.lockedAmount - refunded
        });
        vm.prank(bridger);
        try escrow.finalizeAuction(s, keccak256(abi.encode(s, b)), ins) {} catch {}
    }

    function retry(uint256 seriesSeed, uint256 bidderSeed, uint64 refundSeed) external {
        uint32 s = _series(seriesSeed);
        address b = _bidder(bidderSeed);
        IEscrowAdapter.BidLock memory l = escrow.getBidLock(s, b);
        uint64 refunded = l.lockedAmount == 0 ? 0 : uint64(bound(refundSeed, 0, l.lockedAmount));
        IEscrowAdapter.FinalizationInstruction memory inst = IEscrowAdapter.FinalizationInstruction({
            bidder: b, refundedAmount: refunded, paidAmount: l.lockedAmount - refunded
        });
        vm.prank(bridger);
        try escrow.retryFinalize(s, keccak256(abi.encode(s, b)), inst) {} catch {}
    }

    function claim(uint256 seriesSeed, uint256 bidderSeed) external {
        try escrow.claimRefund(_series(seriesSeed), _bidder(bidderSeed)) {} catch {}
    }

    function settleOwed(uint256 seriesSeed, uint256 bidderSeed) external {
        try escrow.settleVaultOwed(_series(seriesSeed), _bidder(bidderSeed)) {} catch {}
    }

    function warp(uint256 secondsSeed) external {
        skip(bound(secondsSeed, 1 hours, 10 days));
    }
}

/// @dev The sum of every live series' `totalLocked` equals the single pooled ERC6909 balance
///      the adapter holds in The Compact, across randomized lock/finalize/claim/settle actions.
contract EscrowAdapterConservationInvariantTest is StdInvariant, Test {
    EscrowAdapter internal escrow;
    MockTheCompact internal compact;
    MockERC20 internal paymentToken;
    MockVaultProvider internal provider;
    EscrowConservationHandler internal handler;

    address internal admin = address(1);
    address internal bridger = address(2);
    address internal auction = address(3);

    uint32[] internal seriesIds;

    function setUp() public {
        escrow = DeployProxy.escrowAdapter(admin, bridger);
        compact = new MockTheCompact();
        paymentToken = new MockERC20("USD Coin", "USDC", 6);
        MockSettlementVault vault = new MockSettlementVault(address(paymentToken), "Mock Vault USDC", "mvUSDC", 6);
        provider = new MockVaultProvider();
        provider.addVault(vault);
        provider.addLiquiditySource(address(escrow), IVaultProvider.LiquiditySource.IntexBidPrice);

        vm.prank(admin);
        escrow.wire(auction, address(compact), address(provider), address(paymentToken));
        compact.setResetPeriodSeconds(0);

        address[] memory bidders = new address[](3);
        bidders[0] = address(0xB1);
        bidders[1] = address(0xB2);
        bidders[2] = address(0xB3);
        for (uint256 i = 0; i < bidders.length; i++) {
            paymentToken.mint(bidders[i], 1e24);
            vm.prank(bidders[i]);
            paymentToken.approve(address(escrow), type(uint256).max);
        }

        seriesIds.push(1);
        seriesIds.push(2);
        seriesIds.push(3);

        handler = new EscrowConservationHandler(escrow, auction, bridger, bidders, seriesIds);

        bytes4[] memory selectors = new bytes4[](6);
        selectors[0] = EscrowConservationHandler.lock.selector;
        selectors[1] = EscrowConservationHandler.finalize.selector;
        selectors[2] = EscrowConservationHandler.retry.selector;
        selectors[3] = EscrowConservationHandler.claim.selector;
        selectors[4] = EscrowConservationHandler.settleOwed.selector;
        selectors[5] = EscrowConservationHandler.warp.selector;
        targetSelector(FuzzSelector({addr: address(handler), selectors: selectors}));
        targetContract(address(handler));
    }

    function invariant_pooledBalanceEqualsSumOfTotalLocked() public view {
        uint256 sumTotalLocked;
        for (uint256 i = 0; i < seriesIds.length; i++) {
            (,, uint64 totalLocked) = escrow.getAuctionStatus(seriesIds[i]);
            sumTotalLocked += totalLocked;
        }
        uint256 pooled = compact.balanceOf(address(escrow), escrow.lockId());
        assertEq(sumTotalLocked, pooled, "sum(totalLocked) != pooled Compact balance");
    }
}
