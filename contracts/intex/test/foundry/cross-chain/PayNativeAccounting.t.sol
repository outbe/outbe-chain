// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IIntexAuctionTargetRouter} from "@contracts/target/interfaces/IIntexAuctionTargetRouter.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {SendParam} from "@contracts/shared/interfaces/IIntexNFT1155Router.sol";
import {ERC7786Sender} from "@contracts/shared/ERC7786Sender.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {OpenSeriesLib} from "../helpers/OpenSeriesLib.sol";

/// @title PayNativeAccountingTest
/// @notice Behavioural coverage for the native-fee funding logic that
///         {ERC7786Sender-_sendTo} owns for every intex bridge client. The logic
///         distinguishes two calling conventions:
///           * entry-funded (`msg.value > 0`): the send must cover the quoted fee and refund the
///             excess to the caller. Thus an entry caller's buffer never silently seeds (or
///             drains) the contract's relay float.
///           * relay-funded (`msg.value == 0`): a chain-native module that cannot attach value
///             triggered the send. Thus the send draws the fee from the contract's pre-funded
///             native float and reverts `NotEnoughNative` when short.
///         Conflating the two would let an entry caller's `msg.value` seed future relay sends
///         without refund, or let an entry caller drain the relay float.
/// @dev The tests drive the entry path through the user-facing `IntexNFT1155Router.send` (payable).
///      They drive the relay path by an inbound CLEARING whose handler relays the day's bids from
///      inside `receiveFromOutbe`.
contract PayNativeAccountingTest is CrossChainTest {
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant OUTBE_CHAIN_ID = 2;

    /// @dev Positive fee the loopback bridge charges. Every send must fund this from
    ///      `msg.value` or the float.
    uint256 internal constant BRIDGE_FEE = 0.001 ether;

    IntexAuctionTargetRouter internal bnbRouter;
    IntexNFT1155Router internal nftRouter;

    IntexNFT1155 internal intex;
    address internal admin = address(this);
    address internal auctionRole = address(0xA11C7);
    address internal outbeIca = makeAddr("outbeIca");

    uint32 internal constant SERIES_ID_DAY = 20260501;
    bytes14 internal constant SERIES_ID = "20260501-USD-U";
    uint256 internal constant TOKEN_ID = uint256(keccak256(abi.encodePacked(SERIES_ID)));
    address internal owner = address(0xCAFE);

    function setUp() public {
        _setUpBridge();
        // A positive fee is what makes the funding branches observable. Entry sends must be
        // covered and refunded. Relay sends must draw a non-zero amount from the float.
        bridge.setFee(BRIDGE_FEE);

        intex = DeployProxy.intexNFT1155(admin, admin);

        bnbRouter = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        nftRouter = DeployProxy.intexNFT1155Router(address(intex), address(bridge), admin);

        // Wire the inbound source of TM and the remote router the batch adapter sends to.
        _wireOutbeIca(bnbRouter, outbeIca);
        nftRouter.setRemoteRouter(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(nftRouter)));

        // Wire TM with a stub auction and the batch adapter.
        StubAuction stubAuction = new StubAuction();
        bnbRouter.setDependencies(address(stubAuction), address(intex), admin);

        // `crosschainBurn` is `BRIDGE_ROLE`-gated, so the adapter needs it on the token.
        intex.grantRole(intex.BRIDGE_ROLE(), address(nftRouter));
        intex.grantRole(intex.ROUTER_ROLE(), address(bnbRouter));

        // Series + owner balance so callSeries/owner enumeration and the entry-path bridge sends have tokens.
        intex.openSeries(OpenSeriesLib.params(SERIES_ID_DAY, 10_000, 0));
        intex.issueIntex(owner, 5, SERIES_ID);
    }

    /// @dev A 1-token bridge-out to Outbe. The entry path (payable `send`) burns it from `owner`.
    function _sendParam() internal view returns (SendParam memory) {
        return
            SendParam({dstChainId: OUTBE_CHAIN_ID, to: bytes32(uint256(uint160(owner))), intexId: TOKEN_ID, units: 1});
    }

    function _holderArrays() internal view returns (address[] memory owners, uint256[] memory amounts) {
        owners = new address[](1);
        owners[0] = owner;
        amounts = new uint256[](1);
        amounts[0] = 1;
    }

    // ---------------------------------------------------------------
    // Entry path - msg.value handling (IntexNFT1155Router.send)
    // ---------------------------------------------------------------

    function test_Entry_ExactFeeLeavesNoFloat() public {
        SendParam memory params = _sendParam();
        uint256 fee = nftRouter.quoteSend(params);
        assertEq(fee, BRIDGE_FEE, "fee mirrors the positive bridge fee");

        vm.deal(owner, fee);
        uint256 floatBefore = address(nftRouter).balance;

        vm.prank(owner);
        nftRouter.send{value: fee}(params);

        // `msg.value` flowed through to the bridge exactly. Nothing seeded the relay float.
        assertEq(address(nftRouter).balance, floatBefore, "no leakage on exact-fee entry");
        assertEq(owner.balance, 0, "caller paid the full fee");
    }

    function test_Entry_ExcessIsRefundedToCaller() public {
        SendParam memory params = _sendParam();
        uint256 fee = nftRouter.quoteSend(params);

        uint256 buffer = 0.5 ether;
        vm.deal(owner, fee + buffer);
        uint256 floatBefore = address(nftRouter).balance;

        vm.prank(owner);
        nftRouter.send{value: fee + buffer}(params);

        // Excess refunded out of `_send`, not retained for future relay sends.
        assertEq(address(nftRouter).balance, floatBefore, "excess must not seed the relay float");
        assertEq(owner.balance, buffer, "caller refunded the excess");
    }

    function test_Entry_BelowFeeRevertsMsgValueBelowFee() public {
        SendParam memory params = _sendParam();
        uint256 fee = nftRouter.quoteSend(params);

        uint256 short = fee - 1;
        vm.deal(owner, fee);

        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSelector(ERC7786Sender.MsgValueBelowFee.selector, short, fee));
        nftRouter.send{value: short}(params);
    }

    /// @notice Pin the no-leakage invariant across an entry-followed-by-entry sequence: the second entry must not see
    ///         the first's `msg.value` accumulated as float.
    function test_Entry_DoesNotLeakIntoFloatAcrossSends() public {
        SendParam memory params = _sendParam();
        uint256 fee = nftRouter.quoteSend(params);

        uint256 buffer = 1 ether;
        vm.deal(owner, (fee + buffer) * 2);
        uint256 floatBefore = address(nftRouter).balance;

        vm.prank(owner);
        nftRouter.send{value: fee + buffer}(params);
        assertEq(address(nftRouter).balance, floatBefore, "first entry: no leakage");

        vm.prank(owner);
        nftRouter.send{value: fee + buffer}(params);
        assertEq(address(nftRouter).balance, floatBefore, "second entry: no leakage");
        assertEq(owner.balance, 2 * buffer, "both excess values refunded");
    }

    function test_Entry_RefundFailsRevertsRefundFailed() public {
        // `_send` refunds excess to msg.sender via `.call{value: refund}("")`. A caller whose
        // receive() reverts trips the ReturnFailed guard. Without it, a refactor that swallowed
        // the .call return would silently seed the relay float with the entry caller's excess.
        NftRefundRejector rejector = new NftRefundRejector(address(nftRouter));
        intex.issueIntex(address(rejector), 1, SERIES_ID);

        SendParam memory params = _sendParam();
        params.to = bytes32(uint256(uint160(address(rejector))));
        uint256 fee = nftRouter.quoteSend(params);
        uint256 buffer = 0.3 ether;
        vm.deal(address(rejector), fee + buffer);

        vm.expectRevert(ERC7786Sender.ReturnFailed.selector);
        rejector.callSend{value: fee + buffer}(params);
    }

    // ---------------------------------------------------------------
    // Relay / float path - fired from inside receiveFromOutbe (CLEARING)
    // ---------------------------------------------------------------

    /// @dev The inbound CLEARING handler relays the day's bids, funding the send from IntexAuctionTargetRouter's float.
    ///      With that float empty the send reverts, the round rolls back whole, and the day stays resumable.
    function test_Relay_InsideReceiveFromOutbe_EmptyFloatLeavesTheDayResumable() public {
        assertEq(address(bnbRouter).balance, 0, "router float unfunded");

        _deliverClearing();

        (uint16 nextPage, uint16 totalPages, bool done) = bnbRouter.getIntexAuctionBidsSendState(SERIES_ID_DAY);
        assertEq(nextPage, 0, "the float-starved round sent nothing");
        assertEq(totalPages, 0, "and rolled back the span it had frozen");
        assertFalse(done, "so the day is still open");
    }

    /// @dev With IntexAuctionTargetRouter's float funded, the relay fired from inside `receiveFromOutbe` draws the fee and
    ///      sends cleanly. Nothing is parked.
    function test_Relay_InsideReceiveFromOutbe_FundedFloatSucceeds() public {
        vm.deal(address(bnbRouter), 1 ether);
        uint256 floatBefore = address(bnbRouter).balance;

        _deliverClearing();

        // One bid relays as a data batch plus the completeness marker, so the float pays two fees.
        (,, bool done) = bnbRouter.getIntexAuctionBidsSendState(SERIES_ID_DAY);
        assertTrue(done, "the day relayed whole");
        assertEq(floatBefore - address(bnbRouter).balance, 2 * BRIDGE_FEE, "every batch drew its fee from the float");
    }

    function _deliverClearing() internal {
        _deliver(
            OUTBE_CHAIN_ID, outbeIca, address(bnbRouter), BridgeMsgCodec.encodeIntexAuctionClearingStart(SERIES_ID_DAY)
        );
    }
}

