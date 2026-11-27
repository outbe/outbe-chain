// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ReferenceCurrencyPriceLib} from "../utils/ReferenceCurrencyPriceLib.sol";
import {CrossChainTest} from "../utils/CrossChainTest.sol";
import {DeployProxy} from "../utils/DeployProxy.sol";

import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

/// @dev Selector of OZ `ReentrancyGuardReentrantCall()`, reverted by `nonReentrant` on re-entry.
bytes4 constant REENTRANCY_GUARD_REENTRANT_CALL = 0x3ee5aeb5;

/// @notice Stub Auction that, during the inbound STAGE_START dispatch, tries to re-enter the router's inbound
///         entry. The re-entry reverts iff `receiveFromOutbe` carries `nonReentrant`.
/// @dev Does NOT inherit `IIntexAuction`. The high-level call dispatches by selector, so
///      matching the `startAuction` signature here is sufficient. The probe is the Interchain Account
///      of Outbe too, so the guard (not the caller check) is what rejects the call, before the empty
///      message is read.
contract ReentrancyProbeAuction {
    address public router;
    bool public observed;
    bool public guardHeld;

    function setRouter(address router_) external {
        router = router_;
    }

    function startAuction(uint32, IIntexAuction.AuctionSchedule calldata, IIntexAuction.AuctionParams calldata)
        external
    {
        observed = true;
        (bool ok, bytes memory ret) =
            router.call(abi.encodeCall(IntexAuctionTargetRouter.receiveFromOutbe, (bytes(""))));
        guardHeld = !ok && ret.length >= 4 && bytes4(ret) == REENTRANCY_GUARD_REENTRANT_CALL;
    }
}

/// @title RouterReentrancyTest
/// @notice Behavioural test that `IntexAuctionTargetRouter.receiveFromOutbe` runs under OZ `nonReentrant`.
/// @dev A downstream callee (the auction) tries to re-enter the router's inbound entry mid-dispatch.
///      The re-entry reverts with `ReentrancyGuardReentrantCall` iff the `nonReentrant` modifier is
///      active on `receiveFromOutbe`.
contract RouterReentrancyTest is CrossChainTest {
    uint32 internal constant OUTBE_CHAIN_ID = 2;

    IntexAuctionTargetRouter internal bnbRouter;
    ReentrancyProbeAuction internal probeAuction;

    address internal admin = address(this);

    function setUp() public {
        _setUpBridge();

        probeAuction = new ReentrancyProbeAuction();
        // intex / escrow don't fire on STAGE_START, but the constructor rejects address(0). Reuse the
        // probe so every link is non-zero.
        address probe = address(probeAuction);
        bnbRouter = DeployProxy.targetRouter(
            address(bridge),
            admin,
            DeployProxy.TargetRouterLinks(OUTBE_CHAIN_ID, DeployProxy.WCOEN_BRIDGE, probe, probe, probe, probe)
        );
        probeAuction.setRouter(address(bnbRouter));
        _wireOutbeIca(bnbRouter, probe);
    }

    function test_TM_receiveFromOutbe_runsUnderNonReentrant() public {
        bytes memory packet = BridgeMsgCodec.encodeIntexAuctionStart(
            42, 100, 200, 300, 1e6, 5e6, ReferenceCurrencyPriceLib.one(840, 7e6, 4e6), 5, 6, 7, 3, 9e6
        );

        _deliver(OUTBE_CHAIN_ID, address(probeAuction), address(bnbRouter), packet);

        assertTrue(probeAuction.observed(), "TM auction callback never ran");
        assertTrue(probeAuction.guardHeld(), "IntexAuctionTargetRouter.receiveFromOutbe missing nonReentrant");
    }
}
