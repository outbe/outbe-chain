// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {ONFT1155Adapter} from "@contracts/shared/ONFT1155Adapter.sol";
import {SendParam} from "@contracts/shared/interfaces/IONFT1155Adapter.sol";
import {MessagingFee, MessagingReceipt} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";
import {OptionsBuilder} from "@layerzerolabs/oapp-evm/oapp/libs/OptionsBuilder.sol";
import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";

/// @dev Cross-chain conservation invariants for the IntexNFT1155 + ONFT1155Adapter pair:
///
///   - SI-08: `Σ totalSupply(issuedId)` across chains is never larger than the on-chain
///     `issuedIntexCount` cap of the underlying series. Mint+bridge+round-trip moves balances
///     between chains but cannot inflate the global pool.
///   - SI-09: a `debit` of `amount` on the source credits exactly `amount` on the destination,
///     even when the inbound credit fails: the parked-amount `failedCredits[guid].amount` holds
///     the in-flight units until retry, so the source-burned amount equals
///     `destination-credited + destination-parked` at every step.
contract CrossChainSupplyConservationTest is TestHelperOz5 {
    using OptionsBuilder for bytes;

    uint32 private constant A_EID = 1;
    uint32 private constant B_EID = 2;

    uint32 private constant SERIES_ID = 20260401;
    uint256 private constant TOKEN_ID = uint256(SERIES_ID);
    uint32 private constant ISSUED_INTEX_COUNT = 10_000;

    IntexNFT1155 private tokenA;
    IntexNFT1155 private tokenB;
    ONFT1155Adapter private adapterA;
    ONFT1155Adapter private adapterB;

    address private user = address(0x1);

    function setUp() public virtual override {
        vm.deal(user, 1000 ether);

        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        tokenA = new IntexNFT1155(address(this), address(this));
        tokenB = new IntexNFT1155(address(this), address(this));

        adapterA = ONFT1155Adapter(
            _deployOApp(
                type(ONFT1155Adapter).creationCode,
                abi.encode(address(tokenA), address(endpoints[A_EID]), address(this), B_EID)
            )
        );
        adapterB = ONFT1155Adapter(
            _deployOApp(
                type(ONFT1155Adapter).creationCode,
                abi.encode(address(tokenB), address(endpoints[B_EID]), address(this), A_EID)
            )
        );

        tokenA.grantRole(tokenA.RELAYER_ROLE(), address(adapterA));
        tokenB.grantRole(tokenB.RELAYER_ROLE(), address(adapterB));

        address[] memory oapps = new address[](2);
        oapps[0] = address(adapterA);
        oapps[1] = address(adapterB);
        this.wireOApps(oapps);

        tokenA.createSeries(SERIES_ID, ISSUED_INTEX_COUNT, 0);
        tokenB.createSeries(SERIES_ID, ISSUED_INTEX_COUNT, 0);

        tokenA.markQualified(SERIES_ID);
        tokenB.markQualified(SERIES_ID);
    }

    function test_HopAToB_TotalSupplyPreservedAndBelowCap() public {
        uint256 minted = 100;
        tokenA.mint(user, minted, SERIES_ID);

        uint256 bridged = 60;
        _send(adapterA, B_EID, user, TOKEN_ID, bridged);

        // Source debits exactly `bridged`; destination credits exactly `bridged`.
        assertEq(tokenA.totalSupply(TOKEN_ID), minted - bridged, "A.totalSupply -= bridged");
        assertEq(tokenB.totalSupply(TOKEN_ID), bridged, "B.totalSupply += bridged");

        // SI-08: the global pool stays within the issuance cap and equals the original mint.
        uint256 totalAcrossChains = tokenA.totalSupply(TOKEN_ID) + tokenB.totalSupply(TOKEN_ID);
        assertEq(totalAcrossChains, minted, "SI-08: sum preserved");
        assertLe(totalAcrossChains, ISSUED_INTEX_COUNT, "SI-08: sum <= issuedIntexCount");
    }

    function test_RoundTripAToBToA_TotalSupplyPreserved() public {
        uint256 minted = 100;
        tokenA.mint(user, minted, SERIES_ID);

        _send(adapterA, B_EID, user, TOKEN_ID, minted);
        assertEq(tokenA.totalSupply(TOKEN_ID), 0, "A drained after outbound");
        assertEq(tokenB.totalSupply(TOKEN_ID), minted, "B holds the bridged units");

        _send(adapterB, A_EID, user, TOKEN_ID, minted);
        assertEq(tokenA.totalSupply(TOKEN_ID), minted, "A restored after return");
        assertEq(tokenB.totalSupply(TOKEN_ID), 0, "B drained after return");

        uint256 totalAcrossChains = tokenA.totalSupply(TOKEN_ID) + tokenB.totalSupply(TOKEN_ID);
        assertEq(totalAcrossChains, minted, "SI-08: sum preserved end-to-end");
        assertLe(totalAcrossChains, ISSUED_INTEX_COUNT, "SI-08: sum <= issuedIntexCount");
    }

    function test_ParkBranch_ConservesAcrossDebitAndPark() public {
        // Pick a fresh series that exists on A but not on B: the inbound credit reverts
        // NonexistentToken and the transfer parks on B. Tokens are burned on A but not yet
        // minted on B — the missing amount lives in failedCredits.
        uint32 parkSeries = 20260601;
        uint256 parkTokenId = uint256(parkSeries);
        tokenA.createSeries(parkSeries, ISSUED_INTEX_COUNT, 0);
        tokenA.markQualified(parkSeries);

        uint256 minted = 100;
        uint256 bridged = 100;
        tokenA.mint(user, minted, parkSeries);

        MessagingReceipt memory r = _send(adapterA, B_EID, user, parkTokenId, bridged);

        // Source-side: the source intex burned the bridged amount; the cap-respecting supply on A
        // is the remainder.
        assertEq(tokenA.totalSupply(parkTokenId), minted - bridged, "A.totalSupply -= bridged");
        assertEq(tokenB.totalSupply(parkTokenId), 0, "B not credited (series missing)");

        // Park entry holds the in-flight amount, so the global accounting still adds up.
        (,, uint256 parkedAmount,,, bool exists) = adapterB.failedCredits(r.guid);
        assertTrue(exists, "park entry present");
        assertEq(parkedAmount, bridged, "park amount == bridged");

        uint256 total = tokenA.totalSupply(parkTokenId) + tokenB.totalSupply(parkTokenId) + parkedAmount;
        assertEq(total, minted, "SI-09: source-debited == dest-credited + dest-parked");

        // Fix the destination cause and retry — parked moves into B.totalSupply with no
        // change to the global sum.
        tokenB.createSeries(parkSeries, ISSUED_INTEX_COUNT, 0);
        tokenB.markQualified(parkSeries);
        adapterB.retryCredit(r.guid);

        assertEq(tokenB.totalSupply(parkTokenId), bridged, "B.totalSupply == bridged after retry");
        (,,,,, bool stillExists) = adapterB.failedCredits(r.guid);
        assertFalse(stillExists, "park entry cleared on retry");

        uint256 totalAfterRetry = tokenA.totalSupply(parkTokenId) + tokenB.totalSupply(parkTokenId);
        assertEq(totalAfterRetry, minted, "SI-09: sum preserved after retry");
    }

    function testFuzz_Hop_TotalSupplyAlwaysAtCap(uint256 mintedSeed, uint256 bridgedSeed) public {
        uint256 minted = bound(mintedSeed, 1, ISSUED_INTEX_COUNT);
        uint256 bridged = bound(bridgedSeed, 0, minted);

        tokenA.mint(user, minted, SERIES_ID);
        if (bridged > 0) {
            _send(adapterA, B_EID, user, TOKEN_ID, bridged);
        }

        uint256 totalAcrossChains = tokenA.totalSupply(TOKEN_ID) + tokenB.totalSupply(TOKEN_ID);
        assertEq(totalAcrossChains, minted, "SI-08: sum preserved");
        assertLe(totalAcrossChains, ISSUED_INTEX_COUNT, "SI-08: sum <= issuedIntexCount");
    }

    function _send(
        ONFT1155Adapter from,
        uint32 dstEid,
        address recipient,
        uint256 tokenId,
        uint256 amount
    ) internal returns (MessagingReceipt memory) {
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(400000, 0);
        SendParam memory params = SendParam({
            dstEid: dstEid,
            to: bytes32(uint256(uint160(recipient))),
            tokenId: tokenId,
            amount: amount,
            extraOptions: options,
            composeMsg: ""
        });
        MessagingFee memory fee = from.quoteSend(params, false);
        vm.prank(recipient);
        MessagingReceipt memory r = from.send{value: fee.nativeFee}(params, fee, recipient);
        ONFT1155Adapter dest = (dstEid == B_EID) ? adapterB : adapterA;
        verifyPackets(dstEid, bytes32(uint256(uint160(address(dest)))));
        return r;
    }
}
