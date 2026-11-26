// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ReferenceCurrencyPriceLib} from "../helpers/ReferenceCurrencyPriceLib.sol";
import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {OpenSeriesLib} from "../helpers/OpenSeriesLib.sol";
import {
    UPGRADE_PROBE,
    IntexNFT1155V2,
    IntexNFT1155V2Reinit,
    IntexAuctionV2,
    EscrowAdapterV2,
    IntexAuctionTargetRouterV2,
    IntexNFT1155RouterV2
} from "./UpgradeStubs.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";

interface IUpgradeProbe {
    function upgradeProbe() external pure returns (uint256);
}

/// @dev End-to-end upgrade rehearsal. The rehearsal has these steps:
///      1. Deploy v1 behind a proxy.
///      2. Populate real state.
///      3. Upgrade the implementation to a v1.1 stub that adds a new view.
///      4. Assert that persisted state survived the upgrade, the implementation pointer moved,
///         and the new view is callable.
///      Covers one upgrade per impl contract. The bridge clients (IntexAuctionTargetRouter and IntexNFT1155Router)
///      run against a standalone {MockERC7786Bridge}.
contract UpgradeDrillTest is CrossChainTest {
    uint32 internal constant A_CHAIN_ID = 1;
    uint32 internal constant B_CHAIN_ID = 2;

    address internal admin = makeAddr("admin");

    function setUp() public {
        _setUpBridge();
    }

    function _assertUpgraded(address proxy, address newImpl) internal view {
        bytes32 implSlot = vm.load(proxy, ERC1967Utils.IMPLEMENTATION_SLOT);
        assertEq(address(uint160(uint256(implSlot))), newImpl, "implementation not swapped");
        assertEq(IUpgradeProbe(proxy).upgradeProbe(), UPGRADE_PROBE, "new view unreachable");
    }

    function test_Drill_IntexNFT1155() public {
        IntexNFT1155 nft = DeployProxy.intexNFT1155(admin, admin);
        address owner = makeAddr("owner");

        vm.startPrank(admin);
        nft.openSeries(OpenSeriesLib.params(7, 100, 0));
        nft.issueIntex(owner, 3, OpenSeriesLib.seriesId(7));
        vm.stopPrank();

        IntexNFT1155V2 newImpl = new IntexNFT1155V2(admin, admin, admin, admin, admin);
        vm.prank(admin);
        nft.upgradeToAndCall(address(newImpl), "");

        _assertUpgraded(address(nft), address(newImpl));
        assertEq(nft.balanceOf(owner, nft.issuedIntexId(OpenSeriesLib.seriesId(7))), 3, "balance lost");
        assertEq(nft.totalSupply(nft.issuedIntexId(OpenSeriesLib.seriesId(7))), 3, "supply lost");
        IIntexNFT1155.SeriesData memory data = nft.getIntexSeries(OpenSeriesLib.seriesId(7));
        assertGt(data.issuedAt, 0, "series record lost");
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Issued), "state lost");
        assertEq(nft.targetRouter(), admin, "link lost");
        assertEq(nft.owner(), admin, "owner lost");
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.IntexNFT1155V2Reinit")) - 1)) & ~bytes32(uint256(0xff))
    // Mirrors IntexNFT1155V2Reinit's private storage slot (UpgradeStubs.sol) so the test can read
    // the migrated field via vm.load without a dedicated getter on the stub.
    bytes32 private constant _V2_REINIT_SLOT = 0xa6131e184e5aae318840a83507194e5ed64c56b50a1ac526e8c519cdd8bb2200;

    /// @dev Exercises the `upgradeToAndCall` init-data path. The upgrade runs a `reinitializer(2)`
    ///      migration that sets a new v2 field. Pre-upgrade state survives.
    function test_Drill_IntexNFT1155_ReinitializerPath() public {
        IntexNFT1155 nft = DeployProxy.intexNFT1155(admin, admin);
        address owner = makeAddr("owner");

        vm.startPrank(admin);
        nft.openSeries(OpenSeriesLib.params(7, 100, 0));
        nft.issueIntex(owner, 3, OpenSeriesLib.seriesId(7));
        vm.stopPrank();

        IntexNFT1155V2Reinit newImpl = new IntexNFT1155V2Reinit(admin, admin, admin, admin, admin);
        vm.prank(admin);
        nft.upgradeToAndCall(address(newImpl), abi.encodeCall(IntexNFT1155V2Reinit.initializeV2, (UPGRADE_PROBE)));

        bytes32 implSlot = vm.load(address(nft), ERC1967Utils.IMPLEMENTATION_SLOT);
        assertEq(address(uint160(uint256(implSlot))), address(newImpl), "implementation not swapped");
        uint256 migratedFlag = uint256(vm.load(address(nft), _V2_REINIT_SLOT));
        assertEq(migratedFlag, UPGRADE_PROBE, "reinitializer did not run");
        assertEq(nft.balanceOf(owner, nft.issuedIntexId(OpenSeriesLib.seriesId(7))), 3, "balance lost across reinit");
        assertEq(nft.totalSupply(nft.issuedIntexId(OpenSeriesLib.seriesId(7))), 3, "supply lost across reinit");
    }

    function test_Drill_IntexAuction() public {
        address escrow = makeAddr("escrow");
        IntexAuction auction = DeployProxy.intexAuction(admin, admin, escrow, makeAddr("intex"));
        address bidder = makeAddr("bidder");

        IIntexAuction.AuctionSchedule memory schedule = IIntexAuction.AuctionSchedule({
            commitEnd: uint32(block.timestamp + 1 hours),
            revealEnd: uint32(block.timestamp + 2 hours),
            issuanceEnd: uint32(block.timestamp + 3 hours)
        });
        IIntexAuction.AuctionParams memory params = IIntexAuction.AuctionParams({
            promisLoadMinor: 1000,
            minBidRate: 1,
            prices: ReferenceCurrencyPriceLib.onePriced(840, 1, 1),
            callTrigger: IIntexAuction.IntexCallTrigger({callWindow: 0, callThreshold: 0, callNoticePeriod: 0}),
            minBidQuantity: 1,
            bidBondMinor: 0
        });

        vm.prank(admin);
        auction.startAuction(20260614, schedule, params);
        vm.prank(bidder);
        auction.commitIntexAuctionBid(20260614, keccak256("commit"));

        IntexAuctionV2 newImpl = new IntexAuctionV2(admin, escrow, makeAddr("intex"));
        vm.prank(admin);
        auction.upgradeToAndCall(address(newImpl), "");

        _assertUpgraded(address(auction), address(newImpl));
        assertEq(address(auction.escrow()), escrow, "escrow wiring lost");
        assertEq(auction.committedIntexAuctionBidsByHash(20260614, bidder), keccak256("commit"), "commit lost");
        assertEq(uint8(auction.auctionStage(20260614)), uint8(IIntexAuction.AuctionStage.CommittingBids), "stage lost");
    }

    function test_Drill_EscrowAdapter() public {
        MockWCOEN token = new MockWCOEN();
        MockTheCompact compactMock = new MockTheCompact();
        address auction = makeAddr("auction");
        EscrowAdapter escrow = DeployProxy.escrowAdapter(admin, auction, admin, address(compactMock), address(token));
        uint96 allocatorId = escrow.allocatorId();
        bytes12 lockTag = escrow.lockTag();

        EscrowAdapterV2 newImpl = new EscrowAdapterV2(auction, admin);
        vm.prank(admin);
        escrow.upgradeToAndCall(address(newImpl), "");

        _assertUpgraded(address(escrow), address(newImpl));
        assertEq(escrow.intexAuction(), auction, "auction wiring lost");
        assertEq(address(escrow.intexAuctionAsset()), address(token), "token wiring lost");
        assertEq(escrow.allocatorId(), allocatorId, "allocatorId lost");
        assertEq(escrow.lockTag(), lockTag, "lockTag lost");
        assertEq(escrow.owner(), admin, "owner lost");
    }

    function test_Drill_IntexAuctionTargetRouter() public {
        address erc20Router = makeAddr("erc20Router");
        address auction = makeAddr("auction");
        address intex = makeAddr("intex");
        address escrow = makeAddr("escrow");
        address ica = makeAddr("outbeIca");
        IntexAuctionTargetRouter target = DeployProxy.targetRouter(
            address(bridge), admin, DeployProxy.TargetRouterLinks(A_CHAIN_ID, erc20Router, auction, intex, escrow, ica)
        );

        IntexAuctionTargetRouterV2 newImpl =
            new IntexAuctionTargetRouterV2(address(bridge), A_CHAIN_ID, erc20Router, auction, intex, escrow, ica);
        vm.prank(admin);
        target.upgradeToAndCall(address(newImpl), "");

        _assertUpgraded(address(target), address(newImpl));
        assertEq(address(target.intexAuction()), auction, "auction wiring lost");
        assertEq(address(target.escrow()), escrow, "escrow wiring lost");
        assertEq(target.outbeIca(), ica, "ICA lost");
        assertEq(target.owner(), admin, "owner lost");
        assertEq(address(target.erc20Router()), erc20Router, "erc20Router immutable lost");
    }

    function test_Drill_IntexNFT1155Router() public {
        address tokenAddr = makeAddr("token");
        IntexNFT1155Router batch = DeployProxy.intexNFT1155Router(tokenAddr, address(bridge), admin);
        bytes memory remote = _interop(B_CHAIN_ID, address(0xBEEF));

        vm.prank(admin);
        batch.setRemoteRouter(B_CHAIN_ID, remote);

        IntexNFT1155RouterV2 newImpl = new IntexNFT1155RouterV2(tokenAddr, address(bridge));
        vm.prank(admin);
        batch.upgradeToAndCall(address(newImpl), "");

        _assertUpgraded(address(batch), address(newImpl));
        assertEq(batch.remoteRouter(B_CHAIN_ID), remote, "remote router lost");
        assertEq(address(batch.token()), tokenAddr, "token immutable lost");
        assertEq(batch.owner(), admin, "owner lost");
    }
}
