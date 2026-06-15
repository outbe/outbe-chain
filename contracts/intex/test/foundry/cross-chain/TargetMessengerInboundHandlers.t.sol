// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {Origin} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";

import {TargetMessenger} from "@contracts/bnb/TargetMessenger.sol";
import {OriginMessenger} from "@contracts/outbe/OriginMessenger.sol";
import {IntexAuction} from "@contracts/bnb/IntexAuction.sol";
import {IIntexAuction} from "@contracts/bnb/interfaces/IIntexAuction.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {EscrowAdapter} from "@contracts/bnb/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/bnb/interfaces/IEscrowAdapter.sol";
import {IVaultProvider} from "@contracts/vendor/outbe-vault/interfaces/IVaultProvider.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockERC20} from "@test-mocks/MockERC20.sol";
import {MockSettlementVault} from "@test-mocks/MockSettlementVault.sol";
import {MockVaultProvider} from "@test-mocks/MockVaultProvider.sol";

/// @dev End-to-end traversal of the five `TargetMessenger` inbound handlers that previously only
///      had codec-level round-trip coverage. Each test hand-builds a `BridgeMsgCodec` packet and
///      drives `lzReceive` from the endpoint address, then asserts the downstream side-effect on
///      the wired contract — proving the full _lzReceive -> dispatchInbound -> _handleX -> X path
///      under the current fail-don't-drop model.
contract TargetMessengerInboundHandlersTest is TestHelperOz5 {
    uint32 internal constant BNB_EID = 1;
    uint32 internal constant OUTBE_EID = 2;
    bytes32 internal constant GUID = bytes32(uint256(0xCAFE));

    uint32 internal constant SERIES_ID = 20250101;
    uint32 internal constant ISSUED_INTEX_COUNT = 100;
    uint128 internal constant PROMIS_LOAD_MINOR = 1000;
    uint64 internal constant STRIKE_PRICE = 100e6;
    uint64 internal constant FLOOR_PRICE_MINOR = 40e6;
    uint16 internal constant SETTLEMENT_TOKEN_ALIAS = 840;

    TargetMessenger internal bnbMessenger;
    OriginMessenger internal outbeMessenger;
    IntexAuction internal auction;
    IntexNFT1155 internal intex;
    EscrowAdapter internal escrow;
    ONFT1155AdapterBatch internal onftBatch;
    MockTheCompact internal compact;
    MockERC20 internal paymentToken;
    MockSettlementVault internal vault;
    MockVaultProvider internal provider;

    address internal admin = address(this);
    address internal bidder = address(0xB1);

    uint64 internal nonce = 1;

    function setUp() public override {
        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        intex = new IntexNFT1155(admin, admin);
        auction = new IntexAuction(admin, admin);

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
        onftBatch = new ONFT1155AdapterBatch(address(intex), address(endpoints[BNB_EID]), admin);

        escrow = new EscrowAdapter(admin, admin);
        compact = new MockTheCompact();
        paymentToken = new MockERC20("USD Coin", "USDC", 6);
        vault = new MockSettlementVault(address(paymentToken), "Mock Vault USDC", "mvUSDC", 6);
        provider = new MockVaultProvider();
        provider.addVault(vault);
        provider.addLiquiditySource(address(escrow), IVaultProvider.LiquiditySource.IntexBidPrice);
        escrow.wire(admin, address(compact), address(provider), address(paymentToken));
        compact.setResetPeriodSeconds(0);

        address[] memory bridge = new address[](2);
        bridge[0] = address(bnbMessenger);
        bridge[1] = address(outbeMessenger);
        this.wireOApps(bridge);

        bnbMessenger.wire(address(auction), address(intex), address(escrow), address(onftBatch));

        // The messenger drives auction lifecycle (RELAYER_ROLE), creates/mints IntexNFT1155
        // (RELAYER_ROLE), and finalizes escrow (RELAYER_ROLE). Each downstream contract gates
        // mutations on its own RELAYER_ROLE which only the wired messenger should hold.
        auction.grantRole(auction.RELAYER_ROLE(), address(bnbMessenger));
        intex.grantRole(intex.RELAYER_ROLE(), address(bnbMessenger));
        escrow.grantRole(escrow.RELAYER_ROLE(), address(bnbMessenger));

        // Bidder funds + approve so EscrowAdapter.lockFunds works in the REFUND_INSTRUCTIONS test.
        paymentToken.mint(bidder, 1e24);
        vm.prank(bidder);
        paymentToken.approve(address(escrow), type(uint256).max);
    }

    // --- _handleAuctionStageReveal: green-day signal -> auction stage flips to Revealing ---
    function test_handleAuctionStageReveal_advancesAuctionToRevealing() public {
        _seedAuction();

        bytes memory packet = BridgeMsgCodec.encodeAuctionStageReveal(SERIES_ID, true);
        _deliver(packet);

        assertEq(
            uint8(auction.getAuctionStage(SERIES_ID)),
            uint8(IIntexAuction.AuctionStage.RevealingBids),
            "auction advanced to RevealingBids"
        );
    }

    // --- _handleAuctionResult: clearing data is persisted on the auction ---
    function test_handleAuctionResult_persistsClearingOnAuction() public {
        _seedAuction();
        // Push the auction into Issuance stage so executeAuctionClearing is accepted.
        vm.prank(address(bnbMessenger));
        auction.startRevealingBidsStage(SERIES_ID, true);
        vm.prank(address(bnbMessenger));
        auction.startClearingStage(SERIES_ID);

        uint64 clearingPrice = 100e6;
        bytes memory packet = BridgeMsgCodec.encodeAuctionResult(SERIES_ID, ISSUED_INTEX_COUNT, clearingPrice, 0);
        _deliver(packet);

        IIntexAuction.AuctionResult memory result = auction.getAuctionInfo(SERIES_ID).result;
        assertEq(result.issuedIntexCount, ISSUED_INTEX_COUNT, "issuedIntexCount persisted");
        assertEq(result.auctionIntexClearingPrice, clearingPrice, "clearingPrice persisted");
    }

    // --- _handleIssuanceInstructions: createSeries + mintBatch on the local IntexNFT1155 ---
    function test_handleIssuanceInstructions_createsSeriesAndMints() public {
        address[] memory recipients = new address[](1);
        recipients[0] = bidder;
        uint256[] memory quantities = new uint256[](1);
        quantities[0] = 5;

        BridgeMsgCodec.IssuanceInstructionsPayload memory payload = BridgeMsgCodec.IssuanceInstructionsPayload({
            seriesId: SERIES_ID,
            issuedIntexCount: ISSUED_INTEX_COUNT,
            promisLoadMinor: PROMIS_LOAD_MINOR,
            costAmountMinor: STRIKE_PRICE,
            floorPriceMinor: FLOOR_PRICE_MINOR,
            intexCallPeriod: 0,
            settlementTokenAlias: SETTLEMENT_TOKEN_ALIAS,
            callWindowDays: 30,
            callThresholdDays: 5,
            coenPriceCallTrigger: 25e6,
            recipients: recipients,
            quantities: quantities
        });
        bytes memory packet = BridgeMsgCodec.encodeIssuanceInstructions(payload);
        _deliver(packet);

        uint256 tokenId = intex.issuedTokenId(SERIES_ID);
        assertEq(intex.balanceOf(bidder, tokenId), 5, "bidder credited 5 Issued tokens");
        assertEq(intex.totalSupply(tokenId), 5, "totalSupply == minted amount");
    }

    // --- _handleRefundInstructions: forwarded to EscrowAdapter.finalizeAuction; lock flips Finalized ---
    function test_handleRefundInstructions_finalizesEscrow() public {
        // Lock funds for the bidder so finalizeAuction's per-bidder branch can land.
        uint64 lockedAmount = 1000e6;
        vm.prank(admin);
        escrow.grantRole(escrow.AUCTION_ROLE(), admin);
        vm.prank(admin);
        escrow.lockFunds(SERIES_ID, bidder, lockedAmount);

        address[] memory bidders = new address[](1);
        bidders[0] = bidder;
        uint64[] memory refundedAmounts = new uint64[](1);
        refundedAmounts[0] = lockedAmount;
        uint64[] memory paidAmounts = new uint64[](1);
        paidAmounts[0] = 0;

        bytes memory packet = BridgeMsgCodec.encodeRefundInstructions(SERIES_ID, bidders, refundedAmounts, paidAmounts);
        _deliver(packet);

        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(SERIES_ID, bidder);
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
            promisLoadMinor: PROMIS_LOAD_MINOR,
            minIntexBidPrice: 60e6,
            costAmountMinor: STRIKE_PRICE,
            floorPriceMinor: FLOOR_PRICE_MINOR,
            minIntexBidQuantity: 1
        });
        vm.prank(address(bnbMessenger));
        auction.auctionStart(SERIES_ID, schedule, params);
    }

    function _seedSeriesOnIntex() internal {
        intex.createSeries(SERIES_ID, ISSUED_INTEX_COUNT, 0);
    }

    function _deliver(bytes memory packet) internal {
        Origin memory origin =
            Origin({srcEid: OUTBE_EID, sender: bytes32(uint256(uint160(address(outbeMessenger)))), nonce: nonce});
        nonce++;
        vm.prank(address(endpoints[BNB_EID]));
        bnbMessenger.lzReceive(origin, GUID, packet, address(0), "");
    }
}
