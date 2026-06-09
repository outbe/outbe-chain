// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {MessagingFee, Origin} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";
import {OptionsBuilder} from "@layerzerolabs/oapp-evm/oapp/libs/OptionsBuilder.sol";
import {EnforcedOptionParam} from "@layerzerolabs/oapp-evm/oapp/interfaces/IOAppOptionsType3.sol";

import {TargetMessenger} from "@contracts/bnb/TargetMessenger.sol";
import {ITargetMessenger} from "@contracts/bnb/interfaces/ITargetMessenger.sol";
import {OriginMessenger} from "@contracts/outbe/OriginMessenger.sol";
import {IOriginMessenger} from "@contracts/outbe/interfaces/IOriginMessenger.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";

/// @title PayNativeAccountingTest
/// @notice Behavioural coverage for `_payNative` on both messengers ( for `TargetMessenger`,
/// for `OriginMessenger`): each distinguishes entry-funded (`msg.value > 0`) and
///         relay-funded (`msg.value == 0`) callers. Entry callers' excess is refunded back to them;
///         relay sends draw from the pre-funded native float. Conflating the two would let an entry
///         caller's `msg.value` silently seed future relay sends without refund, or let an entry
///         caller drain the relay float. Also covers `OriginMessenger.sweepNative` float recovery.
/// @dev TM relay-path coverage lives in `PatternADefer.t.sol::test_TM_FlushBidsRelaySucceedsAfterTopUp`
///      — that test exercises `_payNative` with `msg.value == 0` and pre-funded balance > 0.
contract PayNativeAccountingTest is TestHelperOz5 {
    using OptionsBuilder for bytes;

    uint32 internal constant BNB_EID = 1;
    uint32 internal constant OUTBE_EID = 2;

    TargetMessenger internal bnbMessenger;
    OriginMessenger internal outbeMessenger;
    ONFT1155AdapterBatch internal onftBatchBnb;

    IntexNFT1155 internal intex;
    address internal admin = address(this);
    address internal auctionRole = address(0xA11C7);
    address internal desis;
    address internal omRelayer = address(0x0021E1);

    uint32 internal constant SERIES_ID = 20260501;

    function setUp() public override {
        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        desis = address(new MockDesis());
        intex = new IntexNFT1155(admin, admin);

        bnbMessenger = TargetMessenger(
            payable(_deployOApp(
                    type(TargetMessenger).creationCode, abi.encode(address(endpoints[BNB_EID]), admin, OUTBE_EID)
                ))
        );
        outbeMessenger = OriginMessenger(
            payable(_deployOApp(
                    type(OriginMessenger).creationCode, abi.encode(address(endpoints[OUTBE_EID]), admin, BNB_EID)
                ))
        );
        onftBatchBnb = new ONFT1155AdapterBatch(address(intex), address(endpoints[BNB_EID]), admin);

        address[] memory bridge = new address[](2);
        bridge[0] = address(bnbMessenger);
        bridge[1] = address(outbeMessenger);
        this.wireOApps(bridge);

        // Wire TM with stand-in dependencies; `auctionRole` is granted AUCTION_ROLE so it can
        // call `sendBidsBatch` directly (the role is normally granted to the auction contract).
        bnbMessenger.wire(auctionRole, address(intex), admin, address(onftBatchBnb));
        outbeMessenger.wire(desis, makeAddr("factory"));

        // Configure enforcedOptions so `combineOptions` returns a valid LZ options blob and the
        // ULN doesn't reject the outbound send.
        bytes memory bidsOptions = OptionsBuilder.newOptions().addExecutorLzReceiveOption(200000, 0);
        EnforcedOptionParam[] memory params = new EnforcedOptionParam[](1);
        params[0] = EnforcedOptionParam({eid: OUTBE_EID, msgType: BridgeMsgCodec.MSG_BIDS_BATCH, options: bidsOptions});
        bnbMessenger.setEnforcedOptions(params);

        // OriginMessenger side: a clearing-stage send needs valid options and a
        // DESIS_ROLE caller. `omRelayer` stands in for the chain-native module / operator.
        EnforcedOptionParam[] memory omParams = new EnforcedOptionParam[](1);
        omParams[0] = EnforcedOptionParam({
            eid: BNB_EID, msgType: BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING, options: bidsOptions
        });
        outbeMessenger.setEnforcedOptions(omParams);
        outbeMessenger.grantRole(outbeMessenger.DESIS_ROLE(), omRelayer);
    }

    function _bidsParams() internal view returns (ITargetMessenger.BidsBatchParams memory params) {
        address[] memory bidders = new address[](1);
        bidders[0] = address(0xCAFE);
        uint16[] memory qty = new uint16[](1);
        qty[0] = 1;
        uint64[] memory price = new uint64[](1);
        price[0] = 100e6;
        uint32[] memory ts = new uint32[](1);
        ts[0] = uint32(block.timestamp);

        params = ITargetMessenger.BidsBatchParams({
            seriesId: SERIES_ID,
            bidderAddresses: bidders,
            intexQuantities: qty,
            intexBidPrices: price,
            timestamps: ts,
            extraOptions: "",
            refundAddress: address(0)
        });
    }

    // ---------------------------------------------------------------
    // Entry path — msg.value handling
    // ---------------------------------------------------------------

    function test_PayNative_EntryExactMatchSucceeds() public {
        ITargetMessenger.BidsBatchParams memory params = _bidsParams();
        MessagingFee memory fee = bnbMessenger.quoteSendBidsBatch(params, false);

        vm.deal(auctionRole, fee.nativeFee);
        uint256 contractBefore = address(bnbMessenger).balance;

        vm.prank(auctionRole);
        bnbMessenger.sendBidsBatch{value: fee.nativeFee}(params, fee);

        // Contract balance unchanged — `msg.value` flowed through to the endpoint exactly,
        // no excess seeded the relay budget.
        assertEq(address(bnbMessenger).balance, contractBefore, "no leakage on exact-match entry");
        assertEq(auctionRole.balance, 0, "caller paid the full fee");
    }

    function test_PayNative_EntryExcessIsRefundedToCaller() public {
        ITargetMessenger.BidsBatchParams memory params = _bidsParams();
        MessagingFee memory fee = bnbMessenger.quoteSendBidsBatch(params, false);

        uint256 buffer = 0.5 ether;
        vm.deal(auctionRole, fee.nativeFee + buffer);
        uint256 contractBefore = address(bnbMessenger).balance;

        vm.prank(auctionRole);
        bnbMessenger.sendBidsBatch{value: fee.nativeFee + buffer}(params, fee);

        // Contract balance unchanged — excess refunded out of `_payNative`, not retained for
        // future relay sends.
        assertEq(address(bnbMessenger).balance, contractBefore, "excess must not seed relay budget");
        assertEq(auctionRole.balance, buffer, "caller refunded the excess");
    }

    function test_PayNative_EntryInsufficientRevertsMsgValueBelowFee() public {
        ITargetMessenger.BidsBatchParams memory params = _bidsParams();
        MessagingFee memory fee = bnbMessenger.quoteSendBidsBatch(params, false);

        uint256 short = fee.nativeFee - 1;
        vm.deal(auctionRole, fee.nativeFee);

        vm.prank(auctionRole);
        vm.expectRevert(abi.encodeWithSelector(ITargetMessenger.MsgValueBelowFee.selector, short, fee.nativeFee));
        bnbMessenger.sendBidsBatch{value: short}(params, fee);
    }

    /// @notice Pin the no-leakage invariant across an entry-followed-by-entry sequence: the
    ///         second entry must not see the first's `msg.value` accumulated as balance.
    function test_PayNative_EntryDoesNotLeakIntoRelayBudget() public {
        ITargetMessenger.BidsBatchParams memory params = _bidsParams();
        MessagingFee memory fee = bnbMessenger.quoteSendBidsBatch(params, false);

        uint256 buffer = 1 ether;
        vm.deal(auctionRole, (fee.nativeFee + buffer) * 2);
        uint256 contractBefore = address(bnbMessenger).balance;

        vm.prank(auctionRole);
        bnbMessenger.sendBidsBatch{value: fee.nativeFee + buffer}(params, fee);

        // After the first send, contract balance must be unchanged (excess refunded).
        assertEq(address(bnbMessenger).balance, contractBefore, "first entry: no leakage");

        vm.prank(auctionRole);
        bnbMessenger.sendBidsBatch{value: fee.nativeFee + buffer}(params, fee);

        // After the second send, still unchanged.
        assertEq(address(bnbMessenger).balance, contractBefore, "second entry: no leakage");
        // Caller has both excess refunds.
        assertEq(auctionRole.balance, 2 * buffer, "both excess values refunded");
    }

    // ---------------------------------------------------------------
    // OriginMessenger — relay-funded _payNative + float
    // ---------------------------------------------------------------

    function _omClearingFee() internal view returns (MessagingFee memory) {
        return outbeMessenger.quoteSendAuctionStageClearing(SERIES_ID, "", false);
    }

    function test_OM_PayNative_RelayFundedDrawsFromFloat() public {
        MessagingFee memory fee = _omClearingFee();
        vm.deal(address(outbeMessenger), fee.nativeFee + 1 ether); // pre-funded relay float
        uint256 floatBefore = address(outbeMessenger).balance;

        vm.prank(omRelayer);
        outbeMessenger.sendAuctionStageClearing{value: 0}(SERIES_ID, "", fee, omRelayer);

        assertEq(address(outbeMessenger).balance, floatBefore - fee.nativeFee, "relay fee drawn from float");
    }

    function test_OM_PayNative_EntryExcessRefundedNoFloatLeak() public {
        MessagingFee memory fee = _omClearingFee();
        uint256 buffer = 0.5 ether;
        vm.deal(omRelayer, fee.nativeFee + buffer);
        uint256 floatBefore = address(outbeMessenger).balance; // 0

        vm.prank(omRelayer);
        outbeMessenger.sendAuctionStageClearing{value: fee.nativeFee + buffer}(SERIES_ID, "", fee, omRelayer);

        assertEq(address(outbeMessenger).balance, floatBefore, "entry buffer must not seed the float");
        assertEq(omRelayer.balance, buffer, "buffer refunded to the caller");
    }

    function test_OM_PayNative_EntryInsufficientRevertsMsgValueBelowFee() public {
        MessagingFee memory fee = _omClearingFee();
        uint256 short = fee.nativeFee - 1;
        vm.deal(omRelayer, fee.nativeFee);

        vm.prank(omRelayer);
        vm.expectRevert(abi.encodeWithSelector(IOriginMessenger.MsgValueBelowFee.selector, short, fee.nativeFee));
        outbeMessenger.sendAuctionStageClearing{value: short}(SERIES_ID, "", fee, omRelayer);
    }

    function test_OM_PayNative_RelayInsufficientFloatReverts() public {
        MessagingFee memory fee = _omClearingFee();
        vm.deal(address(outbeMessenger), fee.nativeFee - 1); // float below the fee

        vm.prank(omRelayer);
        vm.expectRevert(); // OAppSender.NotEnoughNative(balance) — relay path, msg.value == 0
        outbeMessenger.sendAuctionStageClearing{value: 0}(SERIES_ID, "", fee, omRelayer);
    }

    function test_OM_SweepNative_AdminRecoversFloat() public {
        vm.deal(address(outbeMessenger), 3 ether);
        address payable to = payable(address(0x5EE3));

        outbeMessenger.sweepNative(to, 1 ether);

        assertEq(to.balance, 1 ether, "recipient received the swept amount");
        assertEq(address(outbeMessenger).balance, 2 ether, "remainder stays as float");
    }

    function test_OM_SweepNative_NonAdminReverts() public {
        vm.deal(address(outbeMessenger), 1 ether);
        vm.prank(omRelayer); // DESIS_ROLE, not DEFAULT_ADMIN_ROLE
        vm.expectRevert();
        outbeMessenger.sweepNative(payable(omRelayer), 1 ether);
    }

    function test_OM_SweepNative_OverBalanceReverts() public {
        vm.deal(address(outbeMessenger), 1 ether);
        vm.expectRevert(abi.encodeWithSelector(IOriginMessenger.NativeBalanceInsufficient.selector, 1 ether, 2 ether));
        outbeMessenger.sweepNative(payable(address(0xBEEF)), 2 ether);
    }

    function test_OM_SweepNative_ZeroRecipientReverts() public {
        vm.deal(address(outbeMessenger), 1 ether);
        vm.expectRevert(abi.encodeWithSelector(IOriginMessenger.ZeroAddress.selector, "to"));
        outbeMessenger.sweepNative(payable(address(0)), 1 ether);
    }

    function test_PayNative_EntryRefundFails_RevertsRefundFailed() public {
        // Parallel to the OM RefundFailed pin: TM's _payNative refunds excess to msg.sender via
        // `.call{value: refund}("")`. A caller whose receive() reverts trips the same guard.
        BidsRefundRejector rejector = new BidsRefundRejector(address(bnbMessenger));
        bnbMessenger.grantRole(bnbMessenger.AUCTION_ROLE(), address(rejector));

        ITargetMessenger.BidsBatchParams memory params = _bidsParams();
        MessagingFee memory fee = bnbMessenger.quoteSendBidsBatch(params, false);
        uint256 buffer = 0.3 ether;
        vm.deal(address(rejector), fee.nativeFee + buffer);

        vm.expectRevert(ITargetMessenger.RefundFailed.selector);
        rejector.callSendBidsBatch{value: fee.nativeFee + buffer}(params, fee);
    }

    function test_OM_PayNative_EntryRefundFails_RevertsRefundFailed() public {
        // _payNative refunds excess to msg.sender via `.call{value: refund}("")`. If the caller
        // is a contract whose receive() reverts, the refund leg fails and _payNative reverts.
        // Without this pin, a future refactor that swallows the .call return value would seed
        // the relay float with the entry caller's excess instead of returning RefundFailed.
        RefundRejectingCaller rejector = new RefundRejectingCaller(address(outbeMessenger));
        outbeMessenger.grantRole(outbeMessenger.DESIS_ROLE(), address(rejector));

        MessagingFee memory fee = _omClearingFee();
        uint256 buffer = 0.3 ether;
        vm.deal(address(rejector), fee.nativeFee + buffer);

        vm.expectRevert(IOriginMessenger.RefundFailed.selector);
        rejector.callSendAuctionStageClearing{value: fee.nativeFee + buffer}(SERIES_ID, "", fee);
    }
}

/// @dev Helper whose `receive()` reverts; used to pin TM `_payNative.RefundFailed`.
contract BidsRefundRejector {
    TargetMessenger private immutable messenger;

    constructor(address _messenger) {
        messenger = TargetMessenger(payable(_messenger));
    }

    function callSendBidsBatch(
        ITargetMessenger.BidsBatchParams calldata params,
        MessagingFee calldata fee
    ) external payable {
        messenger.sendBidsBatch{value: msg.value}(params, fee);
    }

    receive() external payable {
        revert("refund-rejected");
    }
}

/// @dev Helper contract whose `receive()` reverts so refunds from `_payNative` fail.
contract RefundRejectingCaller {
    OriginMessenger private immutable messenger;

    constructor(address _messenger) {
        messenger = OriginMessenger(payable(_messenger));
    }

    function callSendAuctionStageClearing(
        uint32 seriesId,
        bytes calldata options,
        MessagingFee calldata fee
    ) external payable {
        messenger.sendAuctionStageClearing{value: msg.value}(seriesId, options, fee, address(this));
    }

    receive() external payable {
        revert("refund-rejected");
    }
}
