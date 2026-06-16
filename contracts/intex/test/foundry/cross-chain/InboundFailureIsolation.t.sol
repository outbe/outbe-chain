// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {Origin} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";

import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {IONFT1155AdapterBatch} from "@contracts/shared/interfaces/IONFT1155AdapterBatch.sol";
import {ONFT1155BatchMsgCodec} from "@contracts/shared/libs/ONFT1155BatchMsgCodec.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";

/// @title InboundFailureIsolationTest
/// @notice Behavioural coverage Pattern B on `ONFT1155AdapterBatch`: a per-item
///         `token.crosschainMint` revert no longer reverts the whole batch — the failure is recorded as
///         a `FailedCrosschainMint` snapshot, `CrosschainMintFailed` is emitted, and `retryCrosschainMint` re-attempts
///         the crosschainMint after the upstream issue is fixed. This is the Critical funds-lock fix
///         from the contract review (R-04).
contract InboundFailureIsolationTest is TestHelperOz5 {
    uint32 internal constant BNB_EID = 1;
    uint32 internal constant OUTBE_EID = 2;

    ONFT1155AdapterBatch internal onftBatchBnb;
    ONFT1155AdapterBatch internal onftBatchOutbe;
    IntexNFT1155 internal intex;

    address internal admin = address(this);

    uint32 internal constant SERIES_GOOD = 20260201;
    uint32 internal constant SERIES_BAD = 20260202;
    uint256 internal constant TOKEN_GOOD = uint256(SERIES_GOOD);
    uint256 internal constant TOKEN_BAD = uint256(SERIES_BAD);

    function setUp() public override {
        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        intex = DeployProxy.intexNFT1155(admin, admin);
        onftBatchBnb = DeployProxy.onftAdapterBatch(address(intex), address(endpoints[BNB_EID]), admin);
        onftBatchOutbe = DeployProxy.onftAdapterBatch(address(intex), address(endpoints[OUTBE_EID]), admin);

        address[] memory batches = new address[](2);
        batches[0] = address(onftBatchBnb);
        batches[1] = address(onftBatchOutbe);
        this.wireOApps(batches);

        // Two series: one Issued (crosschainMint succeeds), one not (crosschainMint reverts on state check).
        intex.createSeries(SERIES_GOOD, 10_000, 0);
        intex.markQualified(SERIES_GOOD);
        // SERIES_BAD intentionally not created — `intex.crosschainMint` will revert on lookup.

        intex.grantRole(intex.RELAYER_ROLE(), address(onftBatchBnb));
    }

    function _deliver(uint32 srcEid, address peer, uint64 nonce, bytes32 guid, bytes memory message) internal {
        Origin memory origin = Origin({srcEid: srcEid, sender: bytes32(uint256(uint160(peer))), nonce: nonce});
        vm.prank(address(endpoints[BNB_EID]));
        (bool ok, bytes memory data) = address(onftBatchBnb)
            .call(
                abi.encodeWithSignature(
                    "lzReceive((uint32,bytes32,uint64),bytes32,bytes,address,bytes)",
                    origin,
                    guid,
                    message,
                    address(0),
                    ""
                )
            );
        if (!ok) {
            assembly {
                revert(add(data, 32), mload(data))
            }
        }
    }

    /// @dev SEND batch (V2 abi.encode): 2 items. Item 0 targets SERIES_GOOD, item 1 SERIES_BAD.
    function _batchPacket(address recipient) internal pure returns (bytes memory) {
        uint256[] memory tokenIds = new uint256[](2);
        tokenIds[0] = TOKEN_GOOD;
        tokenIds[1] = TOKEN_BAD;
        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 50;
        amounts[1] = 75;
        return ONFT1155BatchMsgCodec.encodeBatch(
            ONFT1155BatchMsgCodec.BatchPayload({
                to: bytes32(uint256(uint160(recipient))), tokenIds: tokenIds, amounts: amounts
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
        return ONFT1155BatchMsgCodec.encodeMulti(
            ONFT1155BatchMsgCodec.MultiPayload({recipients: recipients, tokenIds: tokenIds, amounts: amounts})
        );
    }

    // ---------------------------------------------------------------
    // SEND batch — per-item isolation
    // ---------------------------------------------------------------

    function test_BatchReceive_BadItemDoesNotRevertWholeBatch() public {
        address recipient = address(0xCAFE);
        bytes32 guid = bytes32(uint256(0xAABB));

        _deliver(OUTBE_EID, address(onftBatchOutbe), 1, guid, _batchPacket(recipient));

        // Good item minted.
        assertEq(intex.balanceOf(recipient, TOKEN_GOOD), 50, "good item must be minted");

        // Bad item recorded in failedCrosschainMints, NOT minted.
        (address to, uint256 tokenId, uint256 amount,, bool exists) = onftBatchBnb.failedCrosschainMints(guid, 1);
        assertEq(to, recipient, "failed entry.to");
        assertEq(tokenId, TOKEN_BAD, "failed entry.tokenId");
        assertEq(amount, 75, "failed entry.amount");
        assertTrue(exists, "failed entry must exist");

        // Item 0 did NOT fail — no entry for idx=0.
        (,,,, bool existsZero) = onftBatchBnb.failedCrosschainMints(guid, 0);
        assertFalse(existsZero, "good item idx must have no failed entry");
    }

    function test_BatchReceive_RetryCrosschainMintSucceedsAfterUpstreamFix() public {
        address recipient = address(0xCAFE);
        bytes32 guid = bytes32(uint256(0xAACC));

        _deliver(OUTBE_EID, address(onftBatchOutbe), 1, guid, _batchPacket(recipient));

        // Initially the bad item is parked.
        (,,,, bool existsBefore) = onftBatchBnb.failedCrosschainMints(guid, 1);
        assertTrue(existsBefore, "bad item parked");

        // Fix upstream: create SERIES_BAD now so crosschainMint can succeed.
        intex.createSeries(SERIES_BAD, 10_000, 0);
        intex.markQualified(SERIES_BAD);

        // Anyone can retry — no auth gate.
        vm.prank(address(0xDEAD));
        onftBatchBnb.retryCrosschainMint(guid, 1);

        // Now minted.
        assertEq(intex.balanceOf(recipient, TOKEN_BAD), 75, "retried item must be minted");

        // Entry deleted.
        (,,,, bool existsAfter) = onftBatchBnb.failedCrosschainMints(guid, 1);
        assertFalse(existsAfter, "entry deleted after retry");
    }

    function test_BatchReceive_RetryCrosschainMintTwiceRevertsNoSuchFailedCrosschainMint() public {
        address recipient = address(0xCAFE);
        bytes32 guid = bytes32(uint256(0xAADD));

        _deliver(OUTBE_EID, address(onftBatchOutbe), 1, guid, _batchPacket(recipient));

        // Fix upstream + retry once.
        intex.createSeries(SERIES_BAD, 10_000, 0);
        intex.markQualified(SERIES_BAD);
        onftBatchBnb.retryCrosschainMint(guid, 1);

        // Second retry must revert — slot has been deleted.
        vm.expectRevert(abi.encodeWithSelector(IONFT1155AdapterBatch.NoSuchFailedCrosschainMint.selector, guid, 1));
        onftBatchBnb.retryCrosschainMint(guid, 1);
    }

    function test_BatchReceive_RetryCrosschainMintUnknownIdxRevertsNoSuchFailedCrosschainMint() public {
        bytes32 guid = bytes32(uint256(0xAAEE));
        vm.expectRevert(abi.encodeWithSelector(IONFT1155AdapterBatch.NoSuchFailedCrosschainMint.selector, guid, 0));
        onftBatchBnb.retryCrosschainMint(guid, 0);
    }

    // ---------------------------------------------------------------
    // SEND_MULTI batch — per-item isolation across distinct recipients
    // ---------------------------------------------------------------

    function test_MultiReceive_BadItemDoesNotRevertWholeBatch() public {
        address goodRecipient = address(0x1111);
        address badRecipient = address(0x2222);
        bytes32 guid = bytes32(uint256(0xBB11));

        _deliver(OUTBE_EID, address(onftBatchOutbe), 1, guid, _multiPacket(goodRecipient, badRecipient));

        // Good recipient minted.
        assertEq(intex.balanceOf(goodRecipient, TOKEN_GOOD), 50, "good recipient minted");

        // Bad recipient parked.
        (address to,, uint256 amount,, bool exists) = onftBatchBnb.failedCrosschainMints(guid, 1);
        assertEq(to, badRecipient);
        assertEq(amount, 75);
        assertTrue(exists);

        // Bad recipient did NOT receive tokens.
        assertEq(intex.balanceOf(badRecipient, TOKEN_BAD), 0, "bad recipient must NOT be minted");
    }

    // ---------------------------------------------------------------
    // Self-call shim guard
    // ---------------------------------------------------------------

    function test_CrosschainMintOne_ExternalCallerRevertsNotSelf() public {
        vm.expectRevert(IONFT1155AdapterBatch.NotSelf.selector);
        onftBatchBnb.crosschainMintOne(address(0xCAFE), TOKEN_GOOD, 1);
    }

    // ---------------------------------------------------------------
    // Channel liveness — second inbound batch processes normally after a failure
    // ---------------------------------------------------------------

    function test_BatchReceive_SecondBatchProcessesAfterFailure() public {
        address recipient = address(0xCAFE);

        // First batch: one bad item parked.
        _deliver(OUTBE_EID, address(onftBatchOutbe), 1, bytes32(uint256(0xCC01)), _batchPacket(recipient));

        // Second batch: same recipient, different guid, one item targeting SERIES_GOOD.
        uint256[] memory tokenIds = new uint256[](1);
        tokenIds[0] = TOKEN_GOOD;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = 100;
        bytes memory packet = ONFT1155BatchMsgCodec.encodeBatch(
            ONFT1155BatchMsgCodec.BatchPayload({
                to: bytes32(uint256(uint160(recipient))), tokenIds: tokenIds, amounts: amounts
            })
        );
        _deliver(OUTBE_EID, address(onftBatchOutbe), 2, bytes32(uint256(0xCC02)), packet);

        // First batch minted 50 + Second batch minted 100 = 150 of TOKEN_GOOD.
        assertEq(intex.balanceOf(recipient, TOKEN_GOOD), 150, "second batch crosschainMint landed");
    }
}
