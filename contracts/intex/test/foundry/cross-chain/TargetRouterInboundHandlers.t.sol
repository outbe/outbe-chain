// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {ITargetRouter} from "@contracts/target/interfaces/ITargetRouter.sol";
import {OriginRouter} from "@contracts/origin/OriginRouter.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {IVaultProvider} from "@contracts/vendor/outbe-vault/interfaces/IVaultProvider.sol";
import {IntexNFT1155Bridge} from "@contracts/shared/IntexNFT1155Bridge.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockERC20} from "@test-mocks/MockERC20.sol";
import {MockSettlementVault} from "@test-mocks/MockSettlementVault.sol";
import {MockVaultProvider} from "@test-mocks/MockVaultProvider.sol";
import {RevertingERC1155Receiver} from "@test-mocks/RevertingERC1155Receiver.sol";

/// @dev End-to-end traversal of the five `TargetRouter` inbound handlers that previously only
///      had codec-level round-trip coverage. Each test hand-builds a `BridgeMsgCodec` packet and
///      drives `lzReceive` from the endpoint address, then asserts the downstream side-effect on
///      the wired contract — proving the full receiveMessage -> dispatchInbound -> _handleX -> X path
///      under the current fail-don't-drop model.
contract TargetRouterInboundHandlersTest is CrossChainTest {
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant OUTBE_CHAIN_ID = 2;

    uint32 internal constant WORLDWIDE_DAY = 20250101; // yyyymmdd — the auction day (root)
    uint32 internal constant SERIES_ID = WORLDWIDE_DAY; // derived (identity while one series per day)
    uint32 internal constant ISSUED_INTEX_COUNT = 100;
    uint128 internal constant PROMIS_LOAD_MINOR = 1000;
    uint64 internal constant ENTRY_PRICE = 100e6;
    uint64 internal constant FLOOR_PRICE_MINOR = 40e6;
    uint16 internal constant REFERENCE_CURRENCY = 840;

    TargetRouter internal bnbRouter;
    OriginRouter internal outbeRouter;
    IntexAuction internal auction;
    IntexNFT1155 internal intex;
    EscrowAdapter internal escrow;
    IntexNFT1155Bridge internal nftBridge;
    MockTheCompact internal compact;
    MockERC20 internal paymentToken;
    MockSettlementVault internal vault;
    MockVaultProvider internal provider;

    address internal admin = address(this);
    address internal bidder = address(0xB1);

    function setUp() public {
        _setUpBridge();

        intex = DeployProxy.intexNFT1155(admin, admin);
        auction = DeployProxy.intexAuction(admin, admin);

        bnbRouter = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        outbeRouter = DeployProxy.originRouter(address(bridge), admin, BNB_CHAIN_ID);
        nftBridge = DeployProxy.intexNFT1155Bridge(address(intex), address(bridge), admin);

        escrow = DeployProxy.escrowAdapter(admin, admin);
        compact = new MockTheCompact();
        paymentToken = new MockERC20("USD Coin", "USDC", 6);
        vault = new MockSettlementVault(address(paymentToken), "Mock Vault USDC", "mvUSDC", 6);
        provider = new MockVaultProvider();
        provider.addVault(vault);
        provider.addLiquiditySource(address(escrow), IVaultProvider.LiquiditySource.IntexBidPrice);
        escrow.wire(admin, address(compact), address(provider), address(paymentToken));
        compact.setResetPeriodSeconds(0);

        bnbRouter.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(outbeRouter)));
        outbeRouter.setRemoteMessenger(BNB_CHAIN_ID, _interop(BNB_CHAIN_ID, address(bnbRouter)));

        bnbRouter.wire(address(auction), address(intex), address(escrow), address(nftBridge));

        // The router drives auction lifecycle (RELAYER_ROLE), creates/mints IntexNFT1155
        // (RELAYER_ROLE), and finalizes escrow (RELAYER_ROLE). Each downstream contract gates
        // mutations on its own RELAYER_ROLE which only the wired router should hold.
        auction.grantRole(auction.RELAYER_ROLE(), address(bnbRouter));
        intex.grantRole(intex.RELAYER_ROLE(), address(bnbRouter));
        escrow.grantRole(escrow.RELAYER_ROLE(), address(bnbRouter));

        // Bidder funds + approve so EscrowAdapter.lockFunds works in the REFUND_INSTRUCTIONS test.
        paymentToken.mint(bidder, 1e24);
        vm.prank(bidder);
        paymentToken.approve(address(escrow), type(uint256).max);
    }

    // --- _handleAuctionStageReveal: green-day signal -> auction stage flips to Revealing ---
    function test_handleAuctionStageReveal_advancesAuctionToRevealing() public {
        _seedAuction();

        bytes memory packet = BridgeMsgCodec.encodeAuctionStageReveal(WORLDWIDE_DAY, true);
        _deliver(packet);

        assertEq(
            uint8(auction.getAuctionStage(WORLDWIDE_DAY)),
            uint8(IIntexAuction.AuctionStage.RevealingBids),
            "auction advanced to RevealingBids"
        );
    }

    // --- _handleAuctionResult: clearing data is persisted on the auction ---
    function test_handleAuctionResult_persistsClearingOnAuction() public {
        _seedAuction();
        // Push the auction into Issuance stage so executeAuctionClearing is accepted.
        vm.prank(address(bnbRouter));
        auction.startRevealingBidsStage(WORLDWIDE_DAY, true);
        vm.prank(address(bnbRouter));
        auction.startClearingStage(WORLDWIDE_DAY);

        uint64 clearingPrice = 100e6;
        bytes memory packet = BridgeMsgCodec.encodeAuctionResult(WORLDWIDE_DAY, ISSUED_INTEX_COUNT, clearingPrice, 0);
        _deliver(packet);

        IIntexAuction.AuctionResult memory result = auction.getAuctionInfo(WORLDWIDE_DAY).result;
        assertEq(result.issuedIntexCount, ISSUED_INTEX_COUNT, "issuedIntexCount persisted");
        assertEq(result.auctionClearingRate, clearingPrice, "clearingPrice persisted");
    }

    // --- _handleIssuanceInstructions: createSeries + per-recipient mint on the local IntexNFT1155 ---
    function test_handleIssuanceInstructions_createsSeriesAndMints() public {
        address[] memory recipients = new address[](1);
        recipients[0] = bidder;
        uint256[] memory quantities = new uint256[](1);
        quantities[0] = 5;

        BridgeMsgCodec.IssuanceInstructionsPayload memory payload = BridgeMsgCodec.IssuanceInstructionsPayload({
            seriesId: SERIES_ID,
            worldwideDay: WORLDWIDE_DAY,
            issuedIntexCount: ISSUED_INTEX_COUNT,
            promisLoadMinor: PROMIS_LOAD_MINOR,
            entryPriceMinor: ENTRY_PRICE,
            floorPriceMinor: FLOOR_PRICE_MINOR,
            intexCallPeriod: 0,
            issuanceCurrency: 840,
            referenceCurrency: REFERENCE_CURRENCY,
            callWindowDays: 30,
            callThresholdDays: 5,
            callPriceMinor: 25e6,
            recipients: recipients,
            quantities: quantities
        });
        bytes memory packet = BridgeMsgCodec.encodeIssuanceInstructions(payload);
        _deliver(packet);

        uint256 tokenId = intex.issuedTokenId(SERIES_ID);
        assertEq(intex.balanceOf(bidder, tokenId), 5, "bidder crosschainMinted 5 Issued tokens");
        assertEq(intex.totalSupply(tokenId), 5, "totalSupply == minted amount");
    }

    function _issuancePacket(address[] memory recipients, uint256[] memory quantities)
        internal
        view
        returns (bytes memory)
    {
        return BridgeMsgCodec.encodeIssuanceInstructions(
            BridgeMsgCodec.IssuanceInstructionsPayload({
                seriesId: SERIES_ID,
                worldwideDay: WORLDWIDE_DAY,
                issuedIntexCount: ISSUED_INTEX_COUNT,
                promisLoadMinor: PROMIS_LOAD_MINOR,
                entryPriceMinor: ENTRY_PRICE,
                floorPriceMinor: FLOOR_PRICE_MINOR,
                intexCallPeriod: 0,
                issuanceCurrency: 840,
                referenceCurrency: REFERENCE_CURRENCY,
                callWindowDays: 30,
                callThresholdDays: 5,
                callPriceMinor: 25e6,
                recipients: recipients,
                quantities: quantities
            })
        );
    }

    function test_handleIssuanceInstructions_RevertingRecipient_OthersMinted() public {
        RevertingERC1155Receiver bad = new RevertingERC1155Receiver();
        address[] memory recipients = new address[](2);
        recipients[0] = bidder;
        recipients[1] = address(bad);
        uint256[] memory quantities = new uint256[](2);
        quantities[0] = 5;
        quantities[1] = 3;

        _deliver(_issuancePacket(recipients, quantities));

        uint256 tokenId = intex.issuedTokenId(SERIES_ID);
        assertEq(intex.balanceOf(bidder, tokenId), 5, "good recipient minted");
        assertEq(intex.balanceOf(address(bad), tokenId), 0, "reverting recipient not minted");
        assertEq(bnbRouter.nextPendingIssuanceMintIdx(), 1, "one mint parked");
        (uint32 s, address r, uint256 q, bool exists, bool done) = bnbRouter.pendingIssuanceMints(0);
        assertEq(s, SERIES_ID);
        assertEq(r, address(bad));
        assertEq(q, 3);
        assertTrue(exists);
        assertFalse(done);
    }

    function test_flushPendingIssuanceMint_afterFix() public {
        RevertingERC1155Receiver bad = new RevertingERC1155Receiver();
        address[] memory recipients = new address[](2);
        recipients[0] = bidder;
        recipients[1] = address(bad);
        uint256[] memory quantities = new uint256[](2);
        quantities[0] = 5;
        quantities[1] = 3;
        _deliver(_issuancePacket(recipients, quantities));

        // Recipient stops reverting; the parked mint is retried permissionlessly.
        bad.setReject(false);
        bnbRouter.flushPendingIssuanceMint(0);

        uint256 tokenId = intex.issuedTokenId(SERIES_ID);
        assertEq(intex.balanceOf(address(bad), tokenId), 3, "parked mint delivered on flush");
        (,,,, bool done) = bnbRouter.pendingIssuanceMints(0);
        assertTrue(done);

        vm.expectRevert(abi.encodeWithSelector(ITargetRouter.AlreadyFlushed.selector, uint256(0)));
        bnbRouter.flushPendingIssuanceMint(0);
    }

    // --- _handleRefundInstructions: forwarded to EscrowAdapter.finalizeAuction; lock flips Finalized ---
    function test_handleRefundInstructions_finalizesEscrow() public {
        // Lock funds for the bidder so finalizeAuction's per-bidder branch can land.
        uint64 lockedAmount = 1000e6;
        vm.prank(admin);
        escrow.grantRole(escrow.AUCTION_ROLE(), admin);
        vm.prank(admin);
        escrow.lockFunds(WORLDWIDE_DAY, bidder, lockedAmount);

        address[] memory bidders = new address[](1);
        bidders[0] = bidder;
        uint128[] memory refundedAmounts = new uint128[](1);
        refundedAmounts[0] = lockedAmount;
        uint128[] memory paidAmounts = new uint128[](1);
        paidAmounts[0] = 0;

        bytes memory packet =
            BridgeMsgCodec.encodeRefundInstructions(WORLDWIDE_DAY, bidders, refundedAmounts, paidAmounts);
        _deliver(packet);

        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(WORLDWIDE_DAY, bidder);
        assertEq(
            uint8(lock.status), uint8(IEscrowAdapter.LockStatus.Finalized), "lock advanced to Finalized via handler"
        );
    }

    // --- _handleMarkQualified: pure status flip on the local IntexNFT1155 ---
    function test_handleMarkQualified_flipsStatusOnIntex() public {
        _seedSeriesOnIntex();

        bytes memory packet = BridgeMsgCodec.encodeMarkQualified(SERIES_ID);
        _deliver(packet);

        IIntexNFT1155.SeriesData memory data = intex.readData(SERIES_ID);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Qualified), "series flipped to Qualified");
    }

    // --- Helpers ---

    function _seedAuction() internal {
        // The schedule must be strictly increasing and commitEnd > block.timestamp.
        IIntexAuction.AuctionSchedule memory schedule = IIntexAuction.AuctionSchedule({
            commitEnd: uint32(block.timestamp + 1 days),
            revealEnd: uint32(block.timestamp + 2 days),
            issuanceEnd: uint32(block.timestamp + 3 days)
        });
        IIntexAuction.AuctionParams memory params = IIntexAuction.AuctionParams({
            issuanceCurrency: 840,
            referenceCurrency: 840,
            promisLoadMinor: PROMIS_LOAD_MINOR,
            minIntexBidRate: 60e6,
            entryPriceMinor: ENTRY_PRICE,
            floorPriceMinor: FLOOR_PRICE_MINOR,
            callPriceMinor: ENTRY_PRICE,
            callTrigger: IIntexAuction.IntexCallTrigger({windowDays: 0, thresholdDays: 0, intexCallPeriod: 0}),
            minIntexBidQuantity: 1,
            commitBondMinor: 0
        });
        vm.prank(address(bnbRouter));
        auction.auctionStart(WORLDWIDE_DAY, schedule, params);
    }

    function _seedSeriesOnIntex() internal {
        intex.createSeries(CreateSeriesLib.params(WORLDWIDE_DAY, ISSUED_INTEX_COUNT, 0));
    }

    function _deliver(bytes memory packet) internal {
        _deliver(OUTBE_CHAIN_ID, address(outbeRouter), address(bnbRouter), packet);
    }
}
