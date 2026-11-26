// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {Vm} from "forge-std/Vm.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {OpenSeriesLib} from "../helpers/OpenSeriesLib.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {IIntexNFT1155Router, SendParam} from "@contracts/shared/interfaces/IIntexNFT1155Router.sol";
import {ERC7786Sender} from "@contracts/shared/ERC7786Sender.sol";
import {RevertingERC1155Receiver} from "@test-mocks/RevertingERC1155Receiver.sol";

/// @title IntexNFT1155RouterSingleTest
/// @notice Foundry tests for IntexNFT1155Router with IntexNFT1155 token.
/// @dev Tests cross-chain transfers over the {MockERC7786Bridge} loopback via {CrossChainTest}.
///      Series are keyed by `seriesId` (uint32). The issued token id is `uint256(seriesId)`.
///      Delivery is manual: a send records the packet on the bridge and {_deliver} hands it to the
///      destination adapter as the bridge.
contract IntexNFT1155RouterSingleTest is CrossChainTest {
    uint32 private constant A_CHAIN_ID = 1;
    uint32 private constant B_CHAIN_ID = 2;

    uint256 private constant FEE = 0.001 ether;

    IntexNFT1155 private tokenA;
    IntexNFT1155 private tokenB;
    IntexNFT1155Router private adapterA;
    IntexNFT1155Router private adapterB;

    address private admin = address(this);
    address private user = address(0x1);
    uint32 private constant SERIES_ID_DAY = 20260401;
    bytes14 private constant SERIES_ID = "20260401-USD-U";
    uint256 private constant INTEX_ID = uint256(keccak256(abi.encodePacked(SERIES_ID)));
    uint256 private constant AMOUNT = 100;
    uint32 private constant ISSUED_UNITS = 10_000;

    function setUp() public {
        vm.deal(user, 1000 ether);

        _setUpBridge();
        bridge.setFee(FEE);

        // Deploy IntexNFT1155 tokens on both chains
        tokenA = DeployProxy.intexNFT1155(admin, admin);
        tokenB = DeployProxy.intexNFT1155(admin, admin);

        adapterA = DeployProxy.intexNFT1155Router(address(tokenA), address(bridge), admin);
        adapterB = DeployProxy.intexNFT1155Router(address(tokenB), address(bridge), admin);

        // Grant BRIDGE_ROLE to adapters
        tokenA.grantRole(tokenA.BRIDGE_ROLE(), address(adapterA));
        tokenB.grantRole(tokenB.BRIDGE_ROLE(), address(adapterB));

        // Wire adapters (register each as the other's remote router)
        adapterA.setRemoteRouter(B_CHAIN_ID, _interop(B_CHAIN_ID, address(adapterB)));
        adapterB.setRemoteRouter(A_CHAIN_ID, _interop(A_CHAIN_ID, address(adapterA)));

        // Create series on both chains
        tokenA.openSeries(OpenSeriesLib.params(SERIES_ID_DAY, ISSUED_UNITS, 0));
        tokenB.openSeries(OpenSeriesLib.params(SERIES_ID_DAY, ISSUED_UNITS, 0));

        // Mint initial tokens to user on chain A
        tokenA.issueIntex(user, AMOUNT, SERIES_ID);
    }

    /// @dev Deliver the packet the source adapter just handed the bridge to `dst` as if from `src` on A.
    function _deliverAToB() internal {
        _deliver(A_CHAIN_ID, address(adapterA), address(adapterB), bridge.lastPayload());
    }

    function _deliverBToA() internal {
        _deliver(B_CHAIN_ID, address(adapterB), address(adapterA), bridge.lastPayload());
    }

    function test_constructor() public view {
        assertTrue(adapterA.hasRole(adapterA.DEFAULT_ADMIN_ROLE(), admin));
        assertEq(address(adapterA.token()), address(tokenA));
        assertEq(address(adapterA.BRIDGE()), address(bridge));
    }

    /// @notice `token` is immutable, so a zero address would permanently brick the
    ///         adapter (every `crosschainBurn`/`crosschainMint` reverts on a non-contract). Reject at construction.
    function test_constructor_revertsZeroToken() public {
        // Property of the implementation constructor - the token immutable is set there.
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155Router.ZeroAddress.selector, "token"));
        new IntexNFT1155Router(address(0), address(bridge));
    }

    /// @notice A zero bridge address is caught by the `ERC7786Sender` constructor guard.
    function test_constructor_revertsZeroBridge() public {
        vm.expectRevert(ERC7786Sender.InvalidBridge.selector);
        new IntexNFT1155Router(address(tokenA), address(0));
    }

    function test_send() public {
        SendParam memory sendParam =
            SendParam({dstChainId: B_CHAIN_ID, to: bytes32(uint256(uint160(user))), intexId: INTEX_ID, units: AMOUNT});

        uint256 fee = adapterA.quoteSend(sendParam);

        // Check initial balances
        assertEq(tokenA.balanceOf(user, INTEX_ID), AMOUNT);
        assertEq(tokenB.balanceOf(user, INTEX_ID), 0);

        // Send tokens
        vm.prank(user);
        adapterA.send{value: fee}(sendParam);

        // Process cross-chain message
        _deliverAToB();

        // Check final balances
        assertEq(tokenA.balanceOf(user, INTEX_ID), 0);
        assertEq(tokenB.balanceOf(user, INTEX_ID), AMOUNT);
    }

    function test_a_single_send_has_its_own_message_and_arrives_as_one_receive() public {
        SendParam memory sendParam =
            SendParam({dstChainId: B_CHAIN_ID, to: bytes32(uint256(uint160(user))), intexId: INTEX_ID, units: 40});
        uint256 fee = adapterA.quoteSend(sendParam);
        vm.prank(user);
        adapterA.send{value: fee}(sendParam);

        bytes memory packet = bridge.lastPayload();
        assertEq(uint8(packet[1]), adapterA.SEND_ONE(), "its own message type");
        bytes32 receiveId = keccak256(abi.encode(_interop(A_CHAIN_ID, address(adapterA)), packet));

        vm.recordLogs();
        vm.expectEmit(true, true, true, true, address(adapterB));
        emit IIntexNFT1155Router.IntexCrosschainReceived(receiveId, A_CHAIN_ID, user, INTEX_ID, 40);
        _deliverAToB();
        assertEq(_countTopic(IIntexNFT1155Router.IntexCrosschainBatchReceived.selector), 0, "no batch event");
        assertEq(tokenB.balanceOf(user, INTEX_ID), 40);
    }

    function test_a_failed_single_receive_is_reported_received_on_its_retry() public {
        uint32 failDay = 20260402;
        bytes14 failSeries = "20260402-USD-U";
        uint256 failIntexId = uint256(keccak256(abi.encodePacked(failSeries)));
        tokenA.openSeries(OpenSeriesLib.params(failDay, ISSUED_UNITS, 0));
        tokenA.issueIntex(user, AMOUNT, failSeries);
        SendParam memory sendParam = SendParam({
            dstChainId: B_CHAIN_ID, to: bytes32(uint256(uint160(user))), intexId: failIntexId, units: AMOUNT
        });
        uint256 fee = adapterA.quoteSend(sendParam);
        vm.prank(user);
        adapterA.send{value: fee}(sendParam);
        bytes32 receiveId = keccak256(abi.encode(_interop(A_CHAIN_ID, address(adapterA)), bridge.lastPayload()));

        vm.recordLogs();
        _deliverAToB();
        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 failed;
        for (uint256 i; i < logs.length; ++i) {
            assertTrue(logs[i].topics[0] != IIntexNFT1155Router.IntexCrosschainReceived.selector, "not received yet");
            if (logs[i].topics[0] == IIntexNFT1155Router.IntexCrosschainReceiveFailed.selector) failed++;
        }
        assertEq(failed, 1);

        tokenB.openSeries(OpenSeriesLib.params(failDay, ISSUED_UNITS, 0));
        vm.expectEmit(true, true, true, true, address(adapterB));
        emit IIntexNFT1155Router.IntexCrosschainReceived(receiveId, A_CHAIN_ID, user, failIntexId, uint32(AMOUNT));
        adapterB.retryIntexCrosschainReceive(receiveId, 0);
    }

    function test_send_without_the_fee_does_not_use_the_float() public {
        SendParam memory sendParam =
            SendParam({dstChainId: B_CHAIN_ID, to: bytes32(uint256(uint160(user))), intexId: INTEX_ID, units: AMOUNT});
        vm.deal(address(adapterA), 1 ether);

        vm.prank(user);
        vm.expectRevert(abi.encodeWithSelector(ERC7786Sender.MsgValueBelowFee.selector, 0, FEE));
        adapterA.send(sendParam);

        assertEq(address(adapterA).balance, 1 ether, "the float is untouched");
        assertEq(tokenA.balanceOf(user, INTEX_ID), AMOUNT, "nothing burned");
    }

    function test_send_partial_amount() public {
        uint256 sendAmount = AMOUNT / 2;

        SendParam memory sendParam = SendParam({
            dstChainId: B_CHAIN_ID, to: bytes32(uint256(uint160(user))), intexId: INTEX_ID, units: sendAmount
        });

        uint256 fee = adapterA.quoteSend(sendParam);

        vm.prank(user);
        adapterA.send{value: fee}(sendParam);
        _deliverAToB();

        assertEq(tokenA.balanceOf(user, INTEX_ID), AMOUNT - sendAmount);
        assertEq(tokenB.balanceOf(user, INTEX_ID), sendAmount);
    }

    function test_send_to_different_address() public {
        address recipient = address(0x2);

        SendParam memory sendParam = SendParam({
            dstChainId: B_CHAIN_ID, to: bytes32(uint256(uint160(recipient))), intexId: INTEX_ID, units: AMOUNT
        });

        uint256 fee = adapterA.quoteSend(sendParam);

        vm.prank(user);
        adapterA.send{value: fee}(sendParam);
        _deliverAToB();

        assertEq(tokenA.balanceOf(user, INTEX_ID), 0);
        assertEq(tokenB.balanceOf(recipient, INTEX_ID), AMOUNT);
    }

    function test_roundtrip() public {
        // Send A -> B
        SendParam memory sendParamAB =
            SendParam({dstChainId: B_CHAIN_ID, to: bytes32(uint256(uint160(user))), intexId: INTEX_ID, units: AMOUNT});

        uint256 feeAB = adapterA.quoteSend(sendParamAB);
        vm.prank(user);
        adapterA.send{value: feeAB}(sendParamAB);
        _deliverAToB();

        assertEq(tokenB.balanceOf(user, INTEX_ID), AMOUNT);

        // Send B -> A
        SendParam memory sendParamBToA =
            SendParam({dstChainId: A_CHAIN_ID, to: bytes32(uint256(uint160(user))), intexId: INTEX_ID, units: AMOUNT});

        uint256 feeBToA = adapterB.quoteSend(sendParamBToA);
        vm.prank(user);
        adapterB.send{value: feeBToA}(sendParamBToA);
        _deliverBToA();

        assertEq(tokenA.balanceOf(user, INTEX_ID), AMOUNT);
        assertEq(tokenB.balanceOf(user, INTEX_ID), 0);
    }

    function test_revert_invalid_receiver() public {
        SendParam memory sendParam =
            SendParam({dstChainId: B_CHAIN_ID, to: bytes32(0), intexId: INTEX_ID, units: AMOUNT});

        vm.expectRevert(IIntexNFT1155Router.InvalidReceiver.selector);
        adapterA.quoteSend(sendParam);
    }

    function test_revert_insufficient_balance() public {
        SendParam memory sendParam = SendParam({
            dstChainId: B_CHAIN_ID,
            to: bytes32(uint256(uint160(user))),
            intexId: INTEX_ID,
            units: AMOUNT + 1 // More than balance
        });

        uint256 fee = adapterA.quoteSend(sendParam);

        vm.prank(user);
        vm.expectRevert();
        adapterA.send{value: fee}(sendParam);
    }

    function test_intex_state_preserved_after_bridge() public {
        IIntexNFT1155.SeriesData memory dataA = tokenA.getIntexSeries(SERIES_ID);
        assertEq(uint8(dataA.state), uint8(IIntexNFT1155.IntexState.Issued));

        // Send tokens A -> B
        SendParam memory sendParam =
            SendParam({dstChainId: B_CHAIN_ID, to: bytes32(uint256(uint160(user))), intexId: INTEX_ID, units: AMOUNT});

        uint256 fee = adapterA.quoteSend(sendParam);
        vm.prank(user);
        adapterA.send{value: fee}(sendParam);
        _deliverAToB();

        // Verify state is identical on chain B
        IIntexNFT1155.SeriesData memory dataB = tokenB.getIntexSeries(SERIES_ID);
        assertEq(uint8(dataB.state), uint8(IIntexNFT1155.IntexState.Issued));
    }

    // --- Inbound crosschainMint isolation ---

    function test_inboundCrosschainMintFailure_parksAndRetries() public {
        // A series that exists on A only: the destination crosschainMint reverts IntexNotFound, which
        // must park the transfer (not unwind the packet and strand the burned tokens).
        uint32 failDay = 20260402;
        bytes14 failSeries = "20260402-USD-U";
        uint256 failIntexId = uint256(keccak256(abi.encodePacked(failSeries)));
        tokenA.openSeries(OpenSeriesLib.params(failDay, ISSUED_UNITS, 0));
        tokenA.issueIntex(user, AMOUNT, failSeries);

        SendParam memory sendParam = SendParam({
            dstChainId: B_CHAIN_ID, to: bytes32(uint256(uint160(user))), intexId: failIntexId, units: AMOUNT
        });
        uint256 fee = adapterA.quoteSend(sendParam);
        vm.prank(user);
        adapterA.send{value: fee}(sendParam);

        // The bridge derives the receiveId from (sender interop, payload). Recompute it to key the
        // parked entry.
        bytes memory packet = bridge.lastPayload();
        bytes32 receiveId = keccak256(abi.encode(_interop(A_CHAIN_ID, address(adapterA)), packet));

        // Delivery must succeed (crosschainMint parked), not revert.
        _deliverAToB();

        // Source burned, destination not minted, transfer parked under the bridge receiveId.
        assertEq(tokenA.balanceOf(user, failIntexId), 0, "source burned");
        assertEq(tokenB.balanceOf(user, failIntexId), 0, "not minted yet");
        (address to,, uint256 amount, bool exists) = adapterB.getFailedCrosschainReceive(receiveId, 0);
        assertTrue(exists, "crosschainMint parked");
        assertEq(to, user);
        assertEq(amount, AMOUNT);

        // Fix the cause on B, then retry -> minted and entry cleared.
        tokenB.openSeries(OpenSeriesLib.params(failDay, ISSUED_UNITS, 0));
        adapterB.retryIntexCrosschainReceive(receiveId, 0);
        assertEq(tokenB.balanceOf(user, failIntexId), AMOUNT, "minted on retry");

        (,,, bool existsAfter) = adapterB.getFailedCrosschainReceive(receiveId, 0);
        assertFalse(existsAfter, "entry cleared");

        // A re-retry reverts.
        vm.expectRevert(
            abi.encodeWithSelector(IIntexNFT1155Router.NoSuchFailedCrosschainReceive.selector, receiveId, 0)
        );
        adapterB.retryIntexCrosschainReceive(receiveId, 0);
    }

    function test_inboundCrosschainMintFailure_sendBackToSourceRestoresOrigin() public {
        // Park an inbound on B for a series that exists on A only.
        uint32 failDay = 20260402;
        bytes14 failSeries = "20260402-USD-U";
        uint256 failIntexId = uint256(keccak256(abi.encodePacked(failSeries)));
        tokenA.openSeries(OpenSeriesLib.params(failDay, ISSUED_UNITS, 0));
        tokenA.issueIntex(user, AMOUNT, failSeries);

        SendParam memory sendParam = SendParam({
            dstChainId: B_CHAIN_ID, to: bytes32(uint256(uint160(user))), intexId: failIntexId, units: AMOUNT
        });
        uint256 fee = adapterA.quoteSend(sendParam);
        vm.prank(user);
        adapterA.send{value: fee}(sendParam);

        bytes memory packet = bridge.lastPayload();
        bytes32 receiveId = keccak256(abi.encode(_interop(A_CHAIN_ID, address(adapterA)), packet));
        _deliverAToB();

        assertEq(tokenA.balanceOf(user, failIntexId), 0, "source burned");
        (,,, bool parked) = adapterB.getFailedCrosschainReceive(receiveId, 0);
        assertTrue(parked, "parked on B");

        // The reclaimer pays the fee: the quote names it, and B's native float is never used.
        assertEq(adapterB.quoteSendBackToSource(receiveId, 0), FEE);
        vm.deal(address(adapterB), 1 ether);
        vm.prank(user);
        vm.expectRevert(abi.encodeWithSelector(ERC7786Sender.MsgValueBelowFee.selector, 0, FEE));
        adapterB.sendBackToSource(receiveId, 0);

        // Reclaim: B sends the transfer back to its origin A - the only exit that skips B's gate.
        vm.prank(user);
        adapterB.sendBackToSource{value: FEE}(receiveId, 0);

        (,,, bool stillParked) = adapterB.getFailedCrosschainReceive(receiveId, 0);
        assertFalse(stillParked, "entry consumed");

        // Deliver the reverse packet on A -> owner re-minted, cross-chain supply conserved.
        _deliverBToA();
        assertEq(tokenA.balanceOf(user, failIntexId), AMOUNT, "owner re-minted on origin");
        assertEq(tokenB.balanceOf(user, failIntexId), 0, "nothing on destination");

        // A second reclaim reverts - the entry is gone.
        vm.expectRevert(
            abi.encodeWithSelector(IIntexNFT1155Router.NoSuchFailedCrosschainReceive.selector, receiveId, 0)
        );
        adapterB.sendBackToSource(receiveId, 0);
    }

    function test_sendBackToSource_returnsTheUnitsToTheOwnerNotTheRecipient() public {
        RevertingERC1155Receiver recipient = new RevertingERC1155Receiver();
        SendParam memory sendParam = SendParam({
            dstChainId: B_CHAIN_ID, to: bytes32(uint256(uint160(address(recipient)))), intexId: INTEX_ID, units: AMOUNT
        });
        uint256 fee = adapterA.quoteSend(sendParam);
        vm.prank(user);
        adapterA.send{value: fee}(sendParam);
        bytes32 receiveId = keccak256(abi.encode(_interop(A_CHAIN_ID, address(adapterA)), bridge.lastPayload()));
        _deliverAToB();

        (,,, bool parked) = adapterB.getFailedCrosschainReceive(receiveId, 0);
        assertTrue(parked, "the recipient rejects the mint");

        adapterB.sendBackToSource{value: FEE}(receiveId, 0);
        _deliverBToA();

        assertEq(tokenA.balanceOf(user, INTEX_ID), AMOUNT, "the owner gets the units back");
        assertEq(tokenA.balanceOf(address(recipient), INTEX_ID), 0, "the recipient gets nothing on the source");
    }

    function test_retryIntexCrosschainReceive_revertsForUnknownReceiveId() public {
        bytes32 unknown = bytes32(uint256(0xABCD));
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155Router.NoSuchFailedCrosschainReceive.selector, unknown, 0));
        adapterB.retryIntexCrosschainReceive(unknown, 0);
    }

    function test_crosschainReceiveOne_externalCallerRevertsNotSelf() public {
        vm.expectRevert(IIntexNFT1155Router.NotSelf.selector);
        adapterB.crosschainReceiveOne(address(0xCAFE), INTEX_ID, 1);
    }
}
