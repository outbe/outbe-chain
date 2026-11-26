// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {IIntexAuctionTargetRouter} from "@contracts/target/interfaces/IIntexAuctionTargetRouter.sol";
import {IIntexNFT1155Router, RemoteRouter} from "@contracts/shared/interfaces/IIntexNFT1155Router.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";
import {DESIS} from "@precompiles/IDesis.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {InboundReason} from "@contracts/shared/libs/InboundReason.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {ReferenceCurrencyPriceLib} from "../helpers/ReferenceCurrencyPriceLib.sol";

import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";

/**
 * @title IntexAuctionTargetRouterTest
 * @notice Foundry tests for IntexAuctionTargetRouter
 * @dev Tests message encoding/decoding and cross-chain communication.
 *      All auction/series messages are keyed by `seriesId` (uint32).
 */
contract IntexAuctionTargetRouterTest is CrossChainTest {
    uint32 private constant BNB_CHAIN_ID = 1;
    uint32 private constant OUTBE_CHAIN_ID = 2;

    IntexAuctionTargetRouter private targetRouter;
    IntexNFT1155Router private nftRouter;

    // Mock contracts
    IntexAuction private auction;
    IntexNFT1155 private intex;

    address private admin = address(this);
    address private user = address(0x1);

    address private outbeIca = makeAddr("outbeIca");

    uint32 private constant SERIES_ID_DAY = 20250115;
    bytes14 private constant SERIES_ID = "20250115-USD-U"; // yyyymmdd format

    function setUp() public {
        _setUpBridge();
        // The quote test asserts a non-zero fee. Give the loopback bridge a fixed fee to return.
        bridge.setFee(0.001 ether);

        vm.deal(admin, 1000 ether);
        vm.deal(user, 1000 ether);

        address routerProxy = DeployProxy.reserve();
        address nftRouterProxy = DeployProxy.reserve();
        intex = DeployProxy.intexNFT1155(admin, DeployProxy.NftLinks(routerProxy, nftRouterProxy, admin, admin, admin));
        auction = DeployProxy.intexAuction(admin, routerProxy, admin, address(intex));
        targetRouter = DeployProxy.targetRouterAt(
            routerProxy,
            address(bridge),
            admin,
            DeployProxy.TargetRouterLinks(
                OUTBE_CHAIN_ID, DeployProxy.WCOEN_BRIDGE, address(auction), address(intex), admin, outbeIca
            )
        );
        nftRouter = DeployProxy.intexNFT1155RouterAt(
            nftRouterProxy, address(intex), address(bridge), admin, new RemoteRouter[](0)
        );
        _wireOutbeIca(targetRouter, outbeIca);
    }

    // --- Constructor Tests ---
    function test_constructor() public view {
        assertEq(targetRouter.owner(), admin);
    }

    function test_wire() public view {
        assertEq(address(targetRouter.intexAuction()), address(auction));
        assertEq(address(targetRouter.intexContract()), address(intex));
        assertEq(address(targetRouter.escrow()), admin);
        assertEq(targetRouter.outbeIca(), outbeIca);
        assertEq(address(targetRouter.erc20Router()), DeployProxy.WCOEN_BRIDGE);
    }

    function _newRouter(address auction_, address intex_, address escrow_, address ica) private {
        new IntexAuctionTargetRouter(
            address(bridge), OUTBE_CHAIN_ID, DeployProxy.WCOEN_BRIDGE, auction_, intex_, escrow_, ica
        );
    }

    function test_constructor_revert_zero_auction() public {
        vm.expectRevert(abi.encodeWithSelector(IIntexAuctionTargetRouter.ZeroAddress.selector, "intexAuction"));
        _newRouter(address(0), address(intex), admin, outbeIca);
    }

    function test_constructor_revert_zero_intex() public {
        vm.expectRevert(abi.encodeWithSelector(IIntexAuctionTargetRouter.ZeroAddress.selector, "intexNFT1155"));
        _newRouter(address(auction), address(0), admin, outbeIca);
    }

    function test_constructor_revert_zero_escrowAdapter() public {
        vm.expectRevert(abi.encodeWithSelector(IIntexAuctionTargetRouter.ZeroAddress.selector, "escrow"));
        _newRouter(address(auction), address(intex), address(0), outbeIca);
    }

    function test_constructor_revert_zero_outbe_ica() public {
        vm.expectRevert(abi.encodeWithSelector(IIntexAuctionTargetRouter.ZeroAddress.selector, "outbeIca"));
        _newRouter(address(auction), address(intex), admin, address(0));
    }

    // --- Inbound from Outbe ---
    function _start(uint32 commitEnd) private pure returns (bytes memory) {
        return BridgeMsgCodec.encodeIntexAuctionStart(
            SERIES_ID_DAY,
            commitEnd,
            commitEnd + 1 days,
            commitEnd + 2 days,
            1e6,
            60e6,
            ReferenceCurrencyPriceLib.one(840, 100e6, 100e6),
            0,
            0,
            0,
            1,
            0
        );
    }

    function test_receiveFromOutbe_revert_from_anyone_but_the_outbe_ica() public {
        bytes memory start = _start(uint32(block.timestamp + 1 days));

        vm.expectRevert(abi.encodeWithSelector(IIntexAuctionTargetRouter.UnauthorizedCaller.selector, user));
        vm.prank(user);
        targetRouter.receiveFromOutbe(start);

        vm.expectRevert(abi.encodeWithSelector(IIntexAuctionTargetRouter.UnauthorizedCaller.selector, admin));
        targetRouter.receiveFromOutbe(start);

        vm.expectRevert(abi.encodeWithSelector(IIntexAuctionTargetRouter.UnauthorizedCaller.selector, address(bridge)));
        vm.prank(address(bridge));
        targetRouter.receiveFromOutbe(start);
    }

    function test_receiveFromOutbe_start_opens_the_auction_and_a_repeat_is_a_duplicate() public {
        bytes memory start = _start(uint32(block.timestamp + 1 days));

        vm.prank(outbeIca);
        targetRouter.receiveFromOutbe(start);
        assertEq(uint8(auction.auctionStage(SERIES_ID_DAY)), uint8(IIntexAuction.AuctionStage.CommittingBids));

        vm.expectEmit(true, true, false, true, address(targetRouter));
        emit IIntexAuctionTargetRouter.IntexAuctionMessageIgnored(
            BridgeMsgCodec.MSG_INTEX_AUCTION_START, SERIES_ID_DAY, InboundReason.DUPLICATE
        );
        vm.prank(outbeIca);
        targetRouter.receiveFromOutbe(start);
        assertEq(uint8(auction.auctionStage(SERIES_ID_DAY)), uint8(IIntexAuction.AuctionStage.CommittingBids));
    }

    function test_bids_leave_for_desis_on_outbe() public {
        vm.etch(DESIS, address(new MockDesis()).code);
        vm.deal(address(targetRouter), 1 ether);
        uint32 commitEnd = uint32(block.timestamp + 1 days);
        _deliver(OUTBE_CHAIN_ID, outbeIca, address(targetRouter), _start(commitEnd));
        vm.warp(commitEnd + 1);

        bridge.setAutoDeliver(true);
        _deliver(
            OUTBE_CHAIN_ID,
            outbeIca,
            address(targetRouter),
            BridgeMsgCodec.encodeIntexAuctionClearingStart(SERIES_ID_DAY)
        );

        assertEq(bridge.lastRecipient(), _interop(OUTBE_CHAIN_ID, DESIS), "the bids go to Desis on Outbe");
        assertEq(MockDesis(DESIS).received(), 2, "the empty page and the totals marker");
        assertEq(MockDesis(DESIS).lastSender(), _interop(uint32(block.chainid), address(targetRouter)));
        assertEq(
            MockDesis(DESIS).lastPayloadHash(),
            keccak256(BridgeMsgCodec.encodeBidsTotals(SERIES_ID_DAY, uint32(block.chainid), 1, 0)),
            "the totals marker closes the relay"
        );
    }
}
