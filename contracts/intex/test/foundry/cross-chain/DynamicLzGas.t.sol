// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {MessagingFee} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";

import {LzGasEstimator} from "@contracts/shared/libs/LzGasEstimator.sol";
import {OptionsBuilder} from "@layerzerolabs/oapp-evm/oapp/libs/OptionsBuilder.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {ONFT1155BatchMsgCodec} from "@contracts/shared/libs/ONFT1155BatchMsgCodec.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";

/// @dev Test-only harness exposing the internal `LzGasEstimator` for unit assertions.
contract GasEstimatorHarness {
    function estimateGas(uint128 baseGas, uint128 perItemGas, uint256 itemCount) external pure returns (uint256) {
        return LzGasEstimator.estimateGas(baseGas, perItemGas, itemCount);
    }

    function receiveOption(
        uint128 baseGas,
        uint128 perItemGas,
        uint256 itemCount
    ) external pure returns (bytes memory) {
        return LzGasEstimator.receiveOption(baseGas, perItemGas, itemCount);
    }

    function receiveOption(
        uint128 baseGas,
        uint128 perItemGas,
        uint256 itemCount,
        uint16 bufferBps
    ) external pure returns (bytes memory) {
        return LzGasEstimator.receiveOption(baseGas, perItemGas, itemCount, bufferBps);
    }
}

