// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {ITargetRouter} from "@contracts/target/interfaces/ITargetRouter.sol";
import {OriginRouter} from "@contracts/origin/OriginRouter.sol";
import {IOriginRouter} from "@contracts/origin/interfaces/IOriginRouter.sol";
import {VwapRegistry} from "@contracts/target/VwapRegistry.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {InboundReason} from "@contracts/shared/libs/InboundReason.sol";
import {IntexGas} from "@contracts/shared/libs/IntexGas.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";
import {IAccessControl} from "@openzeppelin/contracts/access/IAccessControl.sol";

/// @dev One finalized day's VWAPs leave the origin once per target and land in that target's registry.
contract DailyVwapWireTest is CrossChainTest {
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant OUTBE_CHAIN_ID = 2;
    uint32 internal constant UTC_DAY = 20260315;
    bytes4 internal constant GAS_SELECTOR = bytes4(keccak256("executionGasLimit(uint256)"));

    TargetRouter internal bnbRouter;
    OriginRouter internal outbeRouter;
    VwapRegistry internal registry;

    address internal admin = address(this);
    address internal intexFactory = makeAddr("factory");

    function setUp() public {
        _setUpBridge();

        bnbRouter = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        outbeRouter = DeployProxy.originRouter(address(bridge), admin);
        registry = DeployProxy.vwapRegistry(admin, address(bnbRouter));

        bnbRouter.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(outbeRouter)));
        outbeRouter.setRemoteMessenger(BNB_CHAIN_ID, _interop(BNB_CHAIN_ID, address(bnbRouter)));
        bnbRouter.setVwapRegistry(address(registry));

        outbeRouter.wire(address(new MockDesis()), intexFactory);
        outbeRouter.addTarget(BNB_CHAIN_ID);
    }

    function _rows(uint256 count) internal pure returns (IOriginRouter.DailyVwap[] memory rows) {
        rows = new IOriginRouter.DailyVwap[](count);
        for (uint256 i = 0; i < count; ++i) {
            rows[i] = IOriginRouter.DailyVwap({isoCode: uint16(840 + i), vwapMinor: uint64(1_500_000 + i)});
        }
    }

    function test_ADayTravelsToTheTargetRegistry() public {
        vm.prank(intexFactory);
        outbeRouter.sendDailyVwap(UTC_DAY, _rows(2));

        vm.expectEmit(address(bnbRouter));
        emit ITargetRouter.DailyVwapReceived(OUTBE_CHAIN_ID, UTC_DAY, 2);
        _deliver(OUTBE_CHAIN_ID, address(outbeRouter), address(bnbRouter), bridge.lastPayload());

        assertEq(registry.vwapOf(UTC_DAY, 840), 1_500_000);
        assertEq(registry.vwapOf(UTC_DAY, 841), 1_500_001);
        assertEq(registry.lastUtcDay(840), UTC_DAY);
    }

    /// @dev The destination gas follows the row count, not the message type.
    function test_TheSendBuysDestinationGasForItsRows() public {
        vm.startPrank(intexFactory);
        outbeRouter.sendDailyVwap(UTC_DAY, _rows(1));
        _assertLastGas(IntexGas.dailyVwap(1));
        outbeRouter.sendDailyVwap(UTC_DAY, _rows(BridgeMsgCodec.MAX_REFERENCE_PRICES));
        _assertLastGas(IntexGas.dailyVwap(BridgeMsgCodec.MAX_REFERENCE_PRICES));
        vm.stopPrank();
    }

    /// @dev The origin's own NFT reads the Oracle, so a chain that targets itself sends it nothing.
    function test_TheOriginSkipsItself() public {
        outbeRouter.setRemoteMessenger(uint32(block.chainid), _interop(uint32(block.chainid), address(outbeRouter)));
        outbeRouter.addTarget(uint32(block.chainid));

        vm.recordLogs();
        vm.prank(intexFactory);
        outbeRouter.sendDailyVwap(UTC_DAY, _rows(1));

        assertEq(_countTopic(IOriginRouter.DailyVwapSent.selector), 1, "only the other target is sent to");
        assertEq(bridge.lastRecipient(), _interop(BNB_CHAIN_ID, address(bnbRouter)));
    }

    function test_OnlyTheFactoryMaySendADay() public {
        vm.expectRevert(
            abi.encodeWithSelector(
                IAccessControl.AccessControlUnauthorizedAccount.selector,
                address(this),
                outbeRouter.INTEX_FACTORY_ROLE()
            )
        );
        outbeRouter.sendDailyVwap(UTC_DAY, _rows(1));
    }

    /// @dev Acknowledged rather than held in redelivery.
    function test_ATargetWithoutARegistryAcknowledgesTheDay() public {
        TargetRouter bare = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        bare.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(outbeRouter)));
        bytes memory packet = BridgeMsgCodec.encodeDailyVwap(UTC_DAY, _rows(1));

        vm.expectEmit(address(bare));
        emit ITargetRouter.InboundMessageIgnored(
            OUTBE_CHAIN_ID, BridgeMsgCodec.MSG_DAILY_VWAP, bytes32(uint256(UTC_DAY)), InboundReason.OBSOLETE
        );
        _deliver(OUTBE_CHAIN_ID, address(outbeRouter), address(bare), packet);
    }

    function test_OnlyTheAdminSetsTheRegistry() public {
        address stranger = makeAddr("stranger");
        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(IAccessControl.AccessControlUnauthorizedAccount.selector, stranger, bytes32(0))
        );
        bnbRouter.setVwapRegistry(stranger);

        vm.expectRevert(abi.encodeWithSelector(ITargetRouter.ZeroAddress.selector, "vwapRegistry"));
        bnbRouter.setVwapRegistry(address(0));

        vm.expectEmit(address(bnbRouter));
        emit ITargetRouter.VwapRegistrySet(address(registry));
        bnbRouter.setVwapRegistry(address(registry));
        assertEq(address(bnbRouter.vwapRegistry()), address(registry));
    }

    function _assertLastGas(uint256 expectedGas) internal view {
        bytes[] memory attrs = bridge.getLastAttributes();
        assertEq(attrs.length, 1, "expected one attribute");
        assertEq(attrs[0], abi.encodeWithSelector(GAS_SELECTOR, expectedGas), "gas attribute mismatch");
    }
}
