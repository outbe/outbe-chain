// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IntexGas} from "@contracts/shared/libs/IntexGas.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {ReferenceCurrencyPriceLib} from "../helpers/ReferenceCurrencyPriceLib.sol";
import {IssuanceBatchLib} from "../helpers/IssuanceBatch.sol";
import {Test} from "forge-std/Test.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {DESIS} from "@precompiles/IDesis.sol";
import {IIntexFactory, INTEX_FACTORY} from "@precompiles/IIntexFactory.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MockERC7786Bridge} from "@test-mocks/MockERC7786Bridge.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";

uint16 constant ISSUANCE_CCY = 840;
uint16 constant REFERENCE_CCY = 840;

/// @dev The WCOEN token bridge on Outbe. Like the real one, it has no peer on its own chain, so a
///      route through it to this chain reverts.
contract LocalTokenBridge {
    error RemoteBridgeNotSet(uint32 domain);

    IERC20 internal immutable TOKEN;

    constructor(IERC20 token) {
        TOKEN = token;
    }

    function token() external view returns (IERC20) {
        return TOKEN;
    }

    function quoteSend(uint32 domain, address, uint256, bytes calldata, uint256) external pure returns (uint256) {
        revert RemoteBridgeNotSet(domain);
    }

    function sendAndCall(uint32 domain, address, uint256, bytes calldata, uint256) external payable returns (bytes32) {
        revert RemoteBridgeNotSet(domain);
    }
}

