// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {Origin} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";

import {TargetMessenger} from "@contracts/bnb/TargetMessenger.sol";
import {OriginMessenger} from "@contracts/outbe/OriginMessenger.sol";
import {ONFT1155Adapter} from "@contracts/shared/ONFT1155Adapter.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {IONFT1155AdapterBatch} from "@contracts/shared/interfaces/IONFT1155AdapterBatch.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {ONFT1155MsgCodec} from "@contracts/shared/libs/ONFT1155MsgCodec.sol";
import {ONFT1155BatchMsgCodec} from "@contracts/shared/libs/ONFT1155BatchMsgCodec.sol";

import {IntexAuction} from "@contracts/bnb/IntexAuction.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";

/// @dev Fallback-only stub used as `auction` for nonce-advancement tests so the TM dispatch
///      path doesn't trip on the real auction's per-series state machine when we deliver two
///      packets that would otherwise need fresh state each.
contract NoOpFallback {
    fallback() external payable {}
    receive() external payable {}
}

/// @title DuplicateProtectionTest
/// @notice Behavioural coverage for duplicate-execution protection per messenger.
/// @dev `TargetMessenger` and `OriginMessenger` enable LayerZero ORDERED delivery via the
///      `nextNonce` override (auction-stage flow is semantically sequential — out-of-order is a
///      bug, not just a duplicate). `ONFT1155Adapter` and `ONFT1155AdapterBatch` carry independent
///      transfers and use a `processed[srcEid][guid]` mapping with `AlreadyProcessed` revert.
contract DuplicateProtectionTest is TestHelperOz5 {
    uint32 internal constant BNB_EID = 1;
    uint32 internal constant OUTBE_EID = 2;

    TargetMessenger internal bnbMessenger;
    OriginMessenger internal outbeMessenger;
    ONFT1155Adapter internal onftBnb;
    ONFT1155Adapter internal onftOutbe;
    ONFT1155AdapterBatch internal onftBatchBnb;
    ONFT1155AdapterBatch internal onftBatchOutbe;

    IntexAuction internal auction;
    IntexNFT1155 internal intex;
    IntexNFT1155 internal intexOutbe;
    address internal desis;
    address internal admin = address(this);

    uint32 internal constant SERIES_ID = 20260101;
    uint256 internal constant TOKEN_ID = uint256(SERIES_ID);

    function setUp() public override {
        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        desis = address(new MockDesis());
        auction = DeployProxy.intexAuction(admin, admin);
        intex = DeployProxy.intexNFT1155(admin, admin);
        intexOutbe = DeployProxy.intexNFT1155(admin, admin);

        bnbMessenger = DeployProxy.targetMessenger(address(endpoints[BNB_EID]), admin, OUTBE_EID);
        outbeMessenger = DeployProxy.originMessenger(address(endpoints[OUTBE_EID]), admin, BNB_EID);
        onftBatchBnb = new ONFT1155AdapterBatch(address(intex), address(endpoints[BNB_EID]), admin);
        onftBatchOutbe = new ONFT1155AdapterBatch(address(intexOutbe), address(endpoints[OUTBE_EID]), admin);

        onftBnb = ONFT1155Adapter(
            _deployOApp(
                type(ONFT1155Adapter).creationCode,
                abi.encode(address(intex), address(endpoints[BNB_EID]), admin, OUTBE_EID)
            )
        );
        onftOutbe = ONFT1155Adapter(
            _deployOApp(
                type(ONFT1155Adapter).creationCode,
                abi.encode(address(intexOutbe), address(endpoints[OUTBE_EID]), admin, BNB_EID)
            )
        );

        address[] memory bridge = new address[](2);
        bridge[0] = address(bnbMessenger);
        bridge[1] = address(outbeMessenger);
        this.wireOApps(bridge);

        address[] memory onfts = new address[](2);
        onfts[0] = address(onftBnb);
        onfts[1] = address(onftOutbe);
        this.wireOApps(onfts);

        address[] memory batches = new address[](2);
        batches[0] = address(onftBatchBnb);
        batches[1] = address(onftBatchOutbe);
        this.wireOApps(batches);

        bnbMessenger.wire(address(auction), address(intex), admin, address(onftBatchBnb));
        outbeMessenger.wire(desis, makeAddr("factory"));

        // Seed the receiving NFT contracts with a series + RELAYER role so ONFT credits succeed
        // and so TM's `_handleMarkCalled` reaches `intex.markCalled` without an auth revert.
        intex.createSeries(SERIES_ID, 10_000, 0);
        intex.markQualified(SERIES_ID);
        intex.grantRole(intex.RELAYER_ROLE(), address(onftBnb));
        intex.grantRole(intex.RELAYER_ROLE(), address(onftBatchBnb));
        intex.grantRole(intex.RELAYER_ROLE(), address(bnbMessenger));
    }

    // --- Helpers ---

    function _deliver(
        address oapp,
        address endpointAddr,
        uint32 srcEid,
        address peer,
        uint64 nonce,
        bytes32 guid,
        bytes memory message
    ) internal {
        Origin memory origin = Origin({srcEid: srcEid, sender: bytes32(uint256(uint160(peer))), nonce: nonce});
        vm.prank(endpointAddr);
        (bool ok, bytes memory data) = oapp.call(
            abi.encodeWithSignature(
                "lzReceive((uint32,bytes32,uint64),bytes32,bytes,address,bytes)", origin, guid, message, address(0), ""
            )
        );
        if (!ok) {
            assembly {
                revert(add(data, 32), mload(data))
            }
        }
    }

    /// @dev `STAGE_START` has the simplest downstream — single `auction.auctionStart` call with
    ///      no return values. NoOp fallback accepts it without needing typed stubs.
    function _stageStartPacket(uint32 seriesId) internal pure returns (bytes memory) {
        return BridgeMsgCodec.encodeAuctionStageStart(seriesId, 100, 200, 300, 1e18, 5e6, 7e6, 11e6, 3);
    }

    function _bidsBatchPacket(uint32 seriesId, uint32 srcEid) internal pure returns (bytes memory) {
        return BridgeMsgCodec.encodeBidsBatch(
            seriesId, srcEid, true, 1, new address[](0), new uint16[](0), new uint64[](0), new uint32[](0)
        );
    }

    function _onftPacket(address to, uint256 tokenId_, uint256 amount_) internal pure returns (bytes memory) {
        return abi.encodePacked(ONFT1155MsgCodec.BODY_VERSION_V1, bytes32(uint256(uint160(to))), tokenId_, amount_);
    }

    function _onftBatchPacket(address to, uint256 tokenId_, uint256 amount_) internal pure returns (bytes memory) {
        uint256[] memory tokenIds = new uint256[](1);
        tokenIds[0] = tokenId_;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = amount_;
        return ONFT1155BatchMsgCodec.encodeBatch(
            ONFT1155BatchMsgCodec.BatchPayload({
                to: bytes32(uint256(uint160(to))), tokenIds: tokenIds, amounts: amounts
            })
        );
    }

    // ---------------------------------------------------------------
    // TargetMessenger — ORDERED nextNonce
    // ---------------------------------------------------------------

    function test_TM_nextNonce_StartsAtOne() public view {
        assertEq(
            bnbMessenger.nextNonce(OUTBE_EID, bytes32(uint256(uint160(address(outbeMessenger))))),
            1,
            "fresh channel should expect nonce 1"
        );
    }

    function test_TM_nextNonce_AdvancesAfterDelivery() public {
        bytes32 senderBytes32 = bytes32(uint256(uint160(address(outbeMessenger))));

        // Swap real auction/intex for fallback stubs so the per-series state machine doesn't
        // reject the second delivery. The nonce-advancement invariant is independent of the
        // downstream's state — we're pinning TM's own bookkeeping.
        address stub = address(new NoOpFallback());
        bnbMessenger.wire(stub, stub, stub, stub);

        _deliver(
            address(bnbMessenger),
            address(endpoints[BNB_EID]),
            OUTBE_EID,
            address(outbeMessenger),
            1,
            bytes32(uint256(0x1111)),
            _stageStartPacket(SERIES_ID)
        );
        assertEq(bnbMessenger.inboundNonce(OUTBE_EID, senderBytes32), 1, "nonce 1 recorded");
        assertEq(bnbMessenger.nextNonce(OUTBE_EID, senderBytes32), 2, "next expected is 2");

        _deliver(
            address(bnbMessenger),
            address(endpoints[BNB_EID]),
            OUTBE_EID,
            address(outbeMessenger),
            2,
            bytes32(uint256(0x2222)),
            _stageStartPacket(SERIES_ID)
        );
        assertEq(bnbMessenger.inboundNonce(OUTBE_EID, senderBytes32), 2, "nonce 2 recorded");
        assertEq(bnbMessenger.nextNonce(OUTBE_EID, senderBytes32), 3, "next expected is 3");
    }

    function test_TM_nextNonce_IsolatedPerSender() public {
        bytes32 senderA = bytes32(uint256(uint160(address(outbeMessenger))));
        bytes32 senderB = bytes32(uint256(uint160(address(0xBEEF))));

        address stub = address(new NoOpFallback());
        bnbMessenger.wire(stub, stub, stub, stub);

        // Advancing the (OUTBE_EID, outbeMessenger) channel must not affect a different sender.
        _deliver(
            address(bnbMessenger),
            address(endpoints[BNB_EID]),
            OUTBE_EID,
            address(outbeMessenger),
            1,
            bytes32(uint256(0x3333)),
            _stageStartPacket(SERIES_ID)
        );
        assertEq(bnbMessenger.nextNonce(OUTBE_EID, senderA), 2);
        assertEq(bnbMessenger.nextNonce(OUTBE_EID, senderB), 1, "isolated sender unaffected");
    }

    // ---------------------------------------------------------------
    // OriginMessenger — ORDERED nextNonce
    // ---------------------------------------------------------------

    function test_OM_nextNonce_StartsAtOne() public view {
        assertEq(
            outbeMessenger.nextNonce(BNB_EID, bytes32(uint256(uint160(address(bnbMessenger))))),
            1,
            "fresh channel should expect nonce 1"
        );
    }

    function test_OM_nextNonce_AdvancesAfterDelivery() public {
        bytes32 senderBytes32 = bytes32(uint256(uint160(address(bnbMessenger))));

        _deliver(
            address(outbeMessenger),
            address(endpoints[OUTBE_EID]),
            BNB_EID,
            address(bnbMessenger),
            1,
            bytes32(uint256(0x4444)),
            _bidsBatchPacket(SERIES_ID, BNB_EID)
        );
        assertEq(outbeMessenger.inboundNonce(BNB_EID, senderBytes32), 1);
        assertEq(outbeMessenger.nextNonce(BNB_EID, senderBytes32), 2);
    }

    // ---------------------------------------------------------------
    // ONFT1155Adapter — idempotency by (srcEid, guid)
    // ---------------------------------------------------------------

    function test_ONFT_RedeliveredGuid_RevertsAlreadyProcessed() public {
        address recipient = address(0xCAFE);
        bytes32 guid = bytes32(uint256(0xAA11));
        bytes memory packet = _onftPacket(recipient, TOKEN_ID, 1);

        _deliver(address(onftBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftOutbe), 1, guid, packet);

        vm.expectRevert(abi.encodeWithSelector(ONFT1155Adapter.AlreadyProcessed.selector, OUTBE_EID, guid));
        _deliver(address(onftBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftOutbe), 2, guid, packet);
    }

    function test_ONFT_DistinctGuid_Succeeds() public {
        address recipient = address(0xCAFE);
        bytes memory packet = _onftPacket(recipient, TOKEN_ID, 1);

        _deliver(
            address(onftBnb),
            address(endpoints[BNB_EID]),
            OUTBE_EID,
            address(onftOutbe),
            1,
            bytes32(uint256(0xAA22)),
            packet
        );
        // Second delivery uses a different guid — must succeed.
        _deliver(
            address(onftBnb),
            address(endpoints[BNB_EID]),
            OUTBE_EID,
            address(onftOutbe),
            2,
            bytes32(uint256(0xAA33)),
            packet
        );
    }

    // ---------------------------------------------------------------
    // ONFT1155AdapterBatch — idempotency by (srcEid, guid)
    // ---------------------------------------------------------------

    function test_ONFTBatch_RedeliveredGuid_RevertsAlreadyProcessed() public {
        address recipient = address(0xBEEF);
        bytes32 guid = bytes32(uint256(0xBB11));
        bytes memory packet = _onftBatchPacket(recipient, TOKEN_ID, 1);

        _deliver(
            address(onftBatchBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftBatchOutbe), 1, guid, packet
        );

        vm.expectRevert(abi.encodeWithSelector(IONFT1155AdapterBatch.AlreadyProcessed.selector, OUTBE_EID, guid));
        _deliver(
            address(onftBatchBnb), address(endpoints[BNB_EID]), OUTBE_EID, address(onftBatchOutbe), 2, guid, packet
        );
    }

    function test_ONFTBatch_DistinctGuid_Succeeds() public {
        address recipient = address(0xBEEF);
        bytes memory packet = _onftBatchPacket(recipient, TOKEN_ID, 1);

        _deliver(
            address(onftBatchBnb),
            address(endpoints[BNB_EID]),
            OUTBE_EID,
            address(onftBatchOutbe),
            1,
            bytes32(uint256(0xBB22)),
            packet
        );
        _deliver(
            address(onftBatchBnb),
            address(endpoints[BNB_EID]),
            OUTBE_EID,
            address(onftBatchOutbe),
            2,
            bytes32(uint256(0xBB33)),
            packet
        );
    }
}