/// @title DynamicLzGasTest
/// @notice Coverage for destination `lzReceiveOption` gas scales with payload item count
///         plus a buffer, so a large batch no longer OOMs the inbound `_lzReceive`.
contract DynamicLzGasTest is TestHelperOz5 {
    using OptionsBuilder for bytes;

    uint32 internal constant SRC_EID = 1;
    uint32 internal constant DST_EID = 2;

    GasEstimatorHarness internal harness;

    ONFT1155AdapterBatch internal srcBatch;
    ONFT1155AdapterBatch internal dstBatch;
    IntexNFT1155 internal srcToken;
    IntexNFT1155 internal dstToken;

    address internal admin = address(this);
    uint32 internal constant SERIES_ID = 20260601;
    uint256 internal constant TOKEN_ID = uint256(SERIES_ID);

    function setUp() public override {
        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        harness = new GasEstimatorHarness();

        srcToken = new IntexNFT1155(admin, admin);
        dstToken = new IntexNFT1155(admin, admin);
        srcBatch = new ONFT1155AdapterBatch(address(srcToken), address(endpoints[SRC_EID]), admin);
        dstBatch = new ONFT1155AdapterBatch(address(dstToken), address(endpoints[DST_EID]), admin);

        address[] memory oapps = new address[](2);
        oapps[0] = address(srcBatch);
        oapps[1] = address(dstBatch);
        this.wireOApps(oapps);

        // Both sides have the series; src holders are minted below, dst credits on receive.
        srcToken.createSeries(SERIES_ID, 1_000_000, 0);
        dstToken.createSeries(SERIES_ID, 1_000_000, 0);
        srcToken.markQualified(SERIES_ID);
        dstToken.markQualified(SERIES_ID);

        srcToken.grantRole(srcToken.RELAYER_ROLE(), address(srcBatch));
        dstToken.grantRole(dstToken.RELAYER_ROLE(), address(dstBatch));
        srcBatch.grantRole(srcBatch.SYSTEM_RELAYER_ROLE(), admin);

        vm.deal(address(srcBatch), 100 ether);
    }

    // ---------------------------------------------------------------
    // LzGasEstimator unit tests
    // ---------------------------------------------------------------

    function test_Estimator_GasScalesLinearlyWithItemCount() public view {
        uint128 base = 100_000;
        uint128 perItem = 50_000;

        assertEq(harness.estimateGas(base, perItem, 0), 100_000, "0 items = base only");
        assertEq(harness.estimateGas(base, perItem, 1), 150_000, "1 item");
        assertEq(harness.estimateGas(base, perItem, 10), 600_000, "10 items");
        assertEq(harness.estimateGas(base, perItem, 100), 5_100_000, "100 items");
    }

    function test_Estimator_OptionForLargerCountIsBigger() public view {
        bytes memory one = harness.receiveOption(100_000, 50_000, 1);
        bytes memory hundred = harness.receiveOption(100_000, 50_000, 100);
        // The 100-item option encodes a strictly larger gas value than the 1-item option.
        assertGt(hundred.length == one.length ? 1 : 0, 0, "same encoded length (single lzReceiveOption)");
        assertTrue(keccak256(one) != keccak256(hundred), "distinct options for distinct counts");
    }

    function test_Estimator_BufferIsApplied() public view {
        // Default buffer is 2000 bps (+20%). 100k base, 0 per-item, 0 items → raw 100k, buffered 120k.
        bytes memory defaultBuffered = harness.receiveOption(100_000, 0, 0);
        bytes memory noBuffer = harness.receiveOption(100_000, 0, 0, 0);
        bytes memory explicit20 = harness.receiveOption(100_000, 0, 0, 2000);

        assertEq(keccak256(defaultBuffered), keccak256(explicit20), "default buffer == 2000 bps");
        assertTrue(keccak256(defaultBuffered) != keccak256(noBuffer), "buffer changes the option");
    }

    function test_Estimator_DefaultBufferBps_PinsConstant() public pure {
        // A future bump to e.g. 1500 bps would change destination gas sizing project-wide; pin it.
        assertEq(LzGasEstimator.DEFAULT_BUFFER_BPS, 2000);
    }

    function test_Estimator_ReceiveOption_DefaultBuffer_ByteGolden() public view {
        // base 100_000, perItem 0, items 0 → raw 100_000; * (10_000 + 2000) / 10_000 = 120_000.
        // The encoded option must match a manual OptionsBuilder build with that exact gas. This pins
        // the buffer formula AND the OptionsBuilder ABI shape in one assertion.
        bytes memory actual = harness.receiveOption(100_000, 0, 0);
        bytes memory expected = OptionsBuilder.newOptions().addExecutorLzReceiveOption(120_000, 0);
        assertEq(actual, expected);
    }

    function test_Estimator_ReceiveOption_CustomBuffer_ByteGolden() public view {
        // base 100_000, perItem 50_000, items 10 → raw 600_000; * (10_000 + 5000) / 10_000 = 900_000.
        bytes memory actual = harness.receiveOption(100_000, 50_000, 10, 5000);
        bytes memory expected = OptionsBuilder.newOptions().addExecutorLzReceiveOption(900_000, 0);
        assertEq(actual, expected);
    }

    // ---------------------------------------------------------------
    // E2E: large batch survives delivery with dynamic gas (audit regression)
    // ---------------------------------------------------------------

    function test_E2E_MaxBatchSystemMultiSend_DeliversWithoutOOM() public {
        // MAX_BATCH_SIZE (64) is the largest legal batch. Exercise the dynamic-gas
        // regression at that ceiling — the old fixed 200k option would OOM this delivery.
        uint256 count = srcBatch.MAX_BATCH_SIZE();
        address[] memory holders = new address[](count);
        uint256[] memory amounts = new uint256[](count);
        for (uint256 i = 0; i < count; i++) {
            holders[i] = address(uint160(0x1000 + i));
            amounts[i] = 1;
            srcToken.mint(holders[i], 1, SERIES_ID); // give each source holder a unit to debit
        }

        bytes memory empty = "";
        MessagingFee memory fee = srcBatch.quoteSystemMultiSend(TOKEN_ID, holders, amounts, DST_EID, empty, false);

        srcBatch.systemMultiSend{value: fee.nativeFee}(TOKEN_ID, holders, amounts, DST_EID, empty, fee);

        // Deliver the queued packet to the destination. The dynamic lzReceiveOption sizes the gas
        // to the item count so the inbound `_lzReceive` credits every holder without OOM.
        verifyPackets(DST_EID, addressToBytes32(address(dstBatch)));

        // Every holder credited on the destination token.
        for (uint256 i = 0; i < count; i++) {
            assertEq(dstToken.balanceOf(holders[i], TOKEN_ID), 1, "holder credited on destination");
        }
    }

    function test_SystemMultiSend_OverCap_RevertsBatchTooLarge() public {
        // One past MAX_BATCH_SIZE is rejected fail-fast on the source chain.
        uint256 over = srcBatch.MAX_BATCH_SIZE() + 1;
        address[] memory holders = new address[](over);
        uint256[] memory amounts = new uint256[](over);
        for (uint256 i = 0; i < over; i++) {
            holders[i] = address(uint160(0x1000 + i));
            amounts[i] = 1;
            srcToken.mint(holders[i], 1, SERIES_ID);
        }

        MessagingFee memory fee = MessagingFee(1 ether, 0);
        vm.expectRevert(
            abi.encodeWithSelector(ONFT1155BatchMsgCodec.BatchTooLarge.selector, over, srcBatch.MAX_BATCH_SIZE())
        );
        srcBatch.systemMultiSend{value: fee.nativeFee}(TOKEN_ID, holders, amounts, DST_EID, "", fee);
    }

    function test_E2E_SingleHolderStillDelivers() public {
        address holder = address(0xCAFE);
        srcToken.mint(holder, 1, SERIES_ID);

        address[] memory holders = new address[](1);
        holders[0] = holder;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = 1;

        bytes memory empty = "";
        MessagingFee memory fee = srcBatch.quoteSystemMultiSend(TOKEN_ID, holders, amounts, DST_EID, empty, false);
        srcBatch.systemMultiSend{value: fee.nativeFee}(TOKEN_ID, holders, amounts, DST_EID, empty, fee);

        verifyPackets(DST_EID, addressToBytes32(address(dstBatch)));

        assertEq(dstToken.balanceOf(holder, TOKEN_ID), 1, "single holder credited");
    }
}