/// @dev Outbe as its own target. The Interchain Account of Outbe hands every message to TargetRouter
///      under its IntexGas budget, and the bids relay that fires from inside the CLEARING delivery
///      reaches Desis through a synchronous in-process bridge under its executionGasLimit attribute.
///      An undersized budget makes a delivery run out of gas and fails the walk. Thus the test is
///      also the budget check.
contract LocalLoopbackTest is Test {
    uint32 internal constant DAY = 20260714;
    uint128 internal constant PROMIS_LOAD_MINOR = 1e6;
    uint256 internal constant INITIAL_WCOEN_BALANCE = 100e18;

    bytes32 internal constant REVEAL_BID_TYPEHASH = keccak256(
        "RevealBid(uint32 emissionDay,address bidder,uint16 quantity,uint32 bidRate,uint16 issuanceCurrency,uint16 referenceCurrency)"
    );

    MockERC7786Bridge internal bridge;
    TargetRouter internal target;
    IntexAuction internal auction;
    EscrowAdapter internal escrow;
    IntexNFT1155 internal intex;
    MockWCOEN internal wcoen;
    LocalTokenBridge internal tokenBridge;
    MockTheCompact internal compact;
    address internal outbeIca = makeAddr("outbeIca");

    uint32 internal local;
    uint256 internal startTs;

    uint256 internal iba1Pk = 0x100;
    uint256 internal iba2Pk = 0x200;
    address internal iba1;
    address internal iba2;

    function setUp() public {
        vm.warp(1_760_000_000);
        startTs = block.timestamp;
        local = uint32(block.chainid);
        iba1 = vm.addr(iba1Pk);
        iba2 = vm.addr(iba2Pk);

        // Synchronous delivery under the exact per-message gas attribute.
        bridge = new MockERC7786Bridge();
        bridge.setEnforceGasAttribute(true);
        vm.etch(DESIS, address(new MockDesis()).code);
        vm.etch(INTEX_FACTORY, hex"00");
        vm.mockCall(INTEX_FACTORY, abi.encodeWithSelector(IIntexFactory.receiveLocalProceeds.selector), "");

        wcoen = new MockWCOEN();
        tokenBridge = new LocalTokenBridge(IERC20(address(wcoen)));
        intex = DeployProxy.intexNFT1155(address(this), address(this));
        auction = DeployProxy.intexAuction(address(this), address(this));
        target = DeployProxy.targetRouter(address(bridge), address(this), local, address(tokenBridge));
        target.grantRole(target.OUTBE_ICA_ROLE(), outbeIca);

        escrow = DeployProxy.escrowAdapter(address(this), address(this));
        compact = new MockTheCompact();
        escrow.setDependencies(address(auction), address(compact), address(wcoen));
        escrow.setProceedsRecipient(address(target));
        compact.setResetPeriodSeconds(0);

        target.setDependencies(address(auction), address(intex), address(escrow));

        auction.setEscrow(address(escrow));
        auction.setIntexContract(address(intex));
        intex.grantRole(intex.AUCTION_ROLE(), address(auction));
        auction.grantRole(auction.RELAYER_ROLE(), address(target));
        intex.grantRole(intex.ROUTER_ROLE(), address(target));
        escrow.grantRole(escrow.RELAYER_ROLE(), address(target));

        wcoen.mint(iba1, INITIAL_WCOEN_BALANCE);
        wcoen.mint(iba2, INITIAL_WCOEN_BALANCE);
        vm.prank(iba1);
        wcoen.approve(address(escrow), type(uint256).max);
        vm.prank(iba2);
        wcoen.approve(address(escrow), type(uint256).max);
    }

    /// @dev The Interchain Account of Outbe hands `message` over under `gasLimit`.
    function _fromOutbe(bytes memory message, uint256 gasLimit) internal {
        vm.prank(outbeIca);
        target.receiveFromOutbe{gas: gasLimit}(message);
    }

    function _start() internal view returns (bytes memory) {
        return BridgeMsgCodec.encodeIntexAuctionStart(
            DAY,
            uint32(startTs + 100),
            uint32(startTs + 200),
            uint32(startTs + 300),
            PROMIS_LOAD_MINOR,
            600_000,
            ReferenceCurrencyPriceLib.one(840, 1e6, 200),
            0,
            0,
            0,
            1,
            0
        );
    }

    function _sig(address bidder, uint16 qty, uint32 rate, uint256 pk) internal view returns (bytes memory) {
        bytes32 structHash =
            keccak256(abi.encode(REVEAL_BID_TYPEHASH, DAY, bidder, qty, rate, ISSUANCE_CCY, REFERENCE_CCY));
        bytes32 domainSeparator = keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"),
                keccak256(bytes("IntexAuction")),
                keccak256(bytes("1")),
                block.chainid,
                address(auction)
            )
        );
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(pk, keccak256(abi.encodePacked("\x19\x01", domainSeparator, structHash)));
        return abi.encodePacked(r, s, v);
    }

    function _commitAndReveal(address bidder, uint16 qty, uint32 rate, uint256 pk) internal {
        bytes memory signature = _sig(bidder, qty, rate, pk);
        vm.prank(bidder);
        auction.revealIntexAuctionBid(DAY, qty, rate, ISSUANCE_CCY, REFERENCE_CCY, uint64(block.chainid), signature);
    }

    function test_FullWalk_OutbeAsTarget() public {
        // 1. START opens the day on the local auction.
        _fromOutbe(_start(), IntexGas.intexAuctionStart(1));
        assertEq(uint8(auction.auctionStage(DAY)), uint8(IIntexAuction.AuctionStage.CommittingBids), "not started");

        // 2. Commits during the commit window.
        vm.prank(iba1);
        auction.commitIntexAuctionBid(DAY, keccak256(_sig(iba1, 30, 800_000, iba1Pk)));
        vm.prank(iba2);
        auction.commitIntexAuctionBid(DAY, keccak256(_sig(iba2, 40, 700_000, iba2Pk)));

        // 3. Past commitEnd, the stage computes to RevealingBids. Reveals lock escrow
        //    (qty * load * rate / 1e6), converted to 18-decimal WCOEN.
        vm.warp(startTs + 101);
        assertEq(uint8(auction.auctionStage(DAY)), uint8(IIntexAuction.AuctionStage.RevealingBids), "not revealing");
        _commitAndReveal(iba1, 30, 800_000, iba1Pk);
        _commitAndReveal(iba2, 40, 700_000, iba2Pk);
        assertEq(uint256(escrow.getIntexAuctionBidLock(DAY, iba1).lockedAmount), 24e18, "iba1 lock");
        assertEq(uint256(escrow.getIntexAuctionBidLock(DAY, iba2).lockedAmount), 28e18, "iba2 lock");

        // 4. CLEARING: the delivery itself fires the nested bids relay (BIDS_BATCH + BIDS_TOTALS)
        //    through the bridge to Desis on this chain.
        vm.warp(startTs + 201);
        _fromOutbe(BridgeMsgCodec.encodeIntexAuctionClearingStart(DAY), IntexGas.INTEX_AUCTION_CLEARING_START);
        MockDesis desis = MockDesis(DESIS);
        assertEq(desis.received(), 2, "bids not relayed");
        assertEq(desis.lastSender(), InteroperableAddress.formatEvmV1(local, address(target)), "relay sender");
        assertEq(desis.lastPayloadHash(), keccak256(BridgeMsgCodec.encodeBidsTotals(DAY, local, 1, 2)), "marker");

        // 5. INTEX_AUCTION_RESULT: supply 50 -> iba1 wins 30, iba2 wins 20 at the uniform rate 700k.
        BridgeMsgCodec.IntexAuctionChainResult memory r = BridgeMsgCodec.IntexAuctionChainResult({
            wonQuantity: 0, wonBidsCount: 2, quantity: 50, clearingRate: 700_000, totalDemand: 0
        });
        _fromOutbe(BridgeMsgCodec.encodeIntexAuctionResult(DAY, r), IntexGas.INTEX_AUCTION_RESULT);
        IIntexAuction.IntexAuctionResult memory result = auction.getAuctionInfo(DAY).result;
        assertEq(result.issuedUnits, 50, "issued");
        assertEq(result.clearingRate, 700_000, "clearing rate");

        // 6. REFUND_INSTRUCTIONS: the delivery takes each winner's payment AND hands the paid WCOEN
        //    to IntexFactory on this chain, all inside the refund budget. The rest of each lock
        //    waits for its claim. iba2 is the partial fill: 20 of 40.
        address[] memory bidders = new address[](2);
        bidders[0] = iba1;
        bidders[1] = iba2;
        vm.expectCall(INTEX_FACTORY, abi.encodeCall(IIntexFactory.receiveLocalProceeds, (DAY, 0, 35e18)), 1);
        _fromOutbe(
            BridgeMsgCodec.encodeIntexAuctionWinners(DAY, 0, 1, 700_000, PROMIS_LOAD_MINOR, bidders, 1, 20),
            IntexGas.assetReturn(2, true)
        );

        assertEq(
            uint8(escrow.getIntexAuctionBidLock(DAY, iba1).status), uint8(IEscrowAdapter.LockStatus.Won), "iba1 not won"
        );
        assertEq(escrow.getIntexAuctionBidLock(DAY, iba2).quantity, 20, "iba2 partial");
        auction.returnIntexAuctionAsset(DAY, iba1);
        auction.returnIntexAuctionAsset(DAY, iba2);
        assertEq(wcoen.balanceOf(iba1), INITIAL_WCOEN_BALANCE - 24e18 + 3e18, "iba1 refund"); // paid 30*1*0.7
        assertEq(wcoen.balanceOf(iba2), INITIAL_WCOEN_BALANCE - 28e18 + 14e18, "iba2 refund"); // paid 20*1*0.7
        assertEq(wcoen.allowance(address(target), INTEX_FACTORY), 35e18, "IntexFactory may pull the proceeds");

        // 7. ISSUANCE_INSTRUCTIONS: the series lands on the canonical NFT.
        BridgeMsgCodec.IntexSeriesPayload memory series = BridgeMsgCodec.IntexSeriesPayload({
            seriesId: CreateSeriesLib.seriesId(DAY),
            emissionDay: DAY,
            issuedAt: uint32(block.timestamp),
            issuedUnits: 50,
            promisLoadMinor: PROMIS_LOAD_MINOR,
            entryPriceMinor: 1e6,
            callNoticePeriod: 0,
            issuanceCurrency: 840,
            referenceCurrency: 840,
            callWindow: 30,
            callThreshold: 21,
            callPriceMinor: 200,
            transitMargin: 1 hours
        });
        _fromOutbe(BridgeMsgCodec.encodeIntexSeries(DAY, 0, 1, IssuanceBatchLib.one(series)), IntexGas.issuance(1));
        assertTrue(intex.seriesExists(CreateSeriesLib.seriesId(DAY)), "series created");

        // Each winner issues its own units through the auction.
        auction.redeemWinNote(DAY, iba1);
        auction.redeemWinNote(DAY, iba2);
        uint256 tokenId = intex.issuedTokenId(CreateSeriesLib.seriesId(DAY));
        assertEq(intex.balanceOf(iba1, tokenId), 30, "iba1 issued");
        assertEq(intex.balanceOf(iba2, tokenId), 20, "iba2 issued");

        // 8. Every leg executed within its IntexGas budget: nothing parked anywhere.
        (,, bool bidsRelayed) = target.getIntexAuctionBidsSendState(DAY);
        assertTrue(bidsRelayed, "bids relay finished");
        (,,, bool proceedsParked,) = target.getFailedIntexAuctionProceedsSend(0);
        assertFalse(proceedsParked, "proceeds route parked");
    }
}
