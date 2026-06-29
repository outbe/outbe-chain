// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {ONFT1155Adapter} from "@contracts/shared/ONFT1155Adapter.sol";
import {IONFT1155Adapter, SendParam} from "@contracts/shared/interfaces/IONFT1155Adapter.sol";
import {MessagingFee, MessagingReceipt, Origin} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";
import {ONFT1155MsgCodec} from "@contracts/shared/libs/ONFT1155MsgCodec.sol";

import {OptionsBuilder} from "@layerzerolabs/oapp-evm/oapp/libs/OptionsBuilder.sol";
import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";

import "forge-std/console.sol";
import {RejectingReceiver} from "@test-mocks/RejectingReceiver.sol";

/**
 * @title ONFT1155AdapterTest
 * @notice Foundry tests for ONFT1155Adapter with IntexNFT1155 token
 * @dev Tests cross-chain transfers using LayerZero mock infrastructure.
 *      Series are keyed by `seriesId` (uint32); the issued token id is `uint256(seriesId)`.
 */
contract ONFT1155AdapterTest is TestHelperOz5 {
    using OptionsBuilder for bytes;

    uint32 private aEid = 1;
    uint32 private bEid = 2;

    IntexNFT1155 private tokenA;
    IntexNFT1155 private tokenB;
    ONFT1155Adapter private adapterA;
    ONFT1155Adapter private adapterB;

    address private user = address(0x1);
    uint32 private constant SERIES_ID = 20260401;
    uint256 private constant TOKEN_ID = uint256(SERIES_ID);
    uint256 private constant AMOUNT = 100;
    uint32 private constant ISSUED_INTEX_COUNT = 10_000;

    function setUp() public virtual override {
        vm.deal(user, 1000 ether);

        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        // Deploy IntexNFT1155 tokens on both chains
        tokenA = DeployProxy.intexNFT1155(address(this), address(this));
        tokenB = DeployProxy.intexNFT1155(address(this), address(this));

        adapterA = DeployProxy.onftAdapter(address(tokenA), address(endpoints[aEid]), address(this));
        adapterB = DeployProxy.onftAdapter(address(tokenB), address(endpoints[bEid]), address(this));

        // Grant RELAYER_ROLE to adapters
        tokenA.grantRole(tokenA.RELAYER_ROLE(), address(adapterA));
        tokenB.grantRole(tokenB.RELAYER_ROLE(), address(adapterB));

        // Wire adapters (set peers)
        address[] memory oapps = new address[](2);
        oapps[0] = address(adapterA);
        oapps[1] = address(adapterB);
        this.wireOApps(oapps);

        // Create series on both chains
        tokenA.createSeries(CreateSeriesLib.params(SERIES_ID, ISSUED_INTEX_COUNT, 0));
        tokenB.createSeries(CreateSeriesLib.params(SERIES_ID, ISSUED_INTEX_COUNT, 0));

        // Bridge is only allowed in Qualified state for the user-driven adapter.
        tokenA.markQualified(SERIES_ID);
        tokenB.markQualified(SERIES_ID);

        // Mint initial tokens to user on chain A
        tokenA.mint(user, AMOUNT, SERIES_ID);
    }

    function test_constructor() public view {
        assertEq(adapterA.owner(), address(this));
        assertEq(address(adapterA.token()), address(tokenA));
        assertEq(address(adapterA.endpoint()), address(endpoints[aEid]));
    }

    /// @notice `token` is immutable, so a zero address would permanently brick the
    ///         adapter (every `crosschainBurn`/`crosschainMint` reverts on a non-contract). Reject at construction.
    function test_constructor_revertsZeroToken() public {
        // Property of the implementation constructor — the token immutable is set there.
        vm.expectRevert(abi.encodeWithSelector(IONFT1155Adapter.ZeroAddress.selector, "token"));
        new ONFT1155Adapter(address(0), address(endpoints[aEid]));
    }

    function test_send() public {
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(400000, 0);

        SendParam memory sendParam = SendParam({
            dstEid: bEid,
            to: addressToBytes32(user),
            tokenId: TOKEN_ID,
            amount: AMOUNT,
            extraOptions: options,
            composeMsg: ""
        });

        MessagingFee memory fee = adapterA.quoteSend(sendParam, false);

        // Check initial balances
        assertEq(tokenA.balanceOf(user, TOKEN_ID), AMOUNT);
        assertEq(tokenB.balanceOf(user, TOKEN_ID), 0);

        // Send tokens
        vm.prank(user);
        adapterA.send{value: fee.nativeFee}(sendParam, fee, user);

        // Process cross-chain message
        verifyPackets(bEid, addressToBytes32(address(adapterB)));

        // Check final balances
        assertEq(tokenA.balanceOf(user, TOKEN_ID), 0);
        assertEq(tokenB.balanceOf(user, TOKEN_ID), AMOUNT);
    }

    function test_send_partial_amount() public {
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(400000, 0);
        uint256 sendAmount = AMOUNT / 2;

        SendParam memory sendParam = SendParam({
            dstEid: bEid,
            to: addressToBytes32(user),
            tokenId: TOKEN_ID,
            amount: sendAmount,
            extraOptions: options,
            composeMsg: ""
        });

        MessagingFee memory fee = adapterA.quoteSend(sendParam, false);

        vm.prank(user);
        adapterA.send{value: fee.nativeFee}(sendParam, fee, user);
        verifyPackets(bEid, addressToBytes32(address(adapterB)));

        assertEq(tokenA.balanceOf(user, TOKEN_ID), AMOUNT - sendAmount);
        assertEq(tokenB.balanceOf(user, TOKEN_ID), sendAmount);
    }

    function test_send_to_different_address() public {
        address recipient = address(0x2);
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(400000, 0);

        SendParam memory sendParam = SendParam({
            dstEid: bEid,
            to: addressToBytes32(recipient),
            tokenId: TOKEN_ID,
            amount: AMOUNT,
            extraOptions: options,
            composeMsg: ""
        });

        MessagingFee memory fee = adapterA.quoteSend(sendParam, false);

        vm.prank(user);
        adapterA.send{value: fee.nativeFee}(sendParam, fee, user);
        verifyPackets(bEid, addressToBytes32(address(adapterB)));

        assertEq(tokenA.balanceOf(user, TOKEN_ID), 0);
        assertEq(tokenB.balanceOf(recipient, TOKEN_ID), AMOUNT);
    }

    function test_roundtrip() public {
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(400000, 0);

        // Send A -> B
        SendParam memory sendParamAB = SendParam({
            dstEid: bEid,
            to: addressToBytes32(user),
            tokenId: TOKEN_ID,
            amount: AMOUNT,
            extraOptions: options,
            composeMsg: ""
        });

        MessagingFee memory feeAB = adapterA.quoteSend(sendParamAB, false);
        vm.prank(user);
        adapterA.send{value: feeAB.nativeFee}(sendParamAB, feeAB, user);
        verifyPackets(bEid, addressToBytes32(address(adapterB)));

        assertEq(tokenB.balanceOf(user, TOKEN_ID), AMOUNT);

        // Send B -> A
        SendParam memory sendParamBToA = SendParam({
            dstEid: aEid,
            to: addressToBytes32(user),
            tokenId: TOKEN_ID,
            amount: AMOUNT,
            extraOptions: options,
            composeMsg: ""
        });

        MessagingFee memory feeBToA = adapterB.quoteSend(sendParamBToA, false);
        vm.prank(user);
        adapterB.send{value: feeBToA.nativeFee}(sendParamBToA, feeBToA, user);
        verifyPackets(aEid, addressToBytes32(address(adapterA)));

        assertEq(tokenA.balanceOf(user, TOKEN_ID), AMOUNT);
        assertEq(tokenB.balanceOf(user, TOKEN_ID), 0);
    }

    function test_revert_invalid_receiver() public {
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(400000, 0);

        SendParam memory sendParam = SendParam({
            dstEid: bEid, to: bytes32(0), tokenId: TOKEN_ID, amount: AMOUNT, extraOptions: options, composeMsg: ""
        });

        vm.expectRevert(IONFT1155Adapter.InvalidReceiver.selector);
        adapterA.quoteSend(sendParam, false);
    }

    function test_revert_insufficient_balance() public {
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(400000, 0);

        SendParam memory sendParam = SendParam({
            dstEid: bEid,
            to: addressToBytes32(user),
            tokenId: TOKEN_ID,
            amount: AMOUNT + 1, // More than balance
            extraOptions: options,
            composeMsg: ""
        });

        MessagingFee memory fee = adapterA.quoteSend(sendParam, false);

        vm.prank(user);
        vm.expectRevert();
        adapterA.send{value: fee.nativeFee}(sendParam, fee, user);
    }

    function test_intex_state_preserved_after_bridge() public {
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(400000, 0);

        // Get initial state on chain A (setUp marked the series Qualified for bridging).
        IIntexNFT1155.SeriesData memory dataA = tokenA.readData(SERIES_ID);
        assertEq(uint8(dataA.state), uint8(IIntexNFT1155.IntexState.Qualified));

        // Send tokens A -> B
        SendParam memory sendParam = SendParam({
            dstEid: bEid,
            to: addressToBytes32(user),
            tokenId: TOKEN_ID,
            amount: AMOUNT,
            extraOptions: options,
            composeMsg: ""
        });

        MessagingFee memory fee = adapterA.quoteSend(sendParam, false);
        vm.prank(user);
        adapterA.send{value: fee.nativeFee}(sendParam, fee, user);
        verifyPackets(bEid, addressToBytes32(address(adapterB)));

        // Verify state is identical on chain B
        IIntexNFT1155.SeriesData memory dataB = tokenB.readData(SERIES_ID);
        assertEq(uint8(dataB.state), uint8(IIntexNFT1155.IntexState.Qualified));
    }

    // --- sweepNative Tests ---
    function test_sweepNative_success() public {
        vm.deal(address(adapterA), 4 ether);
        address payable recipient = payable(address(0xBEEF));
        uint256 before = recipient.balance;

        adapterA.sweepNative(recipient, 4 ether);

        assertEq(recipient.balance - before, 4 ether);
        assertEq(address(adapterA).balance, 0);
    }

    function test_sweepNative_revert_zeroTo() public {
        vm.deal(address(adapterA), 1 ether);
        vm.expectRevert(abi.encodeWithSelector(IONFT1155Adapter.ZeroAddress.selector, "to"));
        adapterA.sweepNative(payable(address(0)), 1 ether);
    }

    function test_sweepNative_revert_insufficientBalance() public {
        vm.deal(address(adapterA), 1 ether);
        vm.expectRevert(abi.encodeWithSelector(IONFT1155Adapter.NativeBalanceInsufficient.selector, 1 ether, 2 ether));
        adapterA.sweepNative(payable(address(0xBEEF)), 2 ether);
    }

    function test_sweepNative_revert_failedCall() public {
        vm.deal(address(adapterA), 1 ether);
        RejectingReceiver rejector = new RejectingReceiver();
        vm.expectRevert(IONFT1155Adapter.NativeSweepFailed.selector);
        adapterA.sweepNative(payable(address(rejector)), 1 ether);
    }

    function test_sweepNative_revert_unauthorized() public {
        vm.deal(address(adapterA), 1 ether);
        vm.prank(user);
        vm.expectRevert();
        adapterA.sweepNative(payable(address(0xBEEF)), 1 ether);
    }

    // --- Inbound crosschainMint isolation ---

    function test_inboundCrosschainMintFailure_parksAndRetries() public {
        // A series that exists on A only: the destination crosschainMint reverts NonexistentToken, which
        // must park the transfer (not unwind the packet and strand the burned tokens).
        uint32 failSeries = 20260402;
        uint256 failTokenId = uint256(failSeries);
        tokenA.createSeries(CreateSeriesLib.params(failSeries, ISSUED_INTEX_COUNT, 0));
        tokenA.markQualified(failSeries);
        tokenA.mint(user, AMOUNT, failSeries);

        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(400000, 0);
        SendParam memory sendParam = SendParam({
            dstEid: bEid,
            to: addressToBytes32(user),
            tokenId: failTokenId,
            amount: AMOUNT,
            extraOptions: options,
            composeMsg: ""
        });
        MessagingFee memory fee = adapterA.quoteSend(sendParam, false);
        vm.prank(user);
        MessagingReceipt memory r = adapterA.send{value: fee.nativeFee}(sendParam, fee, user);

        // Delivery must succeed (crosschainMint parked), not revert.
        verifyPackets(bEid, addressToBytes32(address(adapterB)));

        // Source burned; destination not minted; transfer parked under the packet guid.
        assertEq(tokenA.balanceOf(user, failTokenId), 0, "source burned");
        assertEq(tokenB.balanceOf(user, failTokenId), 0, "not minted yet");
        (address to,, uint256 amount,,, bool exists) = adapterB.failedCrosschainMints(r.guid);
        assertTrue(exists, "crosschainMint parked");
        assertEq(to, user);
        assertEq(amount, AMOUNT);

        // Fix the cause on B, then retry → minted and entry cleared.
        tokenB.createSeries(CreateSeriesLib.params(failSeries, ISSUED_INTEX_COUNT, 0));
        tokenB.markQualified(failSeries);
        adapterB.retryCrosschainMint(r.guid);
        assertEq(tokenB.balanceOf(user, failTokenId), AMOUNT, "minted on retry");

        (,,,,, bool existsAfter) = adapterB.failedCrosschainMints(r.guid);
        assertFalse(existsAfter, "entry cleared");

        // A re-retry reverts.
        vm.expectRevert(abi.encodeWithSelector(ONFT1155Adapter.NoSuchFailedCrosschainMint.selector, r.guid));
        adapterB.retryCrosschainMint(r.guid);
    }

    function test_retryCrosschainMint_revertsForUnknownGuid() public {
        bytes32 unknown = bytes32(uint256(0xABCD));
        vm.expectRevert(abi.encodeWithSelector(ONFT1155Adapter.NoSuchFailedCrosschainMint.selector, unknown));
        adapterB.retryCrosschainMint(unknown);
    }

    function test_crosschainMintOne_externalCallerRevertsNotSelf() public {
        vm.expectRevert(ONFT1155Adapter.NotSelf.selector);
        adapterB.crosschainMintOne(address(0xCAFE), TOKEN_ID, 1);
    }

    // Direct inbound packet: body shorter than MIN_LEN_TRANSFER (97) must revert before any field
    // is decoded. The codec's fixed-offset slices would otherwise panic; the explicit assert
    // surfaces a typed error instead.
    function test_lzReceive_ShortBody_RevertsInvalidPayloadLength() public {
        Origin memory origin = Origin({srcEid: aEid, sender: addressToBytes32(address(adapterA)), nonce: 99});
        bytes memory shortBody = new bytes(96);
        bytes32 guid = bytes32(uint256(0xABCD));

        vm.expectRevert(
            abi.encodeWithSelector(ONFT1155MsgCodec.InvalidPayloadLength.selector, uint256(96), uint256(97))
        );
        vm.prank(address(endpoints[bEid]));
        adapterB.lzReceive(origin, guid, shortBody, address(0), "");
    }

    // Direct inbound packet: well-formed length and body version, but the sendTo bytes32 has
    // non-zero high bits. assertAddress must reject before bytes32ToAddress silently truncates.
    function test_lzReceive_MalformedAddress_RevertsMalformedAddress() public {
        bytes32 dirty = bytes32((uint256(0xFF) << 160) | uint256(uint160(user)));
        bytes memory payload = abi.encodePacked(uint8(1), dirty, uint256(TOKEN_ID), uint256(AMOUNT));
        Origin memory origin = Origin({srcEid: aEid, sender: addressToBytes32(address(adapterA)), nonce: 100});
        bytes32 guid = bytes32(uint256(0xBEEF));

        vm.expectRevert(abi.encodeWithSelector(ONFT1155MsgCodec.MalformedAddress.selector, dirty));
        vm.prank(address(endpoints[bEid]));
        adapterB.lzReceive(origin, guid, payload, address(0), "");
    }
}
