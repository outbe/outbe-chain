// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";

uint128 constant BASIS = 1_000_000;
uint32 constant CLEARING_RATE = 600_000;

/// @dev Randomized actions against EscrowAdapter across several concurrent series.
contract EscrowConservationHandler is Test {
    EscrowAdapter internal escrow;
    address internal auction;
    address internal bridger;
    address[] internal bidders;
    uint32[] internal worldwideDays;

    constructor(
        EscrowAdapter _escrow,
        address _auction,
        address _bridger,
        address[] memory _bidders,
        uint32[] memory _worldwideDays
    ) {
        escrow = _escrow;
        auction = _auction;
        bridger = _bridger;
        bidders = _bidders;
        worldwideDays = _worldwideDays;
    }

    function _series(uint256 seed) internal view returns (uint32) {
        return worldwideDays[bound(seed, 0, worldwideDays.length - 1)];
    }

    function _bidder(uint256 seed) internal view returns (address) {
        return bidders[bound(seed, 0, bidders.length - 1)];
    }

    function lock(uint256 seriesSeed, uint256 bidderSeed, uint256 rateSeed, uint256 quantitySeed) external {
        uint32 bidRate = uint32(bound(rateSeed, 1, 1_000_000));
        uint16 quantity = uint16(bound(quantitySeed, 1, 1_000));
        uint128 amount = uint128(BridgeMsgCodec.escrowAmount(quantity, BASIS, bidRate));
        vm.prank(auction);
        try escrow.lockFunds(_series(seriesSeed), _bidder(bidderSeed), amount, bidRate, quantity) {} catch {}
    }

    function finalize(uint256 seriesSeed, uint256 bidderSeed, uint256 wonSeed, bool completesDay) external {
        uint32 s = _series(seriesSeed);
        address b = _bidder(bidderSeed);
        address[] memory winners = new address[](1);
        winners[0] = b;
        uint16 quantity = escrow.getBidLock(s, b).quantity;
        uint16 partialWon = quantity > 1 ? uint16(bound(wonSeed, 0, quantity - 1)) : 0;
        vm.prank(bridger);
        try escrow.finalizeAuction(
            s, keccak256(abi.encode(s, b)), winners, 0, partialWon, CLEARING_RATE, BASIS, completesDay
        ) {}
            catch {}
    }

    function claim(uint256 seriesSeed, uint256 bidderSeed) external {
        try escrow.claimRefund(_series(seriesSeed), _bidder(bidderSeed)) {} catch {}
    }

    function lockBond(uint256 seriesSeed, uint256 bidderSeed, uint128 amountSeed) external {
        uint128 amount = uint128(bound(amountSeed, 1, 1_000_000e6));
        vm.prank(auction);
        try escrow.lockCommitBond(_series(seriesSeed), _bidder(bidderSeed), amount) {} catch {}
    }

    function releaseBond(uint256 seriesSeed, uint256 bidderSeed) external {
        vm.prank(auction);
        try escrow.releaseCommitBond(_series(seriesSeed), _bidder(bidderSeed)) {} catch {}
    }

    function claimAbandonedBond(uint256 seriesSeed, uint256 bidderSeed) external {
        try escrow.claimAbandonedCommitBond(_series(seriesSeed), _bidder(bidderSeed)) {} catch {}
    }

    function warp(uint256 secondsSeed) external {
        skip(bound(secondsSeed, 1 hours, 10 days));
    }
}

/// @dev The sum of every live series' `totalLocked` plus every live commit bond equals the single
///      pooled ERC6909 balance the adapter holds in The Compact, across randomized
///      lock/finalize/claim/bond actions.
contract EscrowAdapterConservationInvariantTest is StdInvariant, Test {
    EscrowAdapter internal escrow;
    MockTheCompact internal compact;
    MockWCOEN internal paymentToken;
    EscrowConservationHandler internal handler;

    address internal admin = address(1);
    address internal bridger = address(2);
    address internal auction = address(3);

    uint32[] internal worldwideDays;
    address[] internal bidders;

    function setUp() public {
        escrow = DeployProxy.escrowAdapter(admin, bridger);
        compact = new MockTheCompact();
        paymentToken = new MockWCOEN();

        vm.prank(admin);
        escrow.wire(auction, address(compact), address(paymentToken));
        vm.prank(admin);
        escrow.setProceedsRecipient(bridger);
        compact.setResetPeriodSeconds(0);

        bidders.push(address(0xB1));
        bidders.push(address(0xB2));
        bidders.push(address(0xB3));
        for (uint256 i = 0; i < bidders.length; i++) {
            paymentToken.mint(bidders[i], 1e30);
            vm.prank(bidders[i]);
            paymentToken.approve(address(escrow), type(uint256).max);
        }

        worldwideDays.push(1);
        worldwideDays.push(2);
        worldwideDays.push(3);

        handler = new EscrowConservationHandler(escrow, auction, bridger, bidders, worldwideDays);

        bytes4[] memory selectors = new bytes4[](7);
        selectors[0] = EscrowConservationHandler.lock.selector;
        selectors[1] = EscrowConservationHandler.finalize.selector;
        selectors[2] = EscrowConservationHandler.claim.selector;
        selectors[3] = EscrowConservationHandler.warp.selector;
        selectors[4] = EscrowConservationHandler.lockBond.selector;
        selectors[5] = EscrowConservationHandler.releaseBond.selector;
        selectors[6] = EscrowConservationHandler.claimAbandonedBond.selector;
        targetSelector(FuzzSelector({addr: address(handler), selectors: selectors}));
        targetContract(address(handler));
    }

    function invariant_pooledBalanceEqualsSumOfTotalLocked() public view {
        uint256 sumTotalLocked;
        for (uint256 i = 0; i < worldwideDays.length; i++) {
            (,, uint128 totalLocked) = escrow.getAuctionStatus(worldwideDays[i]);
            sumTotalLocked += totalLocked;
        }
        // Commit bonds share the pooled lockId with bid escrow but are accounted separately.
        uint256 sumBonds;
        for (uint256 i = 0; i < worldwideDays.length; i++) {
            for (uint256 j = 0; j < bidders.length; j++) {
                sumBonds += escrow.getCommitBond(worldwideDays[i], bidders[j]).amount;
            }
        }
        uint256 pooled = compact.balanceOf(address(escrow), escrow.lockId());
        assertEq(sumTotalLocked + sumBonds, pooled, "sum(totalLocked) + sum(bonds) != pooled Compact balance");
    }

    /// @dev A winner's payment left with the proceeds, so its lock still holds only the rest.
    function invariant_totalLockedIsWhatTheLocksStillHold() public view {
        for (uint256 i = 0; i < worldwideDays.length; i++) {
            uint256 held;
            for (uint256 j = 0; j < bidders.length; j++) {
                IEscrowAdapter.BidLock memory l = escrow.getBidLock(worldwideDays[i], bidders[j]);
                if (l.status == IEscrowAdapter.LockStatus.Locked) {
                    held += l.lockedAmount;
                } else if (l.status == IEscrowAdapter.LockStatus.Won) {
                    held += l.lockedAmount - BridgeMsgCodec.escrowAmount(l.quantity, BASIS, CLEARING_RATE);
                }
            }
            (,, uint128 totalLocked) = escrow.getAuctionStatus(worldwideDays[i]);
            assertEq(held, totalLocked, "totalLocked != what the day's locks still hold");
        }
    }
}
