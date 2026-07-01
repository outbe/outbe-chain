// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {MockERC7786Bridge} from "@test-mocks/MockERC7786Bridge.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {OriginMessenger} from "@contracts/origin/OriginMessenger.sol";
import {TargetMessenger} from "@contracts/target/TargetMessenger.sol";
import {ONFT1155Adapter} from "@contracts/shared/ONFT1155Adapter.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";
import {
    UPGRADE_PROBE,
    IntexNFT1155V2,
    IntexNFT1155V2Reinit,
    IntexAuctionV2,
    EscrowAdapterV2,
    OriginMessengerV2,
    TargetMessengerV2,
    ONFT1155AdapterV2,
    ONFT1155AdapterBatchV2
} from "./UpgradeStubs.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";
import {MockERC20} from "@test-mocks/MockERC20.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockVaultProvider} from "@test-mocks/MockVaultProvider.sol";

interface IUpgradeProbe {
    function upgradeProbe() external pure returns (uint256);
}

/// @dev End-to-end upgrade rehearsal: deploy v1 behind a proxy, populate real state, upgrade the
///      implementation to a v1.1 stub that adds a new view, then assert that persisted state
///      survived the upgrade, the implementation pointer moved, and the new view is callable.
///      Covers one upgrade per impl contract.
/// @dev The three ERC-7786 clients (OriginMessenger, TargetMessenger, ONFT1155AdapterBatch) run against a standalone
///      {MockERC7786Bridge}; the single `ONFT1155Adapter` stays a LayerZero OApp, so the `TestHelperOz5` endpoint
///      harness is retained for it (mirrors `ONFTAdapters.uups.t.sol`).
contract UpgradeDrillTest is TestHelperOz5 {
    // LayerZero endpoint ids for the single (unchanged) `ONFT1155Adapter` drill.
    uint32 internal constant A_EID = 1;
    uint32 internal constant B_EID = 2;
    // ERC-7786 chainIds for the bridge-client drills.
    uint32 internal constant A_CHAIN_ID = 1;
    uint32 internal constant B_CHAIN_ID = 2;

    address internal admin = makeAddr("admin");

    MockERC7786Bridge internal bridge;

    function setUp() public override {
        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);
        bridge = new MockERC7786Bridge();
    }

    /// @dev ERC-7930 interoperable address for `a` on `chainId`.
    function _interop(uint32 chainId, address a) internal pure returns (bytes memory) {
        return InteroperableAddress.formatEvmV1(chainId, a);
    }

    function _assertUpgraded(address proxy, address newImpl) internal view {
        bytes32 implSlot = vm.load(proxy, ERC1967Utils.IMPLEMENTATION_SLOT);
        assertEq(address(uint160(uint256(implSlot))), newImpl, "implementation not swapped");
        assertEq(IUpgradeProbe(proxy).upgradeProbe(), UPGRADE_PROBE, "new view unreachable");
    }

    function test_Drill_IntexNFT1155() public {
        IntexNFT1155 nft = DeployProxy.intexNFT1155(admin, admin);
        address holder = makeAddr("holder");

        vm.startPrank(admin);
        nft.createSeries(CreateSeriesLib.params(7, 100, 0));
        nft.mint(holder, 3, 7);
        vm.stopPrank();

        IntexNFT1155V2 newImpl = new IntexNFT1155V2();
        vm.prank(admin);
        nft.upgradeToAndCall(address(newImpl), "");

        _assertUpgraded(address(nft), address(newImpl));
        assertEq(nft.balanceOf(holder, 7), 3, "balance lost");
        assertEq(nft.totalSupply(7), 3, "supply lost");
        (,,,,,,,, uint32 issuedAt,,,, IIntexNFT1155.IntexState state) = nft.seriesData(7);
        assertGt(issuedAt, 0, "series record lost");
        assertEq(uint8(state), uint8(IIntexNFT1155.IntexState.Issued), "state lost");
        assertTrue(nft.hasRole(nft.RELAYER_ROLE(), admin), "role lost");
    }

    /// @dev Exercises the `upgradeToAndCall` init-data path: upgrade runs a `reinitializer(2)`
    ///      migration that sets a new v2 field, while pre-upgrade state survives.
    function test_Drill_IntexNFT1155_ReinitializerPath() public {
        IntexNFT1155 nft = DeployProxy.intexNFT1155(admin, admin);
        address holder = makeAddr("holder");

        vm.startPrank(admin);
        nft.createSeries(CreateSeriesLib.params(7, 100, 0));
        nft.mint(holder, 3, 7);
        vm.stopPrank();

        IntexNFT1155V2Reinit newImpl = new IntexNFT1155V2Reinit();
        vm.prank(admin);
        nft.upgradeToAndCall(address(newImpl), abi.encodeCall(IntexNFT1155V2Reinit.initializeV2, (UPGRADE_PROBE)));

        bytes32 implSlot = vm.load(address(nft), ERC1967Utils.IMPLEMENTATION_SLOT);
        assertEq(address(uint160(uint256(implSlot))), address(newImpl), "implementation not swapped");
        assertEq(IntexNFT1155V2Reinit(address(nft)).migratedFlag(), UPGRADE_PROBE, "reinitializer did not run");
        assertEq(nft.balanceOf(holder, 7), 3, "balance lost across reinit");
        assertEq(nft.totalSupply(7), 3, "supply lost across reinit");
    }

    function test_Drill_IntexAuction() public {
        IntexAuction auction = DeployProxy.intexAuction(admin, admin);
        address escrow = makeAddr("escrow");
        address bidder = makeAddr("bidder");

        IIntexAuction.AuctionSchedule memory schedule = IIntexAuction.AuctionSchedule({
            commitEnd: uint32(block.timestamp + 1 hours),
            revealEnd: uint32(block.timestamp + 2 hours),
            issuanceEnd: uint32(block.timestamp + 3 hours)
        });
        IIntexAuction.AuctionParams memory params = IIntexAuction.AuctionParams({
            issuanceCurrency: 840,
            referenceCurrency: 840,
            promisLoadMinor: 1000,
            minIntexBidRate: 1,
            entryPriceMinor: 1,
            floorPriceMinor: 1,
            callPriceMinor: 1,
            callTrigger: IIntexAuction.IntexCallTrigger({windowDays: 0, thresholdDays: 0, intexCallPeriod: 0}),
            minIntexBidQuantity: 1
        });

        vm.startPrank(admin);
        auction.wire(escrow);
        auction.auctionStart(20260614, schedule, params);
        vm.stopPrank();
        vm.prank(bidder);
        auction.commitBid(20260614, keccak256("commit"));

        IntexAuctionV2 newImpl = new IntexAuctionV2();
        vm.prank(admin);
        auction.upgradeToAndCall(address(newImpl), "");

        _assertUpgraded(address(auction), address(newImpl));
        assertEq(address(auction.escrowContract()), escrow, "escrow wiring lost");
        assertEq(auction.committedBidsByHash(20260614, bidder), keccak256("commit"), "commit lost");
        assertEq(
            uint8(auction.getAuctionStage(20260614)), uint8(IIntexAuction.AuctionStage.CommittingBids), "stage lost"
        );
    }

    function test_Drill_EscrowAdapter() public {
        EscrowAdapter escrow = DeployProxy.escrowAdapter(admin, admin);
        MockERC20 token = new MockERC20("Mock USD", "MUSD", 6);
        MockTheCompact compactMock = new MockTheCompact();
        MockVaultProvider vault = new MockVaultProvider();
        address auction = makeAddr("auction");

        vm.prank(admin);
        escrow.wire(auction, address(compactMock), address(vault), address(token));
        uint96 allocatorId = escrow.allocatorId();
        bytes12 lockTag = escrow.lockTag();

        EscrowAdapterV2 newImpl = new EscrowAdapterV2();
        vm.prank(admin);
        escrow.upgradeToAndCall(address(newImpl), "");

        _assertUpgraded(address(escrow), address(newImpl));
        assertEq(escrow.intexAuctionContract(), auction, "auction wiring lost");
        assertEq(address(escrow.paymentToken()), address(token), "token wiring lost");
        assertEq(escrow.allocatorId(), allocatorId, "allocatorId lost");
        assertEq(escrow.lockTag(), lockTag, "lockTag lost");
        assertTrue(escrow.hasRole(escrow.AUCTION_ROLE(), auction), "auction role lost");
    }

    function test_Drill_OriginMessenger() public {
        OriginMessenger origin = DeployProxy.originMessenger(address(bridge), admin, B_CHAIN_ID);
        MockDesis desisMock = new MockDesis();
        address factory = makeAddr("factory");
        bytes memory remote = _interop(B_CHAIN_ID, address(0xBEEF));

        vm.startPrank(admin);
        origin.wire(address(desisMock), factory);
        origin.setRemoteMessenger(B_CHAIN_ID, remote);
        vm.stopPrank();

        OriginMessengerV2 newImpl = new OriginMessengerV2(address(bridge), B_CHAIN_ID);
        vm.prank(admin);
        origin.upgradeToAndCall(address(newImpl), "");

        _assertUpgraded(address(origin), address(newImpl));
        assertEq(origin.desis(), address(desisMock), "desis wiring lost");
        assertEq(origin.intexFactory(), factory, "factory wiring lost");
        assertEq(origin.remoteMessenger(B_CHAIN_ID), remote, "remote messenger lost");
        assertEq(origin.BNB_CHAIN_ID(), B_CHAIN_ID, "immutable lost");
    }

    function test_Drill_TargetMessenger() public {
        TargetMessenger target = DeployProxy.targetMessenger(address(bridge), admin, A_CHAIN_ID);
        address auction = makeAddr("auction");
        address intex = makeAddr("intex");
        address escrow = makeAddr("escrow");
        address onft = makeAddr("onft");
        bytes memory remote = _interop(A_CHAIN_ID, address(0xCAFE));

        vm.startPrank(admin);
        target.wire(auction, intex, escrow, onft);
        target.setRemoteMessenger(A_CHAIN_ID, remote);
        vm.stopPrank();

        TargetMessengerV2 newImpl = new TargetMessengerV2(address(bridge), A_CHAIN_ID);
        vm.prank(admin);
        target.upgradeToAndCall(address(newImpl), "");

        _assertUpgraded(address(target), address(newImpl));
        assertEq(address(target.auction()), auction, "auction wiring lost");
        assertEq(address(target.escrowAdapter()), escrow, "escrow wiring lost");
        assertEq(target.remoteMessenger(A_CHAIN_ID), remote, "remote messenger lost");
    }

    function test_Drill_ONFT1155Adapter() public {
        address tokenAddr = makeAddr("token");
        ONFT1155Adapter adapter = DeployProxy.onftAdapter(tokenAddr, address(endpoints[A_EID]), admin);

        vm.prank(admin);
        adapter.setPeer(B_EID, addressToBytes32(address(0xBEEF)));

        ONFT1155AdapterV2 newImpl = new ONFT1155AdapterV2(tokenAddr, address(endpoints[A_EID]));
        vm.prank(admin);
        adapter.upgradeToAndCall(address(newImpl), "");

        _assertUpgraded(address(adapter), address(newImpl));
        assertEq(adapter.peers(B_EID), addressToBytes32(address(0xBEEF)), "peer lost");
        assertEq(address(adapter.token()), tokenAddr, "token immutable lost");
        assertEq(adapter.owner(), admin, "owner lost");
    }

    function test_Drill_ONFT1155AdapterBatch() public {
        address tokenAddr = makeAddr("token");
        ONFT1155AdapterBatch batch = DeployProxy.onftAdapterBatch(tokenAddr, address(bridge), admin);
        address relayer = makeAddr("relayer");
        bytes memory remote = _interop(B_CHAIN_ID, address(0xBEEF));

        vm.startPrank(admin);
        batch.setRemoteMessenger(B_CHAIN_ID, remote);
        batch.grantRole(batch.SYSTEM_RELAYER_ROLE(), relayer);
        vm.stopPrank();

        ONFT1155AdapterBatchV2 newImpl = new ONFT1155AdapterBatchV2(tokenAddr, address(bridge));
        vm.prank(admin);
        batch.upgradeToAndCall(address(newImpl), "");

        _assertUpgraded(address(batch), address(newImpl));
        assertEq(batch.remoteMessenger(B_CHAIN_ID), remote, "remote messenger lost");
        assertEq(address(batch.token()), tokenAddr, "token immutable lost");
        assertTrue(batch.hasRole(batch.SYSTEM_RELAYER_ROLE(), relayer), "role lost");
    }
}
