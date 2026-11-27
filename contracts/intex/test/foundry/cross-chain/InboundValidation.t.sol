// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IntexNFT1155RouterCodec} from "@contracts/shared/libs/IntexNFT1155RouterCodec.sol";
import {IIntexNFT1155Router} from "@contracts/shared/interfaces/IIntexNFT1155Router.sol";

import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";

/// @title InboundValidationTest
/// @notice Inbound validation over the ERC-7786 bridge: a malformed/unknown payload no longer
///         advances a lane silently. Instead the router/adapter reverts with a typed error, the
///         bridge rolls back, and the transport redelivers. Each case asserts that the exact typed
///         revert propagates out of the delivery.
/// @dev IntexAuctionTargetRouter takes its messages from the Interchain Account of Outbe and the NFT bridge
///      from the loopback bridge as the authenticated peer, so the payload is the only thing under
///      test.
contract InboundValidationTest is CrossChainTest {
    address internal constant OWNER = address(0x5E4D);
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant OUTBE_CHAIN_ID = 2;

    IntexAuctionTargetRouter internal bnbRouter;
    IntexNFT1155Router internal nftRouterBnb;
    IntexNFT1155Router internal nftRouterOutbe;

    IntexAuction internal auction;
    IntexNFT1155 internal intex;
    address internal admin = address(this);
    address internal outbeIca = makeAddr("outbeIca");

    function setUp() public {
        _setUpBridge();

        auction = DeployProxy.intexAuction(admin, admin);
        intex = DeployProxy.intexNFT1155(admin, admin);

        bnbRouter = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        nftRouterBnb = DeployProxy.intexNFT1155Router(address(intex), address(bridge), admin);

        IntexNFT1155 intexOutbe = DeployProxy.intexNFT1155(admin, admin);
        nftRouterOutbe = DeployProxy.intexNFT1155Router(address(intexOutbe), address(bridge), admin);

        // Wire the inbound sources so authentication passes and the payload is what fails.
        _wireOutbeIca(bnbRouter, outbeIca);
        nftRouterBnb.setRemoteRouter(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(nftRouterOutbe)));

        bnbRouter.setDependencies(address(auction), address(intex), admin);
    }

    // ---------------------------------------------------------------
    // IntexAuctionTargetRouter - BridgeMsgCodec validation
    // ---------------------------------------------------------------

    function test_TM_TooShortPayload_RevertsInvalidPayloadLength() public {
        // Only the bodyVersion byte - header itself is shorter than 2.
        bytes memory packet = hex"01";
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.InvalidPayloadLength.selector, 0, 1, 2));
        _deliver(OUTBE_CHAIN_ID, outbeIca, address(bnbRouter), packet);
    }

    function test_TM_UnknownMsgType_RevertsUnknownMsgType() public {
        // Header is well-formed (version=1, msgType=0xFE) but msgType is not in TM's accepted set.
        bytes memory packet = hex"01FE";
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.UnknownMsgType.selector, 0xFE));
        _deliver(OUTBE_CHAIN_ID, outbeIca, address(bnbRouter), packet);
    }

    function test_TM_ShortCallNotice_RevertsInvalidPayloadLength() public {
        // INTEX_SERIES_CALL_NOTICE min length = 6 (bodyVersion + msgType + seriesId(4)). Build a 5-byte packet.
        uint24 truncatedSeriesId = 20_250;
        bytes memory packet = abi.encodePacked(
            BridgeMsgCodec.BODY_VERSION_V1, BridgeMsgCodec.MSG_INTEX_SERIES_CALL_NOTICE, truncatedSeriesId
        );
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_INTEX_SERIES_CALL_NOTICE,
                5,
                BridgeMsgCodec.MIN_LEN_INTEX_SERIES_CALL_NOTICE
            )
        );
        _deliver(OUTBE_CHAIN_ID, outbeIca, address(bnbRouter), packet);
    }

    function test_TM_ShortRefundInstructions_RevertsInvalidPayloadLength() public {
        // REFUND_INSTRUCTIONS carries one ABI-encoded array. Its minimum (HEADER_LEN + 288)
        // pins the empty-array floor. Send one byte under it to trip the per-type length check.
        uint256 minLen = BridgeMsgCodec.MIN_LEN_INTEX_AUCTION_WINNERS;
        bytes memory packet = abi.encodePacked(
            BridgeMsgCodec.BODY_VERSION_V1, BridgeMsgCodec.MSG_INTEX_AUCTION_WINNERS, new bytes(minLen - 3)
        );
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector,
                BridgeMsgCodec.MSG_INTEX_AUCTION_WINNERS,
                minLen - 1,
                minLen
            )
        );
        _deliver(OUTBE_CHAIN_ID, outbeIca, address(bnbRouter), packet);
    }

    function test_TM_ShortIssuanceInstructions_RevertsInvalidPayloadLength() public {
        // ISSUANCE_INSTRUCTIONS has the largest minimum (HEADER_LEN + 544): a struct with two
        // arrays. One byte under the floor must trip the per-type length check.
        uint256 minLen = BridgeMsgCodec.MIN_LEN_INTEX_SERIES;
        bytes memory packet =
            abi.encodePacked(BridgeMsgCodec.BODY_VERSION_V1, BridgeMsgCodec.MSG_INTEX_SERIES, new bytes(minLen - 3));
        vm.expectRevert(
            abi.encodeWithSelector(
                BridgeMsgCodec.InvalidPayloadLength.selector, BridgeMsgCodec.MSG_INTEX_SERIES, minLen - 1, minLen
            )
        );
        _deliver(OUTBE_CHAIN_ID, outbeIca, address(bnbRouter), packet);
    }

    function test_TM_RefundPartialOutsideWinners_Reverts() public {
        // REFUND_INSTRUCTIONS naming a partial fill past its winners must revert a typed error,
        // not index out of bounds inside the handler.
        address[] memory winners = new address[](2);
        winners[0] = address(0xB1);
        winners[1] = address(0xB2);

        bytes memory packet = abi.encodePacked(
            BridgeMsgCodec.BODY_VERSION_V1,
            BridgeMsgCodec.MSG_INTEX_AUCTION_WINNERS,
            abi.encode(uint32(42), uint16(0), uint16(1), uint64(600_000), uint128(1e6), winners, uint16(2), uint16(1))
        );
        vm.expectRevert(abi.encodeWithSelector(BridgeMsgCodec.InvalidReturnPartial.selector, uint16(2), uint256(2)));
        _deliver(OUTBE_CHAIN_ID, outbeIca, address(bnbRouter), packet);
    }

    // ---------------------------------------------------------------
    // IntexNFT1155Router codec: version + length + size + msgType + address validation
    // ---------------------------------------------------------------

    function test_NFTBatch_UnknownMsgType_RevertsUnknownMsgType() public {
        // Valid V1 version byte, unknown msgType 0x99 - routing rejects it.
        bytes memory packet = hex"0199";
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155Router.UnknownMsgType.selector, 0x99));
        _deliverToBatch(packet);
    }

    function test_NFTBatch_Version2_RevertsUnsupportedBodyVersion() public {
        // An unknown version must fail closed rather than misdecode into a wrong crosschainMint.
        bytes memory packet = _batch(address(0xCAFE), 1, 100);
        packet[0] = bytes1(uint8(2));
        vm.expectRevert(abi.encodeWithSelector(IntexNFT1155RouterCodec.UnsupportedBodyVersion.selector, uint8(2)));
        _deliverToBatch(packet);
    }

    function test_NFTBatch_ShortHeader_RevertsInvalidPayloadLength() public {
        // A packet shorter than the [version][msgType] header cannot even be routed.
        bytes memory packet = hex"01";
        vm.expectRevert(abi.encodeWithSelector(IntexNFT1155RouterCodec.InvalidPayloadLength.selector, 1, 2));
        _deliverToBatch(packet);
    }

    function test_NFTBatch_TruncatedBody_Reverts() public {
        // Valid header but the abi.encode body is truncated - abi.decode rejects it (no misread).
        bytes memory packet = abi.encodePacked(
            IntexNFT1155RouterCodec.BODY_VERSION_V1, IntexNFT1155RouterCodec.SEND_BATCH, hex"deadbeef"
        );
        vm.expectRevert();
        _deliverToBatch(packet);
    }

    function test_NFTBatch_OverCap_RevertsBatchTooLarge() public {
        // Inbound decoded array length is capped at MAX_BATCH_SIZE.
        uint256 over = IntexNFT1155RouterCodec.MAX_BATCH_SIZE + 1;
        uint256[] memory intexIds = new uint256[](over);
        uint256[] memory amounts = new uint256[](over);
        bytes memory packet = IntexNFT1155RouterCodec.encodeBatch(
            IntexNFT1155RouterCodec.BatchPayload({
                owner: OWNER, to: bytes32(uint256(uint160(address(0xCAFE)))), intexIds: intexIds, units: amounts
            })
        );
        vm.expectRevert(
            abi.encodeWithSelector(
                IntexNFT1155RouterCodec.BatchTooLarge.selector, over, IntexNFT1155RouterCodec.MAX_BATCH_SIZE
            )
        );
        _deliverToBatch(packet);
    }

    function test_NFTBatch_ZeroRecipient_RevertsInvalidReceiver() public {
        // An all-zero recipient passes the high-bit check but is explicitly rejected.
        bytes32[] memory recipients = new bytes32[](1);
        recipients[0] = bytes32(0);
        uint256[] memory intexIds = new uint256[](1);
        intexIds[0] = 1;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = 100;
        bytes memory packet = IntexNFT1155RouterCodec.encodeMulti(
            IntexNFT1155RouterCodec.MultiPayload({
                owner: OWNER, recipients: recipients, intexIds: intexIds, units: amounts
            })
        );
        vm.expectRevert(IIntexNFT1155Router.InvalidReceiver.selector);
        _deliverToBatch(packet);
    }

    function test_NFTBatch_MalformedTo_RevertsMalformedAddress() public {
        // A `to` with non-zero high bits is rejected before any crosschainMint (empty item arrays).
        bytes32 badTo = bytes32(uint256(1) << 200);
        bytes memory packet = abi.encodePacked(
            IntexNFT1155RouterCodec.BODY_VERSION_V1,
            IntexNFT1155RouterCodec.SEND_BATCH,
            abi.encode(
                IntexNFT1155RouterCodec.BatchPayload({
                    owner: OWNER, to: badTo, intexIds: new uint256[](0), units: new uint256[](0)
                })
            )
        );
        vm.expectRevert(abi.encodeWithSelector(IntexNFT1155RouterCodec.MalformedAddress.selector, badTo));
        _deliverToBatch(packet);
    }

    function test_NFTBatch_MalformedRecipient_RevertsMalformedAddress() public {
        // SEND_MULTI with one malformed recipient (non-zero high bits).
        bytes32 badRecipient = bytes32(uint256(1) << 200);
        bytes32[] memory recipients = new bytes32[](1);
        recipients[0] = badRecipient;
        uint256[] memory intexIds = new uint256[](1);
        intexIds[0] = 1;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = 100;
        bytes memory packet = IntexNFT1155RouterCodec.encodeMulti(
            IntexNFT1155RouterCodec.MultiPayload({
                owner: OWNER, recipients: recipients, intexIds: intexIds, units: amounts
            })
        );
        vm.expectRevert(abi.encodeWithSelector(IntexNFT1155RouterCodec.MalformedAddress.selector, badRecipient));
        _deliverToBatch(packet);
    }

    function test_NFTBatch_ZeroTo_RevertsInvalidReceiver() public {
        // SEND_BATCH branch parity to the SEND_MULTI ZeroRecipient test: assertAddress passes for
        // bytes32(0), so the explicit `if (p.to == bytes32(0))` reject is what stops the crosschainMint.
        bytes memory packet = abi.encodePacked(
            IntexNFT1155RouterCodec.BODY_VERSION_V1,
            IntexNFT1155RouterCodec.SEND_BATCH,
            abi.encode(
                IntexNFT1155RouterCodec.BatchPayload({
                    owner: OWNER, to: bytes32(0), intexIds: new uint256[](0), units: new uint256[](0)
                })
            )
        );
        vm.expectRevert(IIntexNFT1155Router.InvalidReceiver.selector);
        _deliverToBatch(packet);
    }

    function test_NFTBatch_MultiOverCap_RevertsBatchTooLarge() public {
        // SEND_MULTI cap parity to the SEND_BATCH OverCap test - decodeMulti rejects oversize arrays
        // before the per-item loop touches any recipient or crosschainMint.
        uint256 over = IntexNFT1155RouterCodec.MAX_BATCH_SIZE + 1;
        bytes32[] memory recipients = new bytes32[](over);
        uint256[] memory intexIds = new uint256[](over);
        uint256[] memory amounts = new uint256[](over);
        bytes memory packet = IntexNFT1155RouterCodec.encodeMulti(
            IntexNFT1155RouterCodec.MultiPayload({
                owner: OWNER, recipients: recipients, intexIds: intexIds, units: amounts
            })
        );
        vm.expectRevert(
            abi.encodeWithSelector(
                IntexNFT1155RouterCodec.BatchTooLarge.selector, over, IntexNFT1155RouterCodec.MAX_BATCH_SIZE
            )
        );
        _deliverToBatch(packet);
    }

    function test_NFTBatch_MultiArrayMismatch_RevertsArrayLengthMismatch() public {
        // SEND_MULTI array-length mismatch propagates the codec revert through dispatch.
        // recipients.length != intexIds.length is enough to trip decodeMulti's guard.
        bytes32[] memory recipients = new bytes32[](2);
        recipients[0] = bytes32(uint256(uint160(address(0xCAFE))));
        recipients[1] = bytes32(uint256(uint160(address(0xBEEF))));
        uint256[] memory intexIds = new uint256[](1);
        intexIds[0] = 1;
        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 10;
        amounts[1] = 20;
        bytes memory packet = abi.encodePacked(
            IntexNFT1155RouterCodec.BODY_VERSION_V1,
            IntexNFT1155RouterCodec.SEND_MULTI,
            abi.encode(
                IntexNFT1155RouterCodec.MultiPayload({
                    owner: OWNER, recipients: recipients, intexIds: intexIds, units: amounts
                })
            )
        );
        vm.expectRevert(IntexNFT1155RouterCodec.ArrayLengthMismatch.selector);
        _deliverToBatch(packet);
    }

    // --- Helpers ---

    /// @dev Deliver a batch packet to `nftRouterBnb` from its wired peer on OUTBE_CHAIN_ID.
    function _deliverToBatch(bytes memory packet) internal {
        _deliver(OUTBE_CHAIN_ID, address(nftRouterOutbe), address(nftRouterBnb), packet);
    }

    /// @dev Build a one-item SEND_BATCH packet for a single recipient.
    function _batch(address to, uint256 intexId_, uint256 amount_) internal pure returns (bytes memory) {
        uint256[] memory intexIds = new uint256[](1);
        intexIds[0] = intexId_;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = amount_;
        return IntexNFT1155RouterCodec.encodeBatch(
            IntexNFT1155RouterCodec.BatchPayload({
                owner: OWNER, to: bytes32(uint256(uint160(to))), intexIds: intexIds, units: amounts
            })
        );
    }
}
