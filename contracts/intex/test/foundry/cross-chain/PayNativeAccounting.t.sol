// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {ITargetRouter} from "@contracts/target/interfaces/ITargetRouter.sol";
import {OriginRouter} from "@contracts/origin/OriginRouter.sol";
import {IntexNFT1155Bridge} from "@contracts/shared/IntexNFT1155Bridge.sol";
import {SendParam} from "@contracts/shared/interfaces/IIntexNFT1155Bridge.sol";
import {ERC7786MessengerBase} from "@contracts/shared/ERC7786MessengerBase.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";

/// @title PayNativeAccountingTest
/// @notice Behavioural coverage for the native-fee funding logic that {ERC7786MessengerBase-_send} owns for every
///         intex bridge client. Two calling conventions are distinguished:
///           * entry-funded (`msg.value > 0`): the send must cover the quoted fee and refund the excess to the
///             caller, so an entry caller's buffer never silently seeds (or drains) the contract's relay float;
///           * relay-funded (`msg.value == 0`): a chain-native module that cannot attach value triggered the send, so
///             the fee is drawn from the contract's pre-funded native float and reverts `NotEnoughNative` when short.
///         Conflating the two would let an entry caller's `msg.value` seed future relay sends without refund, or let
///         an entry caller drain the relay float.
/// @dev Entry path is driven through the user-facing `IntexNFT1155Bridge.send` (payable). The relay/float path
///      is driven both directly (`IntexNFT1155Bridge.systemMultiSend`, not payable → `msg.value == 0`) and through
///      an inbound MARK_CALLED delivery whose `_handleMarkCalled` handler fires that same relay from inside
///      `receiveMessage` — the canonical `msg.value == 0` relay send.
contract PayNativeAccountingTest is CrossChainTest {
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant OUTBE_CHAIN_ID = 2;

    /// @dev Positive fee the loopback bridge charges; every send must fund this from `msg.value` or the float.
    uint256 internal constant BRIDGE_FEE = 0.001 ether;

    TargetRouter internal bnbRouter;
    OriginRouter internal outbeRouter;
    IntexNFT1155Bridge internal nftBridge;

    IntexNFT1155 internal intex;
    address internal admin = address(this);
    address internal auctionRole = address(0xA11C7);

    uint32 internal constant SERIES_ID = 20260501;
    uint256 internal constant TOKEN_ID = uint256(SERIES_ID);
    address internal holder = address(0xCAFE);

    function setUp() public {
        _setUpBridge();
        // A positive fee is what makes the funding branches observable: entry sends must be covered and refunded,
        // relay sends must draw a non-zero amount from the float.
        bridge.setFee(BRIDGE_FEE);

        intex = DeployProxy.intexNFT1155(admin, admin);

        bnbRouter = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        outbeRouter = DeployProxy.originRouter(address(bridge), admin, BNB_CHAIN_ID);
        nftBridge = DeployProxy.intexNFT1155Bridge(address(intex), address(bridge), admin);

        // Register remote messengers so `_send` has a destination and inbound delivery authenticates.
        bnbRouter.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(outbeRouter)));
        outbeRouter.setRemoteMessenger(BNB_CHAIN_ID, _interop(BNB_CHAIN_ID, address(bnbRouter)));
        nftBridge.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(nftBridge)));

        // Wire TM with a stub auction and the batch adapter.
        StubAuction stubAuction = new StubAuction();
        bnbRouter.wire(address(stubAuction), address(intex), admin, address(nftBridge));

        // Holders bridge: the router drives the bridge's systemMultiSend, which crosschainBurns on the local
        // Intex. `crosschainBurn` is `RELAYER_ROLE`-gated and additionally requires `SYSTEM_RELAYER_ROLE` once the
        // series is Called, so the adapter needs both roles on the token.
        nftBridge.grantRole(nftBridge.SYSTEM_RELAYER_ROLE(), address(bnbRouter));
        nftBridge.grantRole(nftBridge.SYSTEM_RELAYER_ROLE(), admin);
        intex.grantRole(intex.SYSTEM_RELAYER_ROLE(), address(nftBridge));
        intex.grantRole(intex.RELAYER_ROLE(), address(nftBridge));
        intex.grantRole(intex.RELAYER_ROLE(), address(bnbRouter));

        // Series + holder balance so markCalled/holder enumeration and the entry-path bridge sends have tokens.
        intex.createSeries(CreateSeriesLib.params(SERIES_ID, 10_000, 0));
        intex.markQualified(SERIES_ID);
        intex.mint(holder, 5, SERIES_ID);
    }

    /// @dev A 1-token bridge-out to Outbe; the entry path (payable `send`) burns it from `holder`.
    function _sendParam() internal view returns (SendParam memory) {
        return
            SendParam({dstChainId: OUTBE_CHAIN_ID, to: bytes32(uint256(uint160(holder))), tokenId: TOKEN_ID, amount: 1});
    }

    function _holderArrays() internal view returns (address[] memory holders, uint256[] memory amounts) {
        holders = new address[](1);
        holders[0] = holder;
        amounts = new uint256[](1);
        amounts[0] = 1;
    }

    // ---------------------------------------------------------------
    // Entry path — msg.value handling (IntexNFT1155Bridge.send)
    // ---------------------------------------------------------------

    function test_Entry_ExactFeeLeavesNoFloat() public {
        SendParam memory params = _sendParam();
        uint256 fee = nftBridge.quoteSend(params);
        assertEq(fee, BRIDGE_FEE, "fee mirrors the positive bridge fee");

        vm.deal(holder, fee);
        uint256 floatBefore = address(nftBridge).balance;

        vm.prank(holder);
        nftBridge.send{value: fee}(params);

        // `msg.value` flowed through to the bridge exactly; nothing seeded the relay float.
        assertEq(address(nftBridge).balance, floatBefore, "no leakage on exact-fee entry");
        assertEq(holder.balance, 0, "caller paid the full fee");
    }

    function test_Entry_ExcessIsRefundedToCaller() public {
        SendParam memory params = _sendParam();
        uint256 fee = nftBridge.quoteSend(params);

        uint256 buffer = 0.5 ether;
        vm.deal(holder, fee + buffer);
        uint256 floatBefore = address(nftBridge).balance;

        vm.prank(holder);
        nftBridge.send{value: fee + buffer}(params);

        // Excess refunded out of `_send`, not retained for future relay sends.
        assertEq(address(nftBridge).balance, floatBefore, "excess must not seed the relay float");
        assertEq(holder.balance, buffer, "caller refunded the excess");
    }

    function test_Entry_BelowFeeRevertsMsgValueBelowFee() public {
        SendParam memory params = _sendParam();
        uint256 fee = nftBridge.quoteSend(params);

        uint256 short = fee - 1;
        vm.deal(holder, fee);

        vm.prank(holder);
        vm.expectRevert(abi.encodeWithSelector(ERC7786MessengerBase.MsgValueBelowFee.selector, short, fee));
        nftBridge.send{value: short}(params);
    }

    /// @notice Pin the no-leakage invariant across an entry-followed-by-entry sequence: the second entry must not see
    ///         the first's `msg.value` accumulated as float.
    function test_Entry_DoesNotLeakIntoFloatAcrossSends() public {
        SendParam memory params = _sendParam();
        uint256 fee = nftBridge.quoteSend(params);

        uint256 buffer = 1 ether;
        vm.deal(holder, (fee + buffer) * 2);
        uint256 floatBefore = address(nftBridge).balance;

        vm.prank(holder);
        nftBridge.send{value: fee + buffer}(params);
        assertEq(address(nftBridge).balance, floatBefore, "first entry: no leakage");

        vm.prank(holder);
        nftBridge.send{value: fee + buffer}(params);
        assertEq(address(nftBridge).balance, floatBefore, "second entry: no leakage");
        assertEq(holder.balance, 2 * buffer, "both excess values refunded");
    }

    function test_Entry_RefundFailsRevertsRefundFailed() public {
        // `_send` refunds excess to msg.sender via `.call{value: refund}("")`; a caller whose receive() reverts trips
        // the RefundFailed guard. Without it, a refactor that swallowed the .call return would silently seed the
        // relay float with the entry caller's excess.
        NftRefundRejector rejector = new NftRefundRejector(address(nftBridge));
        intex.mint(address(rejector), 1, SERIES_ID);

        SendParam memory params = _sendParam();
        params.to = bytes32(uint256(uint160(address(rejector))));
        uint256 fee = nftBridge.quoteSend(params);
        uint256 buffer = 0.3 ether;
        vm.deal(address(rejector), fee + buffer);

        vm.expectRevert(ERC7786MessengerBase.RefundFailed.selector);
        rejector.callSend{value: fee + buffer}(params);
    }

    // ---------------------------------------------------------------
    // System bridge funding — TargetRouter forwards the quoted fee
    // ---------------------------------------------------------------

    function test_SystemMultiSend_UnderfundedReverts() public {
        (address[] memory holders, uint256[] memory amounts) = _holderArrays();
        // The caller must cover the fee; forwarding less than the quote reverts.
        vm.deal(address(this), BRIDGE_FEE);

        vm.expectRevert(
            abi.encodeWithSelector(ERC7786MessengerBase.MsgValueBelowFee.selector, BRIDGE_FEE - 1, BRIDGE_FEE)
        );
        nftBridge.systemMultiSend{value: BRIDGE_FEE - 1}(TOKEN_ID, holders, amounts, OUTBE_CHAIN_ID);
    }

    function test_SystemMultiSend_CallerFundedDrawsFee() public {
        (address[] memory holders, uint256[] memory amounts) = _holderArrays();

        vm.deal(address(this), BRIDGE_FEE);

        nftBridge.systemMultiSend{value: BRIDGE_FEE}(TOKEN_ID, holders, amounts, OUTBE_CHAIN_ID);

        // The caller's value covered the fee and the universal adapter kept nothing.
        assertEq(bridge.lastValue(), BRIDGE_FEE, "bridge received the forwarded fee");
        assertEq(address(nftBridge).balance, 0, "adapter holds no float");
    }

    // ---------------------------------------------------------------
    // Relay / float path — fired from inside receiveMessage (MARK_CALLED)
    // ---------------------------------------------------------------

    /// @dev The inbound MARK_CALLED handler fires the holders relay, funding it from TargetRouter's float. With
    ///      that float empty the forwarded-value call fails, which the handler catches and parks for later flush.
    function test_Relay_InsideReceiveMessage_EmptyFloatDefers() public {
        assertEq(address(bnbRouter).balance, 0, "router float unfunded");

        _deliverMarkCalled();

        (uint256 storedTokenId, bool exists, bool done) = bnbRouter.pendingHoldersRelays(0);
        assertEq(storedTokenId, TOKEN_ID, "holders relay deferred on float-starved NotEnoughNative");
        assertTrue(exists);
        assertFalse(done);
    }

    /// @dev With TargetRouter's float funded, the relay fired from inside `receiveMessage` forwards the fee and
    ///      sends cleanly — nothing is parked.
    function test_Relay_InsideReceiveMessage_FundedFloatSucceeds() public {
        vm.deal(address(bnbRouter), 1 ether); // TargetRouter pays the systemMultiSend fee
        uint256 floatBefore = address(bnbRouter).balance;

        _deliverMarkCalled();

        // No parked relay: TargetRouter funded the send.
        assertEq(bnbRouter.nextPendingHoldersRelayIdx(), 0, "no holders relay deferred");
        assertEq(address(bnbRouter).balance, floatBefore - BRIDGE_FEE, "fee drawn from router float");
        assertEq(address(nftBridge).balance, 0, "adapter holds no float");
    }

    function _deliverMarkCalled() internal {
        _deliver(OUTBE_CHAIN_ID, address(outbeRouter), address(bnbRouter), BridgeMsgCodec.encodeMarkCalled(SERIES_ID));
    }

    // ---------------------------------------------------------------
    // Admin float recovery (sweepNative)
    // ---------------------------------------------------------------

    function test_SweepNative_AdminRecoversFloat() public {
        vm.deal(address(bnbRouter), 3 ether);
        address payable to = payable(address(0x5EE3));

        bnbRouter.sweepNative(to, 1 ether);

        assertEq(to.balance, 1 ether, "recipient received the swept amount");
        assertEq(address(bnbRouter).balance, 2 ether, "remainder stays as float");
    }

    function test_SweepNative_NonAdminReverts() public {
        vm.deal(address(bnbRouter), 1 ether);
        vm.prank(auctionRole); // an arbitrary non-admin caller
        vm.expectRevert();
        bnbRouter.sweepNative(payable(auctionRole), 1 ether);
    }

    function test_SweepNative_OverBalanceReverts() public {
        vm.deal(address(bnbRouter), 1 ether);
        vm.expectRevert(
            abi.encodeWithSelector(ITargetRouter.NativeBalanceInsufficient.selector, uint256(1 ether), uint256(2 ether))
        );
        bnbRouter.sweepNative(payable(address(0xBEEF)), 2 ether);
    }

    function test_SweepNative_ZeroRecipientReverts() public {
        vm.deal(address(bnbRouter), 1 ether);
        vm.expectRevert(abi.encodeWithSelector(ITargetRouter.ZeroAddress.selector, "to"));
        bnbRouter.sweepNative(payable(address(0)), 1 ether);
    }
}

/// @dev Placeholder auction so `TargetRouter.wire` accepts a non-zero auction address. The delivered MARK_CALLED
///      (which only touches `intex`) never calls the auction, so no interface surface is needed.
// solhint-disable-next-line no-empty-blocks
contract StubAuction {}

/// @dev Holds a bridgeable token (accepts the ERC-1155 mint) but whose `receive()` reverts; used to pin `_send`'s
///      RefundFailed guard on the entry path via the NFT bridge.
contract NftRefundRejector {
    IntexNFT1155Bridge private immutable bridge;

    constructor(address _bridge) {
        bridge = IntexNFT1155Bridge(payable(_bridge));
    }

    function callSend(SendParam calldata params) external payable {
        bridge.send{value: msg.value}(params);
    }

    function onERC1155Received(address, address, uint256, uint256, bytes calldata) external pure returns (bytes4) {
        return this.onERC1155Received.selector;
    }

    receive() external payable {
        revert("refund-rejected");
    }
}