/// @dev Auction stub with one bid, so an inbound CLEARING produces exactly one relayed batch - one bridge fee.
contract StubAuction {
    function startAuction(uint32, IIntexAuction.AuctionSchedule calldata, IIntexAuction.AuctionParams calldata)
        external {}
    function startAuctionClearing(uint32) external {}
    function completeAuction(uint32, uint32, uint32, uint32, uint64, uint64) external {}

    function getAuctionDetails(uint32)
        external
        view
        returns (IIntexAuction.AuctionData memory data, IIntexAuction.SubmittedIntexAuctionBidData[] memory bids)
    {
        bids = new IIntexAuction.SubmittedIntexAuctionBidData[](1);
        bids[0] = IIntexAuction.SubmittedIntexAuctionBidData({
            bidder: address(0xCAFE),
            intexQuantity: 1,
            intexBidRate: 100e6,
            timestamp: uint32(block.timestamp),
            issuanceCurrency: 840,
            referenceCurrency: 840
        });
    }

    function revealedIntexAuctionBidsCount(uint32) external view returns (uint256) {
        return 1;
    }

    function revealedIntexAuctionBidsSlice(uint32, uint256 offset, uint256 limit)
        external
        view
        returns (IIntexAuction.SubmittedIntexAuctionBidData[] memory slice)
    {
        uint256 length = 1;
        if (offset >= length) return new IIntexAuction.SubmittedIntexAuctionBidData[](0);
        uint256 end = offset + limit;
        if (end > length) end = length;
        slice = new IIntexAuction.SubmittedIntexAuctionBidData[](end - offset);
        for (uint256 i = 0; i < slice.length; ++i) {
            slice[i] = IIntexAuction.SubmittedIntexAuctionBidData({
                bidder: address(uint160(0xCAFE + offset + i)),
                intexQuantity: 1,
                intexBidRate: 100e6,
                timestamp: uint32(block.timestamp),
                issuanceCurrency: 840,
                referenceCurrency: 840
            });
        }
    }

    function auctionStage(uint32) external pure returns (IIntexAuction.AuctionStage) {
        return IIntexAuction.AuctionStage.AwaitingResult;
    }
}

/// @dev Holds a bridgeable token (accepts the ERC-1155 mint), but its `receive()` reverts.
///      Tests use it to pin `_send`'s ReturnFailed guard on the entry path via the NFT bridge.
contract NftRefundRejector {
    IntexNFT1155Router private immutable bridge;

    constructor(address _bridge) {
        bridge = IntexNFT1155Router(payable(_bridge));
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
