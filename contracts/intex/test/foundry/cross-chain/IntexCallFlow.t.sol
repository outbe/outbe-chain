// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {OriginRouter} from "@contracts/origin/OriginRouter.sol";
import {IOriginRouter} from "@contracts/origin/interfaces/IOriginRouter.sol";
import {IntexNFT1155Bridge} from "@contracts/shared/IntexNFT1155Bridge.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";

import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";

/**
 * @title IntexCallFlowTest
 * @notice End-to-end Intex call flow: markCalled → system bridge → holders migrated to Outbe, over the ERC-7786
 *         loopback bridge.
 * @dev When Desis calls a series, the full cross-chain flow is:
 *      1. OriginRouter sends MARK_CALLED to BSC.
 *      2. TargetRouter marks the series as Called (transfers blocked).
 *      3. TargetRouter reads all holders and triggers `systemMultiSend` (funded from its own relay float).
 *      4. IntexNFT1155Bridge burns Intex on BSC and, once delivered, mints on Outbe.
 *      Delivery is manual: each `sendMessage` records the payload on the loopback bridge, which we then hand-deliver
 *      to the destination as the authenticated peer.
 */
contract IntexCallFlowTest is CrossChainTest {
    uint32 private constant BNB_CHAIN_ID = 1;
    uint32 private constant OUTBE_CHAIN_ID = 2;

    /// @dev Fee the loopback bridge charges; TargetRouter pays it for the holders bridge from its own float.
    uint256 private constant BRIDGE_FEE = 0.001 ether;

    // --- BSC contracts ---
    IntexNFT1155 private intexBnb;
    IntexAuction private auction;
    TargetRouter private targetRouter;
    IntexNFT1155Bridge private nftBridgeBnb;

    // --- Outbe contracts ---
    IntexNFT1155 private intexOutbe;
    OriginRouter private originRouter;
    IntexNFT1155Bridge private nftBridgeOutbe;

    address private admin = address(this);

    // Outbe roles
    address private desis;
    address private intexFactory;

    // Intex holders
    address private holder1 = address(0x10);
    address private holder2 = address(0x20);
    address private holder3 = address(0x30);

    uint32 private constant SERIES_ID = 20250101;
    uint256 private constant TOKEN_ID = uint256(SERIES_ID);
    uint32 private constant ISSUED_INTEX_COUNT = 10_000;

    function setUp() public {
        _setUpBridge();
        bridge.setFee(BRIDGE_FEE);

        vm.deal(admin, 1000 ether);

        // Stand-in Desis recipient that advertises IDesis via ERC-165 so OriginRouter.wire accepts it.
        desis = address(new MockDesis());
        vm.deal(desis, 1000 ether);
        intexFactory = makeAddr("factory");
        vm.deal(intexFactory, 1000 ether);

        // ---- Deploy BSC contracts ----
        intexBnb = DeployProxy.intexNFT1155(admin, admin);
        auction = DeployProxy.intexAuction(admin, admin);
        targetRouter = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        nftBridgeBnb = DeployProxy.intexNFT1155Bridge(address(intexBnb), address(bridge), admin);

        // ---- Deploy Outbe contracts ----
        intexOutbe = DeployProxy.intexNFT1155(admin, admin);
        originRouter = DeployProxy.originRouter(address(bridge), admin, BNB_CHAIN_ID);
        nftBridgeOutbe = DeployProxy.intexNFT1155Bridge(address(intexOutbe), address(bridge), admin);

        // ---- Register remote messengers ----
        targetRouter.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(originRouter)));
        originRouter.setRemoteMessenger(BNB_CHAIN_ID, _interop(BNB_CHAIN_ID, address(targetRouter)));
        nftBridgeBnb.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(nftBridgeOutbe)));
        nftBridgeOutbe.setRemoteMessenger(BNB_CHAIN_ID, _interop(BNB_CHAIN_ID, address(nftBridgeBnb)));

        // ---- Wire contract dependencies ----
        targetRouter.wire(address(auction), address(intexBnb), admin, address(nftBridgeBnb));
        originRouter.wire(desis, intexFactory);

        // ---- Register BNB target + seed the day snapshot (mark-called broadcasts over it) ----
        originRouter.addTarget(BNB_CHAIN_ID);
        vm.deal(address(originRouter), 100 ether);
        _seedDaySnapshot(SERIES_ID);

        // ---- Grant roles ----
        auction.grantRole(auction.RELAYER_ROLE(), address(targetRouter));
        intexBnb.grantRole(intexBnb.RELAYER_ROLE(), address(targetRouter));
        intexBnb.grantRole(intexBnb.RELAYER_ROLE(), address(nftBridgeBnb));
        // The system bridge runs while the series is Called; both batch adapters need SYSTEM_RELAYER_ROLE on their
        // local Intex to crosschainBurn/crosschainMint during that window.
        intexBnb.grantRole(intexBnb.SYSTEM_RELAYER_ROLE(), address(nftBridgeBnb));
        nftBridgeBnb.grantRole(nftBridgeBnb.SYSTEM_RELAYER_ROLE(), address(targetRouter));
        intexOutbe.grantRole(intexOutbe.RELAYER_ROLE(), address(nftBridgeOutbe));
        intexOutbe.grantRole(intexOutbe.SYSTEM_RELAYER_ROLE(), address(nftBridgeOutbe));

        // ---- Pre-fund TargetRouter's float: it pays the systemMultiSend bridge fee ----
        vm.deal(address(targetRouter), 100 ether);
    }

    /// @dev Create the series on a given Intex contract with the shared default parameters.
    function _createSeries(IntexNFT1155 intex) internal {
        intex.createSeries(CreateSeriesLib.params(SERIES_ID, ISSUED_INTEX_COUNT, uint32(21 days)));
    }

    /// @dev Fire a minimal STAGE_START (as the DESIS_ROLE holder) so the day's target snapshot exists for the
    ///      later mark-called/qualified broadcasts, which fan out over that frozen snapshot.
    function _seedDaySnapshot(uint32 day) internal {
        IOriginRouter.AuctionStageStartParams memory p;
        p.worldwideDay = day;
        vm.prank(desis);
        originRouter.sendAuctionStageStart(p);
    }

    // ============================================================
    // Phase 1: Call — pre-settlement bridge (BSC → Outbe)
    // ============================================================

    /// @notice Helper: triggers markCalled from the intexFactory and hand-delivers both bridge messages.
    function _triggerCallAndBridge(uint32 seriesId, bool hasHolders) internal {
        // Desis applies markCalled locally on Outbe before sending the notice to BSC.
        // Without it, the system bridge crosschainMint on Outbe would land in `Issued` and revert.
        if (hasHolders) {
            intexOutbe.markCalled(seriesId);
        }

        // 1. OriginRouter sends MARK_CALLED → BSC. Record the payload for hand-delivery.
        uint256 fee = originRouter.quoteSendMarkCalled(seriesId);
        vm.prank(intexFactory);
        originRouter.sendMarkCalled{value: fee}(seriesId);
        bytes memory markCalledPayload = bridge.lastPayload();

        // 2. Deliver MARK_CALLED → TargetRouter._handleMarkCalled. If holders exist, this fires the
        //    systemMultiSend, whose SEND_MULTI payload the bridge records last.
        _deliver(OUTBE_CHAIN_ID, address(originRouter), address(targetRouter), markCalledPayload);

        // 3. Deliver SEND_MULTI → Outbe batch adapter (only if holders exist).
        if (hasHolders) {
            bytes memory sendMultiPayload = bridge.lastPayload();
            _deliver(BNB_CHAIN_ID, address(nftBridgeBnb), address(nftBridgeOutbe), sendMultiPayload);
        }
    }

    /// @notice Full call flow: 3 holders on BSC are migrated to Outbe with preserved balances.
    function test_call_migratesAllHolders() public {
        _createSeries(intexBnb);
        intexBnb.mint(holder1, 50, SERIES_ID);
        intexBnb.mint(holder2, 30, SERIES_ID);
        intexBnb.mint(holder3, 20, SERIES_ID);
        _createSeries(intexOutbe);

        // Verify initial state
        assertEq(intexBnb.balanceOf(holder1, TOKEN_ID), 50);
        assertEq(intexBnb.balanceOf(holder2, TOKEN_ID), 30);
        assertEq(intexBnb.balanceOf(holder3, TOKEN_ID), 20);
        assertEq(intexOutbe.balanceOf(holder1, TOKEN_ID), 0);
        assertEq(intexOutbe.balanceOf(holder2, TOKEN_ID), 0);
        assertEq(intexOutbe.balanceOf(holder3, TOKEN_ID), 0);

        uint32 calledAt = uint32(block.timestamp);
        _triggerCallAndBridge(SERIES_ID, true);

        // BSC: all tokens burned
        assertEq(intexBnb.balanceOf(holder1, TOKEN_ID), 0, "holder1 BSC balance should be 0");
        assertEq(intexBnb.balanceOf(holder2, TOKEN_ID), 0, "holder2 BSC balance should be 0");
        assertEq(intexBnb.balanceOf(holder3, TOKEN_ID), 0, "holder3 BSC balance should be 0");
        assertEq(intexBnb.totalSupply(TOKEN_ID), 0, "BSC total supply should be 0");

        // Outbe: all tokens minted
        assertEq(intexOutbe.balanceOf(holder1, TOKEN_ID), 50, "holder1 Outbe balance should be 50");
        assertEq(intexOutbe.balanceOf(holder2, TOKEN_ID), 30, "holder2 Outbe balance should be 30");
        assertEq(intexOutbe.balanceOf(holder3, TOKEN_ID), 20, "holder3 Outbe balance should be 20");
        assertEq(intexOutbe.totalSupply(TOKEN_ID), 100, "Outbe total supply should be 100");

        // BSC series state is Called and deadline is derived from calledAt + the series callPeriod
        IIntexNFT1155.SeriesData memory dataBnb = intexBnb.readData(SERIES_ID);
        assertEq(uint8(dataBnb.state), uint8(IIntexNFT1155.IntexState.Called));
        assertEq(dataBnb.calledAt, calledAt);
        assertGt(dataBnb.callTrigger.intexCallPeriod, 0);
    }

    /// @notice Single holder migration.
    function test_call_singleHolder() public {
        _createSeries(intexBnb);
        intexBnb.mint(holder1, 100, SERIES_ID);
        _createSeries(intexOutbe);

        _triggerCallAndBridge(SERIES_ID, true);

        assertEq(intexBnb.balanceOf(holder1, TOKEN_ID), 0);
        assertEq(intexOutbe.balanceOf(holder1, TOKEN_ID), 100);
    }

    /// @notice Call on a series with no holders — markCalled applied, no bridge message needed.
    function test_call_noHolders() public {
        _createSeries(intexBnb);

        _triggerCallAndBridge(SERIES_ID, false);

        IIntexNFT1155.SeriesData memory data = intexBnb.readData(SERIES_ID);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Called));
    }

    /// @notice Outbe holder tracking is correct after call bridge.
    function test_call_holderTrackingOnOutbe() public {
        _createSeries(intexBnb);
        intexBnb.mint(holder1, 60, SERIES_ID);
        intexBnb.mint(holder2, 40, SERIES_ID);
        _createSeries(intexOutbe);

        _triggerCallAndBridge(SERIES_ID, true);

        assertEq(intexOutbe.seriesHolderCount(TOKEN_ID), 2);

        (address[] memory holders, uint256[] memory balances) = intexOutbe.getSeriesHoldersWithBalances(TOKEN_ID);
        assertEq(holders.length, 2);
        assertEq(holders[0], holder1);
        assertEq(holders[1], holder2);
        assertEq(balances[0], 60);
        assertEq(balances[1], 40);
    }

    /// @notice BSC holder tracking cleared after call bridge (all crosschainBurned).
    function test_call_holderTrackingClearedOnBsc() public {
        _createSeries(intexBnb);
        intexBnb.mint(holder1, 70, SERIES_ID);
        intexBnb.mint(holder2, 30, SERIES_ID);
        _createSeries(intexOutbe);

        assertEq(intexBnb.seriesHolderCount(TOKEN_ID), 2);

        _triggerCallAndBridge(SERIES_ID, true);

        assertEq(intexBnb.seriesHolderCount(TOKEN_ID), 0);
        assertEq(intexBnb.balanceOf(holder1, TOKEN_ID), 0);
        assertEq(intexBnb.balanceOf(holder2, TOKEN_ID), 0);
    }
}
