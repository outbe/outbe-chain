// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {IIntexNFT1155Router} from "@contracts/shared/interfaces/IIntexNFT1155Router.sol";
import {IntexNFT1155RouterCodec} from "@contracts/shared/libs/IntexNFT1155RouterCodec.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {OpenSeriesLib} from "../helpers/OpenSeriesLib.sol";

/// @title InboundFailureIsolationTest
/// @notice Behavioural coverage Pattern B on `IntexNFT1155Router`: a per-item
///         `token.crosschainMint` revert no longer reverts the whole batch. Instead:
///         - the bridge records the failure as a `FailedCrosschainReceive` snapshot,
///         - the bridge emits `IntexCrosschainReceiveFailed`,
///         - `retryIntexCrosschainReceive` re-attempts the crosschainMint after the upstream issue is
///           fixed.
///         This is the Critical funds-lock fix from the contract review (R-04).
contract InboundFailureIsolationTest is CrossChainTest {
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant OUTBE_CHAIN_ID = 2;

    IntexNFT1155Router internal nftRouterBnb;
    IntexNFT1155Router internal nftRouterOutbe;
    IntexNFT1155 internal intex;

    address internal admin = address(this);

    uint32 internal constant SERIES_GOOD_DAY = 20260201;
    bytes14 internal constant SERIES_GOOD = "20260201-USD-U";
    uint32 internal constant SERIES_BAD_DAY = 20260202;
    bytes14 internal constant SERIES_BAD = "20260202-USD-U";
    uint256 internal constant TOKEN_GOOD = uint256(keccak256(abi.encodePacked(SERIES_GOOD)));
    uint256 internal constant TOKEN_BAD = uint256(keccak256(abi.encodePacked(SERIES_BAD)));

    function setUp() public {
        _setUpBridge();

        intex = DeployProxy.intexNFT1155(admin, admin);
        nftRouterBnb = DeployProxy.intexNFT1155Router(address(intex), address(bridge), admin);
        nftRouterOutbe = DeployProxy.intexNFT1155Router(address(intex), address(bridge), admin);

        nftRouterBnb.setRemoteRouter(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(nftRouterOutbe)));
        nftRouterOutbe.setRemoteRouter(BNB_CHAIN_ID, _interop(BNB_CHAIN_ID, address(nftRouterBnb)));

        // Two series: one Issued (crosschainMint succeeds), one not (crosschainMint reverts on state check).
        intex.openSeries(OpenSeriesLib.params(SERIES_GOOD_DAY, 10_000, 0));
        // SERIES_BAD intentionally not created - `intex.crosschainMint` will revert on lookup.

        intex.grantRole(intex.BRIDGE_ROLE(), address(nftRouterBnb));
    }

    /// @dev Deliver `message` from the OUTBE peer to the BNB adapter through the bridge.
    function _deliverInbound(bytes memory message) internal {
        _deliver(OUTBE_CHAIN_ID, address(nftRouterOutbe), address(nftRouterBnb), message);
    }

    /// @dev Recompute the bridge's `receiveId` for a message delivered from the OUTBE peer, matching
    ///      `MockERC7786Bridge._deliver`: `keccak256(abi.encode(sender, payload))`.
    function _receiveId(bytes memory message) internal view returns (bytes32) {
        bytes memory sender = _interop(OUTBE_CHAIN_ID, address(nftRouterOutbe));
        return keccak256(abi.encode(sender, message));
    }

    /// @dev SEND_BATCH (V2 abi.encode): 2 items. Item 0 targets SERIES_GOOD, item 1 SERIES_BAD.
    function _batchPacket(address recipient) internal pure returns (bytes memory) {
        uint256[] memory tokenIds = new uint256[](2);
        tokenIds[0] = TOKEN_GOOD;
        tokenIds[1] = TOKEN_BAD;
        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 50;
        amounts[1] = 75;
        return IntexNFT1155RouterCodec.encodeBatch(
            IntexNFT1155RouterCodec.BatchPayload({
                to: bytes32(uint256(uint160(recipient))), intexIds: tokenIds, units: amounts
            })
        );
    }

    /// @dev SEND_MULTI batch (V2 abi.encode): 2 items.
    function _multiPacket(address goodRecipient, address badRecipient) internal pure returns (bytes memory) {
        bytes32[] memory recipients = new bytes32[](2);
        recipients[0] = bytes32(uint256(uint160(goodRecipient)));
        recipients[1] = bytes32(uint256(uint160(badRecipient)));
        uint256[] memory tokenIds = new uint256[](2);
        tokenIds[0] = TOKEN_GOOD;
        tokenIds[1] = TOKEN_BAD;
        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 50;
        amounts[1] = 75;
        return IntexNFT1155RouterCodec.encodeMulti(
            IntexNFT1155RouterCodec.MultiPayload({recipients: recipients, intexIds: tokenIds, units: amounts})
        );
    }

    // ---------------------------------------------------------------
    // SEND_BATCH - per-item isolation
    // ---------------------------------------------------------------

    function test_BatchReceive_BadItemDoesNotRevertWholeBatch() public {
        address recipient = address(0xCAFE);
        bytes memory packet = _batchPacket(recipient);
        bytes32 receiveId = _receiveId(packet);

        _deliverInbound(packet);

        // Good item minted.
        assertEq(intex.balanceOf(recipient, TOKEN_GOOD), 50, "good item must be minted");

        // Bad item recorded in failedCrosschainReceives, NOT minted.
        (address to, uint256 tokenId, uint256 amount, bool exists) =
            nftRouterBnb.getFailedCrosschainReceive(receiveId, 1);
        assertEq(to, recipient, "failed entry.to");
        assertEq(tokenId, TOKEN_BAD, "failed entry.tokenId");
        assertEq(amount, 75, "failed entry.amount");
        assertTrue(exists, "failed entry must exist");

        // Item 0 did NOT fail - no entry for idx=0.
        (,,, bool existsZero) = nftRouterBnb.getFailedCrosschainReceive(receiveId, 0);
        assertFalse(existsZero, "good item idx must have no failed entry");
    }

    function test_BatchReceive_RetryIntexCrosschainReceiveSucceedsAfterUpstreamFix() public {
        address recipient = address(0xCAFE);
        bytes memory packet = _batchPacket(recipient);
        bytes32 receiveId = _receiveId(packet);

        _deliverInbound(packet);

        // Initially the bad item is parked.
        (,,, bool existsBefore) = nftRouterBnb.getFailedCrosschainReceive(receiveId, 1);
        assertTrue(existsBefore, "bad item parked");

        // Fix upstream: create SERIES_BAD now so crosschainMint can succeed.
        intex.openSeries(OpenSeriesLib.params(SERIES_BAD_DAY, 10_000, 0));

        // Anyone can retry - no auth gate.
        vm.prank(address(0xDEAD));
        nftRouterBnb.retryIntexCrosschainReceive(receiveId, 1);

        // Now minted.
        assertEq(intex.balanceOf(recipient, TOKEN_BAD), 75, "retried item must be minted");

        // Entry deleted.
        (,,, bool existsAfter) = nftRouterBnb.getFailedCrosschainReceive(receiveId, 1);
        assertFalse(existsAfter, "entry deleted after retry");
    }

    function test_BatchReceive_RetryIntexCrosschainReceiveTwiceRevertsNoSuchFailedCrosschainReceive() public {
        address recipient = address(0xCAFE);
        bytes memory packet = _batchPacket(recipient);
        bytes32 receiveId = _receiveId(packet);

        _deliverInbound(packet);

        // Fix upstream + retry once.
        intex.openSeries(OpenSeriesLib.params(SERIES_BAD_DAY, 10_000, 0));
        nftRouterBnb.retryIntexCrosschainReceive(receiveId, 1);

        // Second retry must revert, because the slot has been deleted.
        vm.expectRevert(
            abi.encodeWithSelector(IIntexNFT1155Router.NoSuchFailedCrosschainReceive.selector, receiveId, 1)
        );
        nftRouterBnb.retryIntexCrosschainReceive(receiveId, 1);
    }

    function test_BatchReceive_SendBackToSourceConsumesEntryAndSendsReverse() public {
        address recipient = address(0xCAFE);
        bytes memory packet = _batchPacket(recipient);
        bytes32 receiveId = _receiveId(packet);

        _deliverInbound(packet);

        (,,, bool exists) = nftRouterBnb.getFailedCrosschainReceive(receiveId, 1);
        assertTrue(exists, "bad item parked at idx 1");

        // Reclaim routes the stranded item back to its origin peer and consumes the entry.
        nftRouterBnb.sendBackToSource(receiveId, 1);

        (,,, bool existsAfter) = nftRouterBnb.getFailedCrosschainReceive(receiveId, 1);
        assertFalse(existsAfter, "entry consumed on reclaim");

        // The reverse packet is a one-item SEND_MULTI recorded on the bridge.
        bytes memory reverse = bridge.lastPayload();
        assertEq(uint8(reverse[1]), IntexNFT1155RouterCodec.SEND_MULTI, "reverse is SEND_MULTI");

        // A second reclaim reverts - the entry is gone.
        vm.expectRevert(
            abi.encodeWithSelector(IIntexNFT1155Router.NoSuchFailedCrosschainReceive.selector, receiveId, 1)
        );
        nftRouterBnb.sendBackToSource(receiveId, 1);
    }

    function test_BatchReceive_RetryIntexCrosschainReceiveUnknownIdxRevertsNoSuchFailedCrosschainReceive() public {
        bytes32 receiveId = bytes32(uint256(0xAAEE));
        vm.expectRevert(
            abi.encodeWithSelector(IIntexNFT1155Router.NoSuchFailedCrosschainReceive.selector, receiveId, 0)
        );
        nftRouterBnb.retryIntexCrosschainReceive(receiveId, 0);
    }

    // ---------------------------------------------------------------
    // SEND_MULTI batch - per-item isolation across distinct recipients
    // ---------------------------------------------------------------

    function test_MultiReceive_BadItemDoesNotRevertWholeBatch() public {
        address goodRecipient = address(0x1111);
        address badRecipient = address(0x2222);
        bytes memory packet = _multiPacket(goodRecipient, badRecipient);
        bytes32 receiveId = _receiveId(packet);

        _deliverInbound(packet);

        // Good recipient minted.
        assertEq(intex.balanceOf(goodRecipient, TOKEN_GOOD), 50, "good recipient minted");

        // Bad recipient parked.
        (address to,, uint256 amount, bool exists) = nftRouterBnb.getFailedCrosschainReceive(receiveId, 1);
        assertEq(to, badRecipient);
        assertEq(amount, 75);
        assertTrue(exists);

        // Bad recipient did NOT receive tokens.
        assertEq(intex.balanceOf(badRecipient, TOKEN_BAD), 0, "bad recipient must NOT be minted");
    }

    // ---------------------------------------------------------------
    // Self-call shim guard
    // ---------------------------------------------------------------

    function test_CrosschainReceiveOne_ExternalCallerRevertsNotSelf() public {
        vm.expectRevert(IIntexNFT1155Router.NotSelf.selector);
        nftRouterBnb.crosschainReceiveOne(address(0xCAFE), TOKEN_GOOD, 1);
    }

    // ---------------------------------------------------------------
    // Channel liveness - second inbound batch processes normally after a failure
    // ---------------------------------------------------------------

    function test_BatchReceive_SecondBatchProcessesAfterFailure() public {
        address recipient = address(0xCAFE);

        // First batch: one bad item parked.
        _deliverInbound(_batchPacket(recipient));

        // Second batch: same recipient, distinct payload (single good item) -> distinct receiveId.
        uint256[] memory tokenIds = new uint256[](1);
        tokenIds[0] = TOKEN_GOOD;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = 100;
        bytes memory packet = IntexNFT1155RouterCodec.encodeBatch(
            IntexNFT1155RouterCodec.BatchPayload({
                to: bytes32(uint256(uint160(recipient))), intexIds: tokenIds, units: amounts
            })
        );
        _deliverInbound(packet);

        // First batch minted 50 + Second batch minted 100 = 150 of TOKEN_GOOD.
        assertEq(intex.balanceOf(recipient, TOKEN_GOOD), 150, "second batch crosschainMint landed");
    }
}
