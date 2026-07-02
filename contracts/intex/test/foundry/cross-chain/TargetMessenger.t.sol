// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {TargetMessenger} from "@contracts/target/TargetMessenger.sol";
import {OriginMessenger} from "@contracts/origin/OriginMessenger.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {ITargetMessenger} from "@contracts/target/interfaces/ITargetMessenger.sol";
import {IONFT1155AdapterBatch} from "@contracts/shared/interfaces/IONFT1155AdapterBatch.sol";
import {RejectingReceiver} from "@test-mocks/RejectingReceiver.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";

import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";

/**
 * @title TargetMessengerTest
 * @notice Foundry tests for TargetMessenger
 * @dev Tests message encoding/decoding and cross-chain communication.
 *      All auction/series messages are keyed by `seriesId` (uint32).
 */
contract TargetMessengerTest is CrossChainTest {
    uint32 private constant BNB_CHAIN_ID = 1;
    uint32 private constant OUTBE_CHAIN_ID = 2;

    TargetMessenger private bnbAdapter;
    OriginMessenger private outbeAdapter;
    ONFT1155AdapterBatch private batchAdapter;

    // Mock contracts
    IntexAuction private auction;
    IntexNFT1155 private intex;

    address private admin = address(this);
    address private user = address(0x1);

    // Stand-in Desis recipient that advertises `IDesis` via ERC-165 — declared in setUp().
    address private desis;

    uint32 private constant SERIES_ID = 20250115; // yyyymmdd format

    function setUp() public {
        _setUpBridge();
        // The quote test asserts a non-zero fee; give the loopback bridge a fixed fee to return.
        bridge.setFee(0.001 ether);

        vm.deal(admin, 1000 ether);
        vm.deal(user, 1000 ether);

        // Stand-in Desis recipient.
        desis = address(new MockDesis());
        vm.deal(desis, 1000 ether);

        // Deploy mock contracts
        auction = DeployProxy.intexAuction(admin, admin);
        intex = DeployProxy.intexNFT1155(admin, admin);

        // Deploy BNB adapter
        bnbAdapter = DeployProxy.targetMessenger(address(bridge), admin, OUTBE_CHAIN_ID);

        // Deploy Outbe adapter (for cross-chain testing)
        outbeAdapter = DeployProxy.originMessenger(address(bridge), admin, BNB_CHAIN_ID);

        // Deploy batch adapter on BNB
        batchAdapter = DeployProxy.onftAdapterBatch(address(intex), address(bridge), admin);

        // Wire adapters (register remote messengers)
        bnbAdapter.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(outbeAdapter)));
        outbeAdapter.setRemoteMessenger(BNB_CHAIN_ID, _interop(BNB_CHAIN_ID, address(bnbAdapter)));

        // Wire BNB adapter dependencies
        bnbAdapter.wire(address(auction), address(intex), admin, address(batchAdapter));

        // Wire Outbe adapter dependencies
        outbeAdapter.wire(desis, makeAddr("factory"));

        // Grant RELAYER_ROLE to adapter
        auction.grantRole(auction.RELAYER_ROLE(), address(bnbAdapter));
        intex.grantRole(intex.RELAYER_ROLE(), address(bnbAdapter));

        // Grant SYSTEM_RELAYER_ROLE to TargetMessenger on batch adapter
        batchAdapter.grantRole(batchAdapter.SYSTEM_RELAYER_ROLE(), address(bnbAdapter));

        // Grant RELAYER_ROLE to batch adapter on intex (for crosschainBurn)
        intex.grantRole(intex.RELAYER_ROLE(), address(batchAdapter));
    }

    // --- Helpers ---
    /// @dev Build a single-bid BidsBatchParams payload keyed by SERIES_ID.
    function _bidsBatchParams(uint256 count) internal view returns (ITargetMessenger.BidsBatchParams memory) {
        address[] memory bidderAddresses = new address[](count);
        uint16[] memory intexQuantities = new uint16[](count);
        uint32[] memory intexBidRates = new uint32[](count);
        uint32[] memory timestamps = new uint32[](count);

        for (uint256 i = 0; i < count; i++) {
            bidderAddresses[i] = address(uint160(0x1000 + i));
            intexQuantities[i] = uint16(10 + i);
            intexBidRates[i] = uint32(100e6 + i);
            timestamps[i] = uint32(block.timestamp);
        }

        return ITargetMessenger.BidsBatchParams({
            seriesId: SERIES_ID,
            bidderAddresses: bidderAddresses,
            intexQuantities: intexQuantities,
            intexBidRates: intexBidRates,
            timestamps: timestamps
        });
    }

    // --- Constructor Tests ---
    function test_constructor() public view {
        assertTrue(bnbAdapter.hasRole(bnbAdapter.DEFAULT_ADMIN_ROLE(), admin));
    }

    function test_wire() public view {
        assertEq(address(bnbAdapter.auction()), address(auction));
        assertEq(address(bnbAdapter.intex()), address(intex));
        assertTrue(bnbAdapter.hasRole(bnbAdapter.AUCTION_ROLE(), address(auction)));
    }

    function test_wire_revert_zero_address() public {
        TargetMessenger newAdapter = DeployProxy.targetMessenger(address(bridge), admin, OUTBE_CHAIN_ID);

        vm.expectRevert(abi.encodeWithSelector(ITargetMessenger.ZeroAddress.selector, "auction"));
        newAdapter.wire(address(0), address(intex), admin, address(batchAdapter));
    }

    function test_wire_revert_zero_intex() public {
        TargetMessenger newAdapter = DeployProxy.targetMessenger(address(bridge), admin, OUTBE_CHAIN_ID);

        vm.expectRevert(abi.encodeWithSelector(ITargetMessenger.ZeroAddress.selector, "intex"));
        newAdapter.wire(address(auction), address(0), admin, address(batchAdapter));
    }

    function test_wire_revert_zero_escrowAdapter() public {
        TargetMessenger newAdapter = DeployProxy.targetMessenger(address(bridge), admin, OUTBE_CHAIN_ID);

        vm.expectRevert(abi.encodeWithSelector(ITargetMessenger.ZeroAddress.selector, "escrowAdapter"));
        newAdapter.wire(address(auction), address(intex), address(0), address(batchAdapter));
    }

    function test_wire_revert_zero_onftBatchAdapter() public {
        TargetMessenger newAdapter = DeployProxy.targetMessenger(address(bridge), admin, OUTBE_CHAIN_ID);

        vm.expectRevert(abi.encodeWithSelector(ITargetMessenger.ZeroAddress.selector, "onftBatchAdapter"));
        newAdapter.wire(address(auction), address(intex), admin, address(0));
    }

    // --- Access Control Tests ---
    function test_sendBidsBatch_revert_unauthorized() public {
        ITargetMessenger.BidsBatchParams memory params = _bidsBatchParams(1);

        vm.prank(user);
        vm.expectRevert();
        bnbAdapter.sendBidsBatch{value: 0.1 ether}(params);
    }

    // --- Role Constants Tests ---
    function test_role_constants() public view {
        assertEq(bnbAdapter.AUCTION_ROLE(), keccak256("AUCTION_ROLE"));
    }

    // --- Quote Tests ---
    function test_quoteSendBidsBatch() public view {
        ITargetMessenger.BidsBatchParams memory params = _bidsBatchParams(2);

        uint256 fee = bnbAdapter.quoteSendBidsBatch(params);

        // Fee should be non-zero
        assertEq(fee, 0.001 ether);
    }

    // --- ERC165 Tests ---
    function test_supportsInterface() public view {
        // IAccessControl interface ID
        bytes4 accessControlId = 0x7965db0b;
        assertTrue(bnbAdapter.supportsInterface(accessControlId));
    }

    // --- sweepNative Tests (TargetMessenger) ---
    function test_sweepNative_bnb_success() public {
        vm.deal(address(bnbAdapter), 5 ether);
        address payable recipient = payable(address(0xBEEF));
        uint256 before = recipient.balance;

        bnbAdapter.sweepNative(recipient, 5 ether);

        assertEq(recipient.balance - before, 5 ether);
        assertEq(address(bnbAdapter).balance, 0);
    }

    function test_sweepNative_bnb_revert_zeroTo() public {
        vm.deal(address(bnbAdapter), 1 ether);
        vm.expectRevert(abi.encodeWithSelector(ITargetMessenger.ZeroAddress.selector, "to"));
        bnbAdapter.sweepNative(payable(address(0)), 1 ether);
    }

    function test_sweepNative_bnb_revert_insufficientBalance() public {
        vm.deal(address(bnbAdapter), 1 ether);
        vm.expectRevert(abi.encodeWithSelector(ITargetMessenger.NativeBalanceInsufficient.selector, 1 ether, 2 ether));
        bnbAdapter.sweepNative(payable(address(0xBEEF)), 2 ether);
    }

    function test_sweepNative_bnb_revert_failedCall() public {
        vm.deal(address(bnbAdapter), 1 ether);
        RejectingReceiver rejector = new RejectingReceiver();
        vm.expectRevert(ITargetMessenger.NativeSweepFailed.selector);
        bnbAdapter.sweepNative(payable(address(rejector)), 1 ether);
    }

    function test_sweepNative_bnb_revert_unauthorized() public {
        vm.deal(address(bnbAdapter), 1 ether);
        vm.prank(user);
        vm.expectRevert();
        bnbAdapter.sweepNative(payable(address(0xBEEF)), 1 ether);
    }

    // --- sweepNative Tests (ONFT1155AdapterBatch) ---
    function test_sweepNative_batch_success() public {
        vm.deal(address(batchAdapter), 3 ether);
        address payable recipient = payable(address(0xCAFE));
        uint256 before = recipient.balance;

        batchAdapter.sweepNative(recipient, 3 ether);

        assertEq(recipient.balance - before, 3 ether);
        assertEq(address(batchAdapter).balance, 0);
    }

    function test_sweepNative_batch_revert_zeroTo() public {
        vm.deal(address(batchAdapter), 1 ether);
        vm.expectRevert(abi.encodeWithSelector(IONFT1155AdapterBatch.ZeroAddress.selector, "to"));
        batchAdapter.sweepNative(payable(address(0)), 1 ether);
    }

    function test_sweepNative_batch_revert_insufficientBalance() public {
        vm.deal(address(batchAdapter), 1 ether);
        vm.expectRevert(
            abi.encodeWithSelector(IONFT1155AdapterBatch.NativeBalanceInsufficient.selector, 1 ether, 2 ether)
        );
        batchAdapter.sweepNative(payable(address(0xCAFE)), 2 ether);
    }

    function test_sweepNative_batch_revert_failedCall() public {
        vm.deal(address(batchAdapter), 1 ether);
        RejectingReceiver rejector = new RejectingReceiver();
        vm.expectRevert(IONFT1155AdapterBatch.NativeSweepFailed.selector);
        batchAdapter.sweepNative(payable(address(rejector)), 1 ether);
    }

    function test_sweepNative_batch_revert_unauthorized() public {
        vm.deal(address(batchAdapter), 1 ether);
        vm.prank(user);
        vm.expectRevert();
        batchAdapter.sweepNative(payable(address(0xCAFE)), 1 ether);
    }
}
