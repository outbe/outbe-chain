// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {TargetMessenger} from "@contracts/bnb/TargetMessenger.sol";
import {OriginMessenger} from "@contracts/outbe/OriginMessenger.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";

import {IntexAuction} from "@contracts/bnb/IntexAuction.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";

import {MessagingFee} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";
import {OptionsBuilder} from "@layerzerolabs/oapp-evm/oapp/libs/OptionsBuilder.sol";
import {EnforcedOptionParam} from "@layerzerolabs/oapp-evm/oapp/interfaces/IOAppOptionsType3.sol";
import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";

/**
 * @title IntexCallFlowTest
 * @notice End-to-end Intex call flow: markCalled → system bridge → holders migrated to Outbe.
 * @dev When Desis calls a series, the full cross-chain flow is:
 *      1. OriginMessenger sends markCalled via LZ to BSC
 *      2. TargetMessenger marks series as Called (transfers blocked)
 *      3. TargetMessenger reads all holders and triggers systemMultiSend
 *      4. ONFT1155AdapterBatch burns Intex on BSC, mints on Outbe
 *      After this flow, all Intex holders are on Outbe and ready to settle.
 */
contract IntexCallFlowTest is TestHelperOz5 {
    using OptionsBuilder for bytes;

    uint32 private bnbEid = 1;
    uint32 private outbeEid = 2;

    // --- BSC contracts ---
    IntexNFT1155 private intexBnb;
    IntexAuction private auction;
    TargetMessenger private bnbAdapter;
    ONFT1155AdapterBatch private batchAdapterBnb;

    // --- Outbe contracts ---
    IntexNFT1155 private intexOutbe;
    OriginMessenger private outbeAdapter;
    ONFT1155AdapterBatch private batchAdapterOutbe;

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

    function setUp() public virtual override {
        vm.deal(admin, 1000 ether);

        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        // Stand-in Desis recipient that advertises IDesis via ERC-165 so OriginMessenger.wire accepts it.
        desis = address(new MockDesis());
        vm.deal(desis, 1000 ether);
        intexFactory = makeAddr("factory");
        vm.deal(intexFactory, 1000 ether);

        // ---- Deploy BSC contracts ----
        intexBnb = DeployProxy.intexNFT1155(admin, admin);
        auction = DeployProxy.intexAuction(admin, admin);

        bnbAdapter = DeployProxy.targetMessenger(address(endpoints[bnbEid]), admin, outbeEid);

        batchAdapterBnb = ONFT1155AdapterBatch(
            payable(_deployOApp(
                    type(ONFT1155AdapterBatch).creationCode,
                    abi.encode(address(intexBnb), address(endpoints[bnbEid]), admin)
                ))
        );

        // ---- Deploy Outbe contracts ----
        intexOutbe = DeployProxy.intexNFT1155(admin, admin);

        outbeAdapter = DeployProxy.originMessenger(address(endpoints[outbeEid]), admin, bnbEid);

        batchAdapterOutbe = ONFT1155AdapterBatch(
            payable(_deployOApp(
                    type(ONFT1155AdapterBatch).creationCode,
                    abi.encode(address(intexOutbe), address(endpoints[outbeEid]), admin)
                ))
        );

        // ---- Wire LZ peers ----
        address[] memory bridgeOapps = new address[](2);
        bridgeOapps[0] = address(bnbAdapter);
        bridgeOapps[1] = address(outbeAdapter);
        this.wireOApps(bridgeOapps);

        address[] memory batchOapps = new address[](2);
        batchOapps[0] = address(batchAdapterBnb);
        batchOapps[1] = address(batchAdapterOutbe);
        this.wireOApps(batchOapps);

        // ---- Wire contract dependencies ----
        bnbAdapter.wire(address(auction), address(intexBnb), admin, address(batchAdapterBnb));
        outbeAdapter.wire(desis, intexFactory);

        // ---- Grant roles ----
        auction.grantRole(auction.RELAYER_ROLE(), address(bnbAdapter));
        intexBnb.grantRole(intexBnb.RELAYER_ROLE(), address(bnbAdapter));
        intexBnb.grantRole(intexBnb.RELAYER_ROLE(), address(batchAdapterBnb));
        // The system bridge runs while the series is Called; both batch adapters need
        // SYSTEM_RELAYER_ROLE on their local Intex to debit/credit during that window.
        intexBnb.grantRole(intexBnb.SYSTEM_RELAYER_ROLE(), address(batchAdapterBnb));
        batchAdapterBnb.grantRole(batchAdapterBnb.SYSTEM_RELAYER_ROLE(), address(bnbAdapter));
        intexOutbe.grantRole(intexOutbe.RELAYER_ROLE(), address(batchAdapterOutbe));
        intexOutbe.grantRole(intexOutbe.SYSTEM_RELAYER_ROLE(), address(batchAdapterOutbe));

        // ---- Fund adapters for LZ fees ----
        vm.deal(address(bnbAdapter), 100 ether);
        vm.deal(address(batchAdapterBnb), 100 ether);

        // ---- Enforced options: SEND_MULTI from BSC → Outbe ----
        EnforcedOptionParam[] memory params = new EnforcedOptionParam[](1);
        params[0] = EnforcedOptionParam({
            eid: outbeEid,
            msgType: batchAdapterBnb.SEND_MULTI(),
            options: OptionsBuilder.newOptions().addExecutorLzReceiveOption(2_000_000, 0)
        });
        batchAdapterBnb.setEnforcedOptions(params);
    }

    /// @dev Create the series on a given Intex contract with the shared default parameters.
    function _createSeries(IntexNFT1155 intex) internal {
        intex.createSeries(SERIES_ID, ISSUED_INTEX_COUNT, 0);
    }

    // ============================================================
    // Phase 1: Call — pre-settlement bridge (BSC → Outbe)
    // ============================================================

    /// @notice Helper: triggers markCalled from Desis and delivers both LZ messages.
    function _triggerCallAndBridge(uint32 seriesId, bool hasHolders) internal {
        // Desis applies markCalled locally on Outbe before sending the LZ notice to BSC.
        // Without it, the system bridge credit on Outbe would land in `Issued` and revert.
        if (hasHolders) {
            intexOutbe.markCalled(seriesId);
        }

        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(2_000_000, 0);
        MessagingFee memory fee = outbeAdapter.quoteSendMarkCalled(seriesId, options, false);

        vm.prank(intexFactory);
        outbeAdapter.sendMarkCalled{value: fee.nativeFee}(seriesId, options, fee, intexFactory);

        // LZ delivers MSG_MARK_CALLED → TargetMessenger._handleMarkCalled
        verifyPackets(bnbEid, addressToBytes32(address(bnbAdapter)));

        // LZ delivers SEND_MULTI → Outbe batch adapter (only if holders exist)
        if (hasHolders) {
            verifyPackets(outbeEid, addressToBytes32(address(batchAdapterOutbe)));
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

        // BSC series state is Called and deadline is derived from calledAt + default callPeriod
        IIntexNFT1155.SeriesData memory dataBnb = intexBnb.readData(SERIES_ID);
        assertEq(uint8(dataBnb.state), uint8(IIntexNFT1155.IntexState.Called));
        assertEq(dataBnb.calledAt, calledAt);
        assertGt(dataBnb.intexCallPeriod, 0);
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

    /// @notice BSC holder tracking cleared after call bridge (all debited).
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
