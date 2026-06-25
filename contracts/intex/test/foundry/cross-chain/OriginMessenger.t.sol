// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {OriginMessenger} from "@contracts/origin/OriginMessenger.sol";
import {TargetMessenger} from "@contracts/target/TargetMessenger.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {IOriginMessenger} from "@contracts/origin/interfaces/IOriginMessenger.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";

import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";

import {MessagingFee, MessagingReceipt, Origin} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";
import {OptionsBuilder} from "@layerzerolabs/oapp-evm/oapp/libs/OptionsBuilder.sol";
import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";

/**
 * @title OriginMessengerTest
 * @notice Foundry tests for OriginMessenger
 * @dev Tests message encoding/decoding and access control.
 *      All auction/series messages are keyed by `seriesId` (uint32).
 */
contract OriginMessengerTest is TestHelperOz5 {
    using OptionsBuilder for bytes;

    uint32 private bnbEid = 1;
    uint32 private outbeEid = 2;

    OriginMessenger private outbeAdapter;
    TargetMessenger private bnbAdapter;
    ONFT1155AdapterBatch private batchAdapter;

    // Stand-in Desis recipient that advertises `IDesis` via ERC-165 so that
    // `OriginMessenger.wire`'s interface probe accepts it.
    address private desis;

    address private intexFactory;

    // Mock BNB contracts
    IntexAuction private auction;
    IntexNFT1155 private intex;

    address private admin = address(this);
    address private user = address(0x1);

    uint32 private constant SERIES_ID = 20250115; // yyyymmdd format

    function setUp() public virtual override {
        vm.deal(admin, 1000 ether);
        vm.deal(user, 1000 ether);

        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        // Stand-in Desis recipient — declared after super.setUp() so vm.deal targets a real address.
        desis = address(new MockDesis());
        vm.deal(desis, 1000 ether);
        intexFactory = makeAddr("factory");
        vm.deal(intexFactory, 1000 ether);

        // Deploy mock BNB contracts
        auction = DeployProxy.intexAuction(admin, admin);
        intex = DeployProxy.intexNFT1155(admin, admin);

        // Deploy Outbe adapter
        outbeAdapter = DeployProxy.originMessenger(address(endpoints[outbeEid]), admin, bnbEid);

        // Deploy BNB adapter (for cross-chain testing)
        bnbAdapter = DeployProxy.targetMessenger(address(endpoints[bnbEid]), admin, outbeEid);

        // Deploy batch adapter on BNB
        batchAdapter = DeployProxy.onftAdapterBatch(address(intex), address(endpoints[bnbEid]), admin);

        // Wire adapters (set peers)
        address[] memory oapps = new address[](2);
        oapps[0] = address(bnbAdapter);
        oapps[1] = address(outbeAdapter);
        this.wireOApps(oapps);

        // Wire Outbe adapter
        outbeAdapter.wire(desis, intexFactory);

        // Wire BNB adapter
        bnbAdapter.wire(address(auction), address(intex), admin, address(batchAdapter));
    }

    // --- Helpers ---
    /// @dev Build a baseline AuctionStageStartParams payload keyed by SERIES_ID.
    function _baseStageStartParams() internal view returns (IOriginMessenger.AuctionStageStartParams memory) {
        return IOriginMessenger.AuctionStageStartParams({
            seriesId: SERIES_ID,
            commitEnd: uint32(block.timestamp + 3600),
            revealEnd: uint32(block.timestamp + 5400),
            issuanceEnd: uint32(block.timestamp + 7200),
            issuanceCurrency: 840,
            referenceCurrency: 840,
            promisLoadMinor: 1000,
            minIntexBidRate: 50e6,
            entryPrice: 100e6,
            floorPriceMinor: 50e6,
            callPriceMinor: 25e6,
            intexCallPeriod: 0,
            callWindowDays: 0,
            callThresholdDays: 0,
            minIntexBidQuantity: 1
        });
    }

    function _baseIssuanceParams(address[] memory recipients, uint256[] memory quantities)
        internal
        pure
        returns (IOriginMessenger.IssuanceInstructionsParams memory)
    {
        return IOriginMessenger.IssuanceInstructionsParams({
            seriesId: SERIES_ID,
            issuedIntexCount: 10_000,
            promisLoadMinor: 1000,
            costAmountMinor: 100e6,
            entryPriceMinor: 100e6,
            floorPriceMinor: 50e6,
            intexCallPeriod: 0,
            issuanceCurrency: 840,
            referenceCurrency: 840,
            callWindowDays: 30,
            callThresholdDays: 5,
            callPriceMinor: 25e6,
            recipients: recipients,
            quantities: quantities
        });
    }

    // --- Constructor Tests ---
    function test_constructor() public view {
        assertEq(outbeAdapter.BNB_EID(), bnbEid);
        assertTrue(outbeAdapter.hasRole(outbeAdapter.DEFAULT_ADMIN_ROLE(), admin));
    }

    function test_wire() public view {
        assertEq(outbeAdapter.desis(), desis);
        assertTrue(outbeAdapter.hasRole(outbeAdapter.DESIS_ROLE(), desis));
    }

    function test_wire_revert_zero_address() public {
        OriginMessenger newAdapter = DeployProxy.originMessenger(address(endpoints[outbeEid]), admin, bnbEid);

        vm.expectRevert(abi.encodeWithSelector(IOriginMessenger.ZeroAddress.selector, "desis"));
        newAdapter.wire(address(0), intexFactory);
    }

    // --- Access Control Tests ---
    function test_sendAuctionStageStart_revert_unauthorized() public {
        MessagingFee memory fee = MessagingFee({nativeFee: 0.1 ether, lzTokenFee: 0});
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        vm.prank(user);
        vm.expectRevert();
        outbeAdapter.sendAuctionStageStart{value: 0.1 ether}(_baseStageStartParams(), options, fee, user);
    }

    function test_sendAuctionStageReveal_revert_unauthorized() public {
        MessagingFee memory fee = MessagingFee({nativeFee: 0.1 ether, lzTokenFee: 0});
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        vm.prank(user);
        vm.expectRevert();
        outbeAdapter.sendAuctionStageReveal{value: 0.1 ether}(SERIES_ID, true, options, fee, user);
    }

    function test_sendMarkCalled_revert_unauthorized() public {
        MessagingFee memory fee = MessagingFee({nativeFee: 0.1 ether, lzTokenFee: 0});
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        vm.prank(user);
        vm.expectRevert();
        outbeAdapter.sendMarkCalled{value: 0.1 ether}(SERIES_ID, options, fee, user);
    }

    function test_sendAuctionStageClearing_revert_unauthorized() public {
        MessagingFee memory fee = MessagingFee({nativeFee: 0.1 ether, lzTokenFee: 0});
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        vm.prank(user);
        vm.expectRevert();
        outbeAdapter.sendAuctionStageClearing{value: 0.1 ether}(SERIES_ID, options, fee, user);
    }

    function test_sendAuctionResult_revert_unauthorized() public {
        MessagingFee memory fee = MessagingFee({nativeFee: 0.1 ether, lzTokenFee: 0});
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        vm.prank(user);
        vm.expectRevert();
        outbeAdapter.sendAuctionResult{value: 0.1 ether}(SERIES_ID, 10_000, 100e6, 50, options, fee, user);
    }

    function test_sendIssuanceInstructions_revert_unauthorized() public {
        address[] memory recipients = new address[](1);
        recipients[0] = address(0x1);
        uint256[] memory quantities = new uint256[](1);
        quantities[0] = 1;

        MessagingFee memory fee = MessagingFee({nativeFee: 0.1 ether, lzTokenFee: 0});
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        vm.prank(user);
        vm.expectRevert();
        outbeAdapter.sendIssuanceInstructions{value: 0.1 ether}(
            _baseIssuanceParams(recipients, quantities), options, fee, user
        );
    }

    function test_sendRefundInstructions_revert_unauthorized() public {
        address[] memory bidders = new address[](1);
        bidders[0] = address(0x1);
        uint64[] memory refundedAmounts = new uint64[](1);
        refundedAmounts[0] = 100e6;
        uint64[] memory paidAmounts = new uint64[](1);
        paidAmounts[0] = 50e6;

        MessagingFee memory fee = MessagingFee({nativeFee: 0.1 ether, lzTokenFee: 0});
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        vm.prank(user);
        vm.expectRevert();
        outbeAdapter.sendRefundInstructions{value: 0.1 ether}(
            SERIES_ID, bidders, refundedAmounts, paidAmounts, options, fee, user
        );
    }

    function test_sendMarkQualified_revert_unauthorized() public {
        MessagingFee memory fee = MessagingFee({nativeFee: 0.1 ether, lzTokenFee: 0});
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        vm.prank(user);
        vm.expectRevert();
        outbeAdapter.sendMarkQualified{value: 0.1 ether}(SERIES_ID, options, fee, user);
    }

    // --- Validation Tests ---
    function test_sendIssuanceInstructions_revert_empty_array() public {
        address[] memory recipients = new address[](0);
        uint256[] memory quantities = new uint256[](0);

        MessagingFee memory fee = MessagingFee({nativeFee: 0.1 ether, lzTokenFee: 0});
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        vm.prank(intexFactory);
        vm.expectRevert(IOriginMessenger.EmptyArray.selector);
        outbeAdapter.sendIssuanceInstructions{value: 0.1 ether}(
            _baseIssuanceParams(recipients, quantities), options, fee, intexFactory
        );
    }

    function test_sendIssuanceInstructions_revert_array_length_mismatch() public {
        address[] memory recipients = new address[](2);
        uint256[] memory quantities = new uint256[](1); // Mismatch

        recipients[0] = address(0x1);
        recipients[1] = address(0x2);
        quantities[0] = 10;

        MessagingFee memory fee = MessagingFee({nativeFee: 0.1 ether, lzTokenFee: 0});
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        vm.prank(intexFactory);
        vm.expectRevert(IOriginMessenger.ArrayLengthMismatch.selector);
        outbeAdapter.sendIssuanceInstructions{value: 0.1 ether}(
            _baseIssuanceParams(recipients, quantities), options, fee, intexFactory
        );
    }

    function test_sendRefundInstructions_revert_empty_array() public {
        address[] memory bidders = new address[](0);
        uint64[] memory refundedAmounts = new uint64[](0);
        uint64[] memory paidAmounts = new uint64[](0);

        MessagingFee memory fee = MessagingFee({nativeFee: 0.1 ether, lzTokenFee: 0});
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        vm.prank(desis);
        vm.expectRevert(IOriginMessenger.EmptyArray.selector);
        outbeAdapter.sendRefundInstructions{value: 0.1 ether}(
            SERIES_ID, bidders, refundedAmounts, paidAmounts, options, fee, desis
        );
    }

    function test_sendRefundInstructions_revert_array_length_mismatch() public {
        address[] memory bidders = new address[](2);
        uint64[] memory refundedAmounts = new uint64[](2);
        uint64[] memory paidAmounts = new uint64[](1); // Mismatch

        bidders[0] = address(0x1);
        bidders[1] = address(0x2);
        refundedAmounts[0] = 100e6;
        refundedAmounts[1] = 200e6;
        paidAmounts[0] = 50e6;

        MessagingFee memory fee = MessagingFee({nativeFee: 0.1 ether, lzTokenFee: 0});
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        vm.prank(desis);
        vm.expectRevert(IOriginMessenger.ArrayLengthMismatch.selector);
        outbeAdapter.sendRefundInstructions{value: 0.1 ether}(
            SERIES_ID, bidders, refundedAmounts, paidAmounts, options, fee, desis
        );
    }

    // --- Role Constants Tests ---
    function test_role_constants() public view {
        assertEq(outbeAdapter.DESIS_ROLE(), keccak256("DESIS_ROLE"));
    }

    // --- Quote Tests ---
    function test_quoteSendAuctionStageStart() public view {
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        MessagingFee memory fee = outbeAdapter.quoteSendAuctionStageStart(_baseStageStartParams(), options, false);

        assertTrue(fee.nativeFee > 0);
    }

    function test_quoteSendAuctionStageReveal() public view {
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        MessagingFee memory fee = outbeAdapter.quoteSendAuctionStageReveal(SERIES_ID, true, options, false);

        assertTrue(fee.nativeFee > 0);
    }

    function test_quoteSendAuctionStageClearing() public view {
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        MessagingFee memory fee = outbeAdapter.quoteSendAuctionStageClearing(SERIES_ID, options, false);

        assertTrue(fee.nativeFee > 0);
    }

    function test_quoteSendAuctionResult() public view {
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        // (seriesId, issuedIntexCount, auctionClearingRate, wonBidsCount, ...)
        MessagingFee memory fee = outbeAdapter.quoteSendAuctionResult(SERIES_ID, 500, 75e6, 42, options, false);

        assertTrue(fee.nativeFee > 0);
    }

    function test_quoteSendIssuanceInstructions() public view {
        address[] memory recipients = new address[](2);
        uint256[] memory quantities = new uint256[](2);

        recipients[0] = address(0x1);
        recipients[1] = address(0x2);
        quantities[0] = 10;
        quantities[1] = 20;

        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        MessagingFee memory fee =
            outbeAdapter.quoteSendIssuanceInstructions(_baseIssuanceParams(recipients, quantities), options, false);

        assertTrue(fee.nativeFee > 0);
    }

    function test_quoteSendRefundInstructions() public view {
        address[] memory bidders = new address[](2);
        uint64[] memory refundedAmounts = new uint64[](2);
        uint64[] memory paidAmounts = new uint64[](2);

        bidders[0] = address(0x1);
        bidders[1] = address(0x2);
        refundedAmounts[0] = 100e6;
        refundedAmounts[1] = 200e6;
        paidAmounts[0] = 50e6;
        paidAmounts[1] = 75e6;

        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        MessagingFee memory fee =
            outbeAdapter.quoteSendRefundInstructions(SERIES_ID, bidders, refundedAmounts, paidAmounts, options, false);

        assertTrue(fee.nativeFee > 0);
    }

    function test_quoteSendMarkCalled() public view {
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);

        MessagingFee memory fee = outbeAdapter.quoteSendMarkCalled(SERIES_ID, options, false);

        assertTrue(fee.nativeFee > 0);
    }

    // --- ERC165 Tests ---
    function test_supportsInterface() public view {
        // IAccessControl interface ID
        bytes4 accessControlId = 0x7965db0b;
        assertTrue(outbeAdapter.supportsInterface(accessControlId));
    }
}
