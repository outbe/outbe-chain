// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {TargetMessenger} from "@contracts/target/TargetMessenger.sol";
import {ITargetMessenger} from "@contracts/target/interfaces/ITargetMessenger.sol";
import {OriginMessenger} from "@contracts/origin/OriginMessenger.sol";
import {IntexNFT1155Bridge} from "@contracts/shared/IntexNFT1155Bridge.sol";
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
/// @dev Entry path is driven through `TargetMessenger.sendBidsBatch` (payable, `AUCTION_ROLE`). The relay/float path
///      is driven both directly (`IntexNFT1155Bridge.systemMultiSend`, not payable → `msg.value == 0`) and through
///      an inbound MARK_CALLED delivery whose `_handleMarkCalled` handler fires that same relay from inside
///      `receiveMessage` — the canonical `msg.value == 0` relay send.
contract PayNativeAccountingTest is CrossChainTest {
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant OUTBE_CHAIN_ID = 2;

    /// @dev Positive fee the loopback bridge charges; every send must fund this from `msg.value` or the float.
    uint256 internal constant BRIDGE_FEE = 0.001 ether;

    TargetMessenger internal bnbMessenger;
    OriginMessenger internal outbeMessenger;
    IntexNFT1155Bridge internal onftBatch;

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

        bnbMessenger = DeployProxy.targetMessenger(address(bridge), admin, OUTBE_CHAIN_ID);
        outbeMessenger = DeployProxy.originMessenger(address(bridge), admin, BNB_CHAIN_ID);
        onftBatch = DeployProxy.intexNFT1155Bridge(address(intex), address(bridge), admin);

        // Register remote messengers so `_send` has a destination and inbound delivery authenticates.
        bnbMessenger.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(outbeMessenger)));
        outbeMessenger.setRemoteMessenger(BNB_CHAIN_ID, _interop(BNB_CHAIN_ID, address(bnbMessenger)));
        onftBatch.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(onftBatch)));

        // Wire TM with a stub auction and the batch adapter; `auctionRole` gets AUCTION_ROLE so it can call
        // `sendBidsBatch` directly (that role is normally held by the auction contract).
        StubAuction stubAuction = new StubAuction();
        bnbMessenger.wire(address(stubAuction), address(intex), admin, address(onftBatch));
        bnbMessenger.grantRole(bnbMessenger.AUCTION_ROLE(), auctionRole);

        // Holders bridge: the messenger drives the adapter's systemMultiSend, which crosschainBurns on the local
        // Intex. `crosschainBurn` is `RELAYER_ROLE`-gated and additionally requires `SYSTEM_RELAYER_ROLE` once the
        // series is Called, so the adapter needs both roles on the token.
        onftBatch.grantRole(onftBatch.SYSTEM_RELAYER_ROLE(), address(bnbMessenger));
        onftBatch.grantRole(onftBatch.SYSTEM_RELAYER_ROLE(), admin);
        intex.grantRole(intex.SYSTEM_RELAYER_ROLE(), address(onftBatch));
        intex.grantRole(intex.RELAYER_ROLE(), address(onftBatch));
        intex.grantRole(intex.RELAYER_ROLE(), address(bnbMessenger));

        // Series + one holder so markCalled + holder enumeration produce a non-empty relay.
        intex.createSeries(CreateSeriesLib.params(SERIES_ID, 10_000, 0));
        intex.markQualified(SERIES_ID);
        intex.mint(holder, 1, SERIES_ID);
    }

    function _bidsParams() internal view returns (ITargetMessenger.BidsBatchParams memory params) {
        address[] memory bidders = new address[](1);
        bidders[0] = address(0xB1D);
        uint16[] memory qty = new uint16[](1);
        qty[0] = 1;
        uint32[] memory rate = new uint32[](1);
        rate[0] = 100e6;
        uint32[] memory ts = new uint32[](1);
        ts[0] = uint32(block.timestamp);

        params = ITargetMessenger.BidsBatchParams({
            seriesId: SERIES_ID, bidderAddresses: bidders, intexQuantities: qty, intexBidRates: rate, timestamps: ts
        });
    }

    function _holderArrays() internal view returns (address[] memory holders, uint256[] memory amounts) {
        holders = new address[](1);
        holders[0] = holder;
        amounts = new uint256[](1);
        amounts[0] = 1;
    }

    // ---------------------------------------------------------------
    // Entry path — msg.value handling (sendBidsBatch)
    // ---------------------------------------------------------------

    function test_Entry_ExactFeeLeavesNoFloat() public {
        ITargetMessenger.BidsBatchParams memory params = _bidsParams();
        uint256 fee = bnbMessenger.quoteSendBidsBatch(params);
        assertEq(fee, BRIDGE_FEE, "fee mirrors the positive bridge fee");

        vm.deal(auctionRole, fee);
        uint256 floatBefore = address(bnbMessenger).balance;

        vm.prank(auctionRole);
        bnbMessenger.sendBidsBatch{value: fee}(params);

        // `msg.value` flowed through to the bridge exactly; nothing seeded the relay float.
        assertEq(address(bnbMessenger).balance, floatBefore, "no leakage on exact-fee entry");
        assertEq(auctionRole.balance, 0, "caller paid the full fee");
    }

    function test_Entry_ExcessIsRefundedToCaller() public {
        ITargetMessenger.BidsBatchParams memory params = _bidsParams();
        uint256 fee = bnbMessenger.quoteSendBidsBatch(params);

        uint256 buffer = 0.5 ether;
        vm.deal(auctionRole, fee + buffer);
        uint256 floatBefore = address(bnbMessenger).balance;

        vm.prank(auctionRole);
        bnbMessenger.sendBidsBatch{value: fee + buffer}(params);

        // Excess refunded out of `_send`, not retained for future relay sends.
        assertEq(address(bnbMessenger).balance, floatBefore, "excess must not seed the relay float");
        assertEq(auctionRole.balance, buffer, "caller refunded the excess");
    }

    function test_Entry_BelowFeeRevertsMsgValueBelowFee() public {
        ITargetMessenger.BidsBatchParams memory params = _bidsParams();
        uint256 fee = bnbMessenger.quoteSendBidsBatch(params);

        uint256 short = fee - 1;
        vm.deal(auctionRole, fee);

        vm.prank(auctionRole);
        vm.expectRevert(abi.encodeWithSelector(ERC7786MessengerBase.MsgValueBelowFee.selector, short, fee));
        bnbMessenger.sendBidsBatch{value: short}(params);
    }

    /// @notice Pin the no-leakage invariant across an entry-followed-by-entry sequence: the second entry must not see
    ///         the first's `msg.value` accumulated as float.
    function test_Entry_DoesNotLeakIntoFloatAcrossSends() public {
        ITargetMessenger.BidsBatchParams memory params = _bidsParams();
        uint256 fee = bnbMessenger.quoteSendBidsBatch(params);

        uint256 buffer = 1 ether;
        vm.deal(auctionRole, (fee + buffer) * 2);
        uint256 floatBefore = address(bnbMessenger).balance;

        vm.prank(auctionRole);
        bnbMessenger.sendBidsBatch{value: fee + buffer}(params);
        assertEq(address(bnbMessenger).balance, floatBefore, "first entry: no leakage");

        vm.prank(auctionRole);
        bnbMessenger.sendBidsBatch{value: fee + buffer}(params);
        assertEq(address(bnbMessenger).balance, floatBefore, "second entry: no leakage");
        assertEq(auctionRole.balance, 2 * buffer, "both excess values refunded");
    }

    function test_Entry_RefundFailsRevertsRefundFailed() public {
        // `_send` refunds excess to msg.sender via `.call{value: refund}("")`; a caller whose receive() reverts trips
        // the RefundFailed guard. Without it, a refactor that swallowed the .call return would silently seed the
        // relay float with the entry caller's excess.
        BidsRefundRejector rejector = new BidsRefundRejector(address(bnbMessenger));
        bnbMessenger.grantRole(bnbMessenger.AUCTION_ROLE(), address(rejector));

        ITargetMessenger.BidsBatchParams memory params = _bidsParams();
        uint256 fee = bnbMessenger.quoteSendBidsBatch(params);
        uint256 buffer = 0.3 ether;
        vm.deal(address(rejector), fee + buffer);

        vm.expectRevert(ERC7786MessengerBase.RefundFailed.selector);
        rejector.callSendBidsBatch{value: fee + buffer}(params);
    }

    // ---------------------------------------------------------------
    // System bridge funding — TargetMessenger forwards the quoted fee
    // ---------------------------------------------------------------

    function test_SystemMultiSend_UnderfundedReverts() public {
        (address[] memory holders, uint256[] memory amounts) = _holderArrays();
        // The caller must cover the fee; forwarding less than the quote reverts.
        vm.deal(address(this), BRIDGE_FEE);

        vm.expectRevert(
            abi.encodeWithSelector(ERC7786MessengerBase.MsgValueBelowFee.selector, BRIDGE_FEE - 1, BRIDGE_FEE)
        );
        onftBatch.systemMultiSend{value: BRIDGE_FEE - 1}(TOKEN_ID, holders, amounts, OUTBE_CHAIN_ID);
    }

    function test_SystemMultiSend_CallerFundedDrawsFee() public {
        (address[] memory holders, uint256[] memory amounts) = _holderArrays();

        vm.deal(address(this), BRIDGE_FEE);

        onftBatch.systemMultiSend{value: BRIDGE_FEE}(TOKEN_ID, holders, amounts, OUTBE_CHAIN_ID);

        // The caller's value covered the fee and the universal adapter kept nothing.
        assertEq(bridge.lastValue(), BRIDGE_FEE, "bridge received the forwarded fee");
        assertEq(address(onftBatch).balance, 0, "adapter holds no float");
    }

    // ---------------------------------------------------------------
    // Relay / float path — fired from inside receiveMessage (MARK_CALLED)
    // ---------------------------------------------------------------

    /// @dev The inbound MARK_CALLED handler fires the holders relay, funding it from TargetMessenger's float. With
    ///      that float empty the forwarded-value call fails, which the handler catches and parks for later flush.
    function test_Relay_InsideReceiveMessage_EmptyFloatDefers() public {
        assertEq(address(bnbMessenger).balance, 0, "messenger float unfunded");

        _deliverMarkCalled();

        (uint256 storedTokenId, bool exists, bool done) = bnbMessenger.pendingHoldersRelays(0);
        assertEq(storedTokenId, TOKEN_ID, "holders relay deferred on float-starved NotEnoughNative");
        assertTrue(exists);
        assertFalse(done);
    }

    /// @dev With TargetMessenger's float funded, the relay fired from inside `receiveMessage` forwards the fee and
    ///      sends cleanly — nothing is parked.
    function test_Relay_InsideReceiveMessage_FundedFloatSucceeds() public {
        vm.deal(address(bnbMessenger), 1 ether); // TargetMessenger pays the systemMultiSend fee
        uint256 floatBefore = address(bnbMessenger).balance;

        _deliverMarkCalled();

        // No parked relay: TargetMessenger funded the send.
        assertEq(bnbMessenger.nextPendingHoldersRelayIdx(), 0, "no holders relay deferred");
        assertEq(address(bnbMessenger).balance, floatBefore - BRIDGE_FEE, "fee drawn from messenger float");
        assertEq(address(onftBatch).balance, 0, "adapter holds no float");
    }

    function _deliverMarkCalled() internal {
        _deliver(
            OUTBE_CHAIN_ID, address(outbeMessenger), address(bnbMessenger), BridgeMsgCodec.encodeMarkCalled(SERIES_ID)
        );
    }

    // ---------------------------------------------------------------
    // Admin float recovery (sweepNative)
    // ---------------------------------------------------------------

    function test_SweepNative_AdminRecoversFloat() public {
        vm.deal(address(bnbMessenger), 3 ether);
        address payable to = payable(address(0x5EE3));

        bnbMessenger.sweepNative(to, 1 ether);

        assertEq(to.balance, 1 ether, "recipient received the swept amount");
        assertEq(address(bnbMessenger).balance, 2 ether, "remainder stays as float");
    }

    function test_SweepNative_NonAdminReverts() public {
        vm.deal(address(bnbMessenger), 1 ether);
        vm.prank(auctionRole); // AUCTION_ROLE, not DEFAULT_ADMIN_ROLE
        vm.expectRevert();
        bnbMessenger.sweepNative(payable(auctionRole), 1 ether);
    }

    function test_SweepNative_OverBalanceReverts() public {
        vm.deal(address(bnbMessenger), 1 ether);
        vm.expectRevert(
            abi.encodeWithSelector(
                ITargetMessenger.NativeBalanceInsufficient.selector, uint256(1 ether), uint256(2 ether)
            )
        );
        bnbMessenger.sweepNative(payable(address(0xBEEF)), 2 ether);
    }

    function test_SweepNative_ZeroRecipientReverts() public {
        vm.deal(address(bnbMessenger), 1 ether);
        vm.expectRevert(abi.encodeWithSelector(ITargetMessenger.ZeroAddress.selector, "to"));
        bnbMessenger.sweepNative(payable(address(0)), 1 ether);
    }
}

/// @dev Placeholder auction so `TargetMessenger.wire` accepts a non-zero auction address. Neither the entry path
///      (`sendBidsBatch` encodes its params directly) nor the delivered MARK_CALLED (which only touches `intex`)
///      calls the auction, so no interface surface is needed.
// solhint-disable-next-line no-empty-blocks
contract StubAuction {}

/// @dev Helper whose `receive()` reverts; used to pin `_send`'s RefundFailed guard on the entry path.
contract BidsRefundRejector {
    TargetMessenger private immutable messenger;

    constructor(address _messenger) {
        messenger = TargetMessenger(payable(_messenger));
    }

    function callSendBidsBatch(ITargetMessenger.BidsBatchParams calldata params) external payable {
        messenger.sendBidsBatch{value: msg.value}(params);
    }

    receive() external payable {
        revert("refund-rejected");
    }
}
