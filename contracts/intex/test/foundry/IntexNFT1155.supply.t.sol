// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {CreateSeriesLib} from "./helpers/CreateSeriesLib.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {IAccessControl} from "@openzeppelin/contracts/access/IAccessControl.sol";
import {Test} from "forge-std/Test.sol";

/// @title — supply cap, burnSettled state gate, paginated expireSeries and holders getter.
/// @notice Every test here exercises a behavior introduced by the lifecycle/DoS hardening pass.
contract IntexNFT1155SupplyTest is Test {
    IntexNFT1155 nft;

    address admin = address(0xA11CE);
    address bridger = address(0xB81DE);
    address settler = address(0x5E771E);
    address promis = address(0x7307);
    address holderA = address(0xA);
    address holderB = address(0xB);

    uint32 constant SERIES_ID = 20260401;
    uint256 constant TOKEN_ID = uint256(SERIES_ID);

    uint32 constant CALL_PERIOD = uint32(1 days);

    function setUp() public {
        nft = DeployProxy.intexNFT1155(admin, bridger);
        vm.startPrank(admin);
        nft.grantRole(nft.SETTLEMENT_ROLE(), settler);
        nft.grantRole(nft.PROMIS_ROLE(), promis);
        vm.stopPrank();
    }

    function _createSeries(uint32 cap) internal {
        vm.prank(bridger);
        nft.createSeries(CreateSeriesLib.params(SERIES_ID, cap, CALL_PERIOD));
    }

    // --- Supply cap: createSeries / mint ---

    function test_CreateSeries_ZeroIssuedCount_Reverts() public {
        vm.prank(bridger);
        vm.expectRevert(IIntexNFT1155.ZeroIssuedIntexCount.selector);
        nft.createSeries(CreateSeriesLib.params(SERIES_ID, 0, CALL_PERIOD));
    }

    function test_Mint_AtCap_Succeeds() public {
        uint32 cap = 100;
        _createSeries(cap);

        vm.prank(bridger);
        nft.mint(holderA, cap, SERIES_ID);

        assertEq(nft.totalSupply(TOKEN_ID), cap);
        assertEq(nft.readData(SERIES_ID).issuedIntexCount, cap);
        assertEq(nft.balanceOf(holderA, TOKEN_ID), cap);
    }

    function test_Mint_OverCap_Reverts() public {
        uint32 cap = 100;
        _createSeries(cap);

        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.SupplyCapExceeded.selector, SERIES_ID, cap + 1, cap));
        nft.mint(holderA, cap + 1, SERIES_ID);
    }

    function test_Mint_OneOverAfterPartial_Reverts() public {
        uint32 cap = 100;
        _createSeries(cap);

        vm.startPrank(bridger);
        nft.mint(holderA, 60, SERIES_ID);
        // 60 + 41 = 101 > 100 → reverts with the post-increment attempted total.
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.SupplyCapExceeded.selector, SERIES_ID, cap + 1, cap));
        nft.mint(holderA, 41, SERIES_ID);
        vm.stopPrank();
    }

    // --- burnSettled state gate (state ∈ {Qualified, Called}) ---

    function _mintAndSettle(uint32 cap, uint256 mintAmount, uint256 settleAmount, bool callBeforeSettle) internal {
        _createSeries(cap);
        vm.prank(bridger);
        nft.mint(holderA, mintAmount, SERIES_ID);
        if (callBeforeSettle) {
            vm.prank(bridger);
            nft.markCalled(SERIES_ID);
        } else {
            vm.prank(bridger);
            nft.markQualified(SERIES_ID);
        }
        vm.prank(settler);
        nft.settle(SERIES_ID, holderA, holderA, settleAmount);
    }

    function test_BurnSettled_OnIssuedState_Reverts() public {
        // Stage a Settled balance that exists despite the series sitting in Issued state. The
        // production flow can't reach this configuration today, but the gate is the precondition
        // we want to test — a future change (e.g. airdropping Settled) must not silently unlock
        // burnSettled while the series is still in Issued.
        _createSeries(10);
        // Use the storage slot directly to forge a Settled balance under an Issued series.
        uint256 sTok = nft.settledTokenId(SERIES_ID);
        // Pre-state: Issued, no Settled balances. Calling burnSettled here must hit the typed
        // state gate, not the ERC1155 zero-balance revert.
        vm.prank(promis);
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexNFT1155.InvalidState.selector,
                uint8(IIntexNFT1155.IntexState.Qualified),
                uint8(IIntexNFT1155.IntexState.Issued)
            )
        );
        nft.burnSettled(holderA, SERIES_ID, 1);
        // Silence unused-var linter on the helper.
        sTok;
    }

    function test_BurnSettled_OnQualifiedState_Succeeds() public {
        _mintAndSettle({cap: 10, mintAmount: 6, settleAmount: 4, callBeforeSettle: false});
        // Series is in Qualified, holder has 4 Settled.
        vm.prank(promis);
        nft.burnSettled(holderA, SERIES_ID, 3);
        assertEq(nft.balanceOf(holderA, nft.settledTokenId(SERIES_ID)), 1);
    }

    function test_BurnSettled_OnCalledState_Succeeds() public {
        _mintAndSettle({cap: 10, mintAmount: 6, settleAmount: 4, callBeforeSettle: true});
        // Series is in Called, holder has 4 Settled.
        vm.prank(promis);
        nft.burnSettled(holderA, SERIES_ID, 4);
        assertEq(nft.balanceOf(holderA, nft.settledTokenId(SERIES_ID)), 0);
    }

    // --- ZeroAmount: split out of the former overloaded EmptyArray (one error = one failure) ---

    function test_Settle_ZeroAmount_Reverts() public {
        _createSeries(10);
        vm.prank(bridger);
        nft.mint(holderA, 5, SERIES_ID);
        // amount == 0 is rejected before any series-state work.
        vm.prank(settler);
        vm.expectRevert(IIntexNFT1155.ZeroAmount.selector);
        nft.settle(SERIES_ID, holderA, holderA, 0);
    }

    function test_BurnSettled_ZeroAmount_Reverts() public {
        _mintAndSettle({cap: 10, mintAmount: 6, settleAmount: 4, callBeforeSettle: false});
        vm.prank(promis);
        vm.expectRevert(IIntexNFT1155.ZeroAmount.selector);
        nft.burnSettled(holderA, SERIES_ID, 0);
    }

    // --- expireSeries pagination (stateless, sweeps from index 0 every page) ---

    function _seedHolders(uint32 cap, uint256 count, uint256 perHolder) internal returns (address[] memory holders) {
        _createSeries(cap);
        holders = new address[](count);
        vm.startPrank(bridger);
        for (uint256 i = 0; i < count; i++) {
            // Synthesize a non-zero address that ERC1155 accepts.
            address h = address(uint160(0x1000 + i));
            holders[i] = h;
            nft.mint(h, perHolder, SERIES_ID);
        }
        nft.markCalled(SERIES_ID);
        vm.stopPrank();
        vm.warp(block.timestamp + CALL_PERIOD + 1);
    }

    function test_ExpireSeries_ZeroLimit_Reverts() public {
        _seedHolders(100, 3, 5);
        vm.prank(bridger);
        vm.expectRevert(IIntexNFT1155.ZeroLimit.selector);
        nft.expireSeries(SERIES_ID, 0);
    }

    function test_R03_ExpireSeries_RequiresRelayerRole() public {
        // Per docs/nft/lifecycle.md and the audit, expireSeries must be gated by RELAYER_ROLE.
        // Pre-fix it is permissionless — any address can mass-burn balances for any series
        // past its deadline.
        _seedHolders(50, 3, 5);

        address rando = address(0xBAD);
        bytes32 relayerRole = nft.RELAYER_ROLE();
        vm.prank(rando);
        vm.expectRevert(
            abi.encodeWithSelector(IAccessControl.AccessControlUnauthorizedAccount.selector, rando, relayerRole)
        );
        nft.expireSeries(SERIES_ID, type(uint256).max);
    }

    function test_ExpireSeries_BeforeDeadline_Reverts() public {
        _createSeries(10);
        vm.startPrank(bridger);
        nft.mint(holderA, 5, SERIES_ID);
        nft.markCalled(SERIES_ID);
        // No warp — still inside the call period: not-yet-expired, not the idempotent no-op.
        IIntexNFT1155.SeriesData memory d = nft.readData(SERIES_ID);
        uint32 derivedDeadline = d.calledAt + d.callTrigger.intexCallPeriod;
        vm.expectRevert(
            abi.encodeWithSelector(IIntexNFT1155.SeriesNotYetExpired.selector, derivedDeadline, uint32(block.timestamp))
        );
        nft.expireSeries(SERIES_ID, 100);
        vm.stopPrank();
    }

    function test_ExpireSeries_NotCalled_Reverts() public {
        _createSeries(10);
        vm.warp(block.timestamp + 365 days);
        vm.prank(bridger);
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexNFT1155.InvalidState.selector,
                uint8(IIntexNFT1155.IntexState.Called),
                uint8(IIntexNFT1155.IntexState.Issued)
            )
        );
        nft.expireSeries(SERIES_ID, 100);
    }

    function test_ExpireSeries_FinalPageEmitsSeriesExpired() public {
        _seedHolders(20, 3, 5);

        vm.expectEmit(true, true, false, true);
        emit IIntexNFT1155.SeriesExpired(TOKEN_ID, bridger);
        vm.prank(bridger);
        nft.expireSeries(SERIES_ID, type(uint256).max);

        assertEq(nft.totalSupply(TOKEN_ID), 0);
        assertEq(nft.seriesHolderCount(TOKEN_ID), 0);
        // State stays Called.
        assertEq(uint8(nft.readData(SERIES_ID).state), uint8(IIntexNFT1155.IntexState.Called));
    }

    function test_ExpireSeries_MidPageEmitsProgress() public {
        _seedHolders(50, 5, 3);

        vm.expectEmit(true, false, false, true);
        emit IIntexNFT1155.SeriesExpiredProgress(SERIES_ID, 2);
        vm.prank(bridger);
        nft.expireSeries(SERIES_ID, 2);

        assertEq(nft.seriesHolderCount(TOKEN_ID), 3, "two holders swept this page");
        assertEq(nft.totalSupply(TOKEN_ID), 3 * 3);
    }

    function test_ExpireSeries_DrainsAcrossMultiplePages() public {
        uint256 holderCount = 10;
        uint256 perHolder = 4;
        _seedHolders(uint32(holderCount * perHolder), holderCount, perHolder);

        // Page 1: 4 holders → progress (6 remaining).
        vm.expectEmit(true, false, false, true);
        emit IIntexNFT1155.SeriesExpiredProgress(SERIES_ID, 4);
        vm.prank(bridger);
        nft.expireSeries(SERIES_ID, 4);
        assertEq(nft.seriesHolderCount(TOKEN_ID), 6);

        // Page 2: 4 holders → progress (2 remaining).
        vm.expectEmit(true, false, false, true);
        emit IIntexNFT1155.SeriesExpiredProgress(SERIES_ID, 4);
        vm.prank(bridger);
        nft.expireSeries(SERIES_ID, 4);
        assertEq(nft.seriesHolderCount(TOKEN_ID), 2);

        // Page 3: 4 requested, 2 actually swept → final-page emits SeriesExpired.
        vm.expectEmit(true, true, false, true);
        emit IIntexNFT1155.SeriesExpired(TOKEN_ID, bridger);
        vm.prank(bridger);
        nft.expireSeries(SERIES_ID, 4);

        assertEq(nft.seriesHolderCount(TOKEN_ID), 0);
        assertEq(nft.totalSupply(TOKEN_ID), 0);
    }

    function test_ExpireSeries_AfterFullSweep_Reverts() public {
        _seedHolders(15, 3, 5);
        vm.prank(bridger);
        nft.expireSeries(SERIES_ID, type(uint256).max);

        // Idempotency: with totalSupply == 0, the next call reverts NothingToExpire (not
        // SeriesNotYetExpired — the deadline has passed; there is simply nothing left to sweep).
        vm.prank(bridger);
        vm.expectRevert(IIntexNFT1155.NothingToExpire.selector);
        nft.expireSeries(SERIES_ID, type(uint256).max);
    }

    function test_ExpireSeries_PreservesSettledBalances() public {
        // A holder that already settled some keeps the Settled balance through expiration.
        _createSeries(10);
        vm.startPrank(bridger);
        nft.mint(holderA, 10, SERIES_ID);
        nft.markCalled(SERIES_ID);
        vm.stopPrank();
        vm.prank(settler);
        nft.settle(SERIES_ID, holderA, holderA, 4);

        vm.warp(block.timestamp + CALL_PERIOD + 1);
        vm.prank(bridger);
        nft.expireSeries(SERIES_ID, type(uint256).max);

        (uint256 issued, uint256 settled) = nft.tokenIds(SERIES_ID);
        assertEq(nft.balanceOf(holderA, issued), 0);
        assertEq(nft.balanceOf(holderA, settled), 4);
        assertEq(nft.totalSupply(issued), 0);
        assertEq(nft.totalSupply(settled), 4);
    }

    // --- Live-supply cap (a burn frees cap room; cap is `totalSupply ≤ issuedIntexCount`) ---

    function test_Cap_Mint_AfterSettle_FreesCapRoom() public {
        // Mint to cap, settle (burns 4 Issued → totalSupply 6): the freed room is reusable, so a
        // mint of 4 succeeds back up to the cap, and only the unit past the cap reverts.
        uint32 cap = 10;
        _createSeries(cap);

        vm.startPrank(bridger);
        nft.mint(holderA, cap, SERIES_ID);
        nft.markQualified(SERIES_ID);
        vm.stopPrank();

        vm.prank(settler);
        nft.settle(SERIES_ID, holderA, holderA, 4);
        assertEq(nft.readData(SERIES_ID).totalSupply, 6, "settle burns Issued, freeing cap room");

        // The 4 units freed by settle can be re-minted.
        vm.prank(bridger);
        nft.mint(holderA, 4, SERIES_ID);
        assertEq(nft.readData(SERIES_ID).totalSupply, cap, "re-mint refills freed room up to cap");

        // One more overshoots the cap.
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.SupplyCapExceeded.selector, SERIES_ID, cap + 1, cap));
        nft.mint(holderA, 1, SERIES_ID);
    }

    function test_Cap_CrosschainMint_AtCap_Reverts() public {
        // After totalSupply reaches the cap (via mint), crosschainMint must reject any further
        // incoming supply — the live-totalSupply invariant is `totalSupply ≤ cap` at all times.
        uint32 cap = 10;
        _createSeries(cap);

        vm.startPrank(bridger);
        nft.mint(holderA, cap, SERIES_ID);
        nft.markQualified(SERIES_ID);

        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.SupplyCapExceeded.selector, SERIES_ID, cap + 1, cap));
        nft.crosschainMint(holderB, TOKEN_ID, 1);
        vm.stopPrank();
    }

    function test_Cap_CrosschainMint_AfterCrosschainBurn_RefillsCapRoom() public {
        // Cross-chain return: tokens bridged out (crosschainBurn) come back (crosschainMint). The crosschainMint cap is
        // per-instant `totalSupply ≤ cap`, so the room cleared by crosschainBurn may be refilled by crosschainMint.
        uint32 cap = 10;
        _createSeries(cap);

        vm.startPrank(bridger);
        nft.mint(holderA, cap, SERIES_ID);
        nft.markQualified(SERIES_ID);
        nft.crosschainBurn(holderA, TOKEN_ID, 4);
        nft.crosschainMint(holderB, TOKEN_ID, 4);
        vm.stopPrank();

        assertEq(nft.totalSupply(TOKEN_ID), cap);
        assertEq(nft.balanceOf(holderA, TOKEN_ID), 6);
        assertEq(nft.balanceOf(holderB, TOKEN_ID), 4);
        assertEq(nft.readData(SERIES_ID).totalSupply, cap, "totalSupply back at cap after refill");
    }

    function test_Cap_Mint_AfterExpireSeries_FreesCapRoom() public {
        // expireSeries drains totalSupply to 0, freeing the full cap; a subsequent mint draws
        // against the live (now-zero) supply rather than a cumulative counter.
        uint32 cap = 10;
        _createSeries(cap);

        vm.startPrank(bridger);
        nft.mint(holderA, cap, SERIES_ID);
        nft.markCalled(SERIES_ID);
        vm.stopPrank();

        vm.warp(block.timestamp + CALL_PERIOD + 1);
        // Permissioned per R-03 — caller pranks as bridger to satisfy the role.
        vm.prank(bridger);
        nft.expireSeries(SERIES_ID, type(uint256).max);

        assertEq(nft.totalSupply(TOKEN_ID), 0);
        vm.prank(bridger);
        nft.mint(holderA, 1, SERIES_ID);
        assertEq(nft.readData(SERIES_ID).totalSupply, 1, "mint draws against the freed cap");
    }

    function test_Cap_TotalSupply_TracksLiveIssuedBalance() public {
        uint32 cap = 10;
        _createSeries(cap);
        assertEq(nft.readData(SERIES_ID).totalSupply, 0);

        vm.startPrank(bridger);
        nft.mint(holderA, 3, SERIES_ID);
        assertEq(nft.readData(SERIES_ID).totalSupply, 3);

        nft.mint(holderB, 4, SERIES_ID);
        assertEq(nft.readData(SERIES_ID).totalSupply, 7);

        nft.markQualified(SERIES_ID);
        // settle burns Issued from holderA — live totalSupply decreases, freeing cap room.
        vm.stopPrank();
        vm.prank(settler);
        nft.settle(SERIES_ID, holderA, holderA, 2);
        assertEq(nft.totalSupply(TOKEN_ID), 5, "settle burns Issued (totalSupply 7 - 2)");
        assertEq(nft.readData(SERIES_ID).totalSupply, 5, "SeriesData mirror tracks live Issued supply");
    }

    function test_Cap_Mint_OverCap_SurfacesTypedRevertNotPanic() public {
        // The cap-check intermediate is widened to uint256 so `totalSupply + qty` cannot wrap
        // uint32 — even at `issuedIntexCount == type(uint32).max`. We can't drive `totalSupply`
        // all the way to 2^32 in a test (per-mint capped at uint16.max would need 65k+ calls),
        // but the widening is proved by inspection AND by this small-cap test that verifies
        // the typed SupplyCapExceeded surfaces cleanly on the overshoot. Pre-widening, an
        // analogous setup at the uint32 boundary would panic with arithmetic overflow.
        uint32 cap = 100;
        _createSeries(cap);

        vm.startPrank(bridger);
        nft.mint(holderA, cap, SERIES_ID);

        // mint overshoot by uint16-bounded amounts — typed revert, not panic
        vm.expectRevert(
            abi.encodeWithSelector(IIntexNFT1155.SupplyCapExceeded.selector, SERIES_ID, uint256(cap) + 1, uint256(cap))
        );
        nft.mint(holderB, 1, SERIES_ID);

        // crosschainMint overshoot — typed revert with the (tokenId-derived seriesId, attempted, cap) tuple
        nft.markQualified(SERIES_ID);
        vm.expectRevert(
            abi.encodeWithSelector(IIntexNFT1155.SupplyCapExceeded.selector, SERIES_ID, uint256(cap) + 1, uint256(cap))
        );
        nft.crosschainMint(holderB, TOKEN_ID, 1);
        vm.stopPrank();
    }

    // --- getIssuedHoldersWithBalances pagination ---

    function test_GetIssuedHolders_ZeroLimit_Reverts() public {
        // ZeroLimit is the single canonical error for any pagination zero-limit, replacing the
        // earlier two-error split (ZeroLimit + ZeroPaginationLimit) so callers can branch on
        // one selector across `expireSeries` and the holders getter.
        _createSeries(10);
        vm.expectRevert(IIntexNFT1155.ZeroLimit.selector);
        nft.getIssuedHoldersWithBalances(SERIES_ID, 0, 0);
    }

    function test_GetIssuedHolders_OffsetBeyondLength_ReturnsEmpty() public {
        address[] memory seeded = _seedHolders(50, 3, 5);
        // Pull state out of "Called" timing isn't important — view function ignores deadline.
        (address[] memory holders, uint256[] memory issued, uint256[] memory settled, uint256 total) =
            nft.getIssuedHoldersWithBalances(SERIES_ID, 999, 100);
        assertEq(holders.length, 0);
        assertEq(issued.length, 0);
        assertEq(settled.length, 0);
        assertEq(total, seeded.length);
    }

    function test_W13_GetIssuedHolders_OffsetPlusLimitOverflow_ClipsCleanly() public {
        // Pre-fix: `end = offset + limit` overflows uint256 and reverts the call.
        // Post-fix: limit is clipped to `total - offset` first; the view returns a slice
        // rather than panicking — callers can safely pass `type(uint256).max` as a sentinel.
        address[] memory seeded = _seedHolders(50, 4, 2);

        (address[] memory holders, uint256[] memory issued, uint256[] memory settled, uint256 total) =
            nft.getIssuedHoldersWithBalances(SERIES_ID, 1, type(uint256).max);

        assertEq(total, seeded.length);
        assertEq(holders.length, seeded.length - 1, "clip to total - offset");
        assertEq(issued.length, holders.length);
        assertEq(settled.length, holders.length);
        for (uint256 i = 0; i < holders.length; i++) {
            assertEq(issued[i], 2);
        }
    }

    function test_GetIssuedHolders_SliceClipsToTotal() public {
        address[] memory seeded = _seedHolders(50, 5, 2);

        (address[] memory holders, uint256[] memory issued, uint256[] memory settled, uint256 total) =
            nft.getIssuedHoldersWithBalances(SERIES_ID, 3, 10);

        assertEq(total, seeded.length);
        assertEq(holders.length, 2);
        assertEq(issued.length, 2);
        assertEq(settled.length, 2);
        for (uint256 i = 0; i < holders.length; i++) {
            assertEq(issued[i], 2);
            assertEq(settled[i], 0);
        }
    }

    function test_GetIssuedHolders_ReportsBothIssuedAndSettledForListedHolders() public {
        _createSeries(20);
        vm.startPrank(bridger);
        nft.mint(holderA, 10, SERIES_ID);
        nft.mint(holderB, 6, SERIES_ID);
        nft.markCalled(SERIES_ID);
        vm.stopPrank();

        vm.prank(settler);
        nft.settle(SERIES_ID, holderA, holderA, 4);

        (address[] memory holders, uint256[] memory issued, uint256[] memory settled, uint256 total) =
            nft.getIssuedHoldersWithBalances(SERIES_ID, 0, type(uint256).max);

        assertEq(total, 2);
        assertEq(holders.length, 2);
        for (uint256 i = 0; i < holders.length; i++) {
            if (holders[i] == holderA) {
                assertEq(issued[i], 6);
                assertEq(settled[i], 4);
            } else {
                assertEq(holders[i], holderB);
                assertEq(issued[i], 6);
                assertEq(settled[i], 0);
            }
        }
    }
}
