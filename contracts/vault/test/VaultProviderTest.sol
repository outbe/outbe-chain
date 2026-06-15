// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity 0.8.30;

import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {ERC20Mock} from "./mocks/ERC20Mock.sol";
import {ERC4626Mock} from "./mocks/ERC4626Mock.sol";
import {ErrorsLib} from "../src/libraries/ErrorsLib.sol";
import {IERC20} from "../src/interfaces/IERC20.sol";
import {IVaultProvider} from "../src/interfaces/IVaultProvider.sol";
import {Test} from "forge-std/Test.sol";
import {TokenBundleReceiverMock} from "./mocks/TokenBundleReceiverMock.sol";
import {VaultProvider} from "../src/VaultProvider.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";

contract ReentrantUnauthorizedReceiverMock {
    VaultProvider public immutable PROVIDER;
    address public immutable ASSET;
    uint256 public immutable REENTER_AMOUNT;

    bool public reenterAttempted;
    bool public reenterSucceeded;

    constructor(VaultProvider _provider, address _asset, uint256 _reenterAmount) {
        PROVIDER = _provider;
        ASSET = _asset;
        REENTER_AMOUNT = _reenterAmount;
    }

    function topUp(address sender, address token, uint256 amount) external {
        require(token == ASSET, "unexpected token");

        reenterAttempted = true;
        (bool success,) = address(PROVIDER)
            .call(
                abi.encodeWithSelector(IVaultProvider.withdrawLiquidity.selector, ASSET, REENTER_AMOUNT, address(this))
            );
        reenterSucceeded = success;

        assert(IERC20(token).transferFrom(sender, address(this), amount));
    }
}

contract VaultProviderTest is Test {
    address immutable PROVIDER_OWNER = makeAddr("providerOwner");
    address immutable CONSUMER = makeAddr("consumer");
    address immutable INBOUND_SOURCE = makeAddr("inboundSource");

    ERC20Mock token;
    ERC4626Mock reserveVault;
    VaultProvider provider;
    TokenBundleReceiverMock receiver;

    function setUp() public {
        token = new ERC20Mock();
        vm.label(address(token), "token");

        reserveVault = new ERC4626Mock(address(token));
        vm.label(address(reserveVault), "reserveVault");

        VaultProvider impl = new VaultProvider();
        provider = VaultProvider(
            address(new ERC1967Proxy(address(impl), abi.encodeCall(VaultProvider.initialize, (PROVIDER_OWNER))))
        );
        vm.label(address(provider), "provider");

        receiver = new TokenBundleReceiverMock();
        vm.label(address(receiver), "receiver");

        // Configure provider
        vm.startPrank(PROVIDER_OWNER);
        provider.addVault(address(reserveVault));
        provider.addLiquidityTarget(CONSUMER, IVaultProvider.LiquidityTarget.Credis);
        provider.addLiquiditySource(INBOUND_SOURCE, IVaultProvider.LiquiditySource.NodCostPrice);
        vm.stopPrank();
    }

    function test_withdrawLiquidity_CallsTopUp() public {
        uint256 depositAmount = 1000e6;

        // Seed the reserve vault via depositLiquidity
        deal(address(token), INBOUND_SOURCE, depositAmount);
        vm.startPrank(INBOUND_SOURCE);
        token.approve(address(provider), depositAmount);
        provider.depositLiquidity(address(token), depositAmount);
        vm.stopPrank();

        // Consumer (registered target) withdraws
        uint256 withdrawAmount = 500e6;
        vm.prank(CONSUMER);
        uint256 burnedShares = provider.withdrawLiquidity(address(token), withdrawAmount, address(receiver));

        // Receiver got funds via topUp (transferFrom)
        assertEq(token.balanceOf(address(receiver)), withdrawAmount, "receiver should hold withdrawn funds");
        assertGt(burnedShares, 0, "should have burned shares");
    }

    function test_withdrawLiquidity_EmitsLiquidityWithdrawn() public {
        uint256 depositAmount = 1000e6;

        deal(address(token), INBOUND_SOURCE, depositAmount);
        vm.startPrank(INBOUND_SOURCE);
        token.approve(address(provider), depositAmount);
        provider.depositLiquidity(address(token), depositAmount);
        vm.stopPrank();

        uint256 withdrawAmount = 500e6;

        vm.prank(CONSUMER);
        vm.expectEmit(true, true, true, false);
        emit IVaultProvider.LiquidityWithdrawn(CONSUMER, address(receiver), address(reserveVault), withdrawAmount, 0);
        provider.withdrawLiquidity(address(token), withdrawAmount, address(receiver));
    }

    function test_withdrawLiquidity_TopUpReceivesCorrectArgs() public {
        uint256 depositAmount = 1000e6;

        deal(address(token), INBOUND_SOURCE, depositAmount);
        vm.startPrank(INBOUND_SOURCE);
        token.approve(address(provider), depositAmount);
        provider.depositLiquidity(address(token), depositAmount);
        vm.stopPrank();

        uint256 withdrawAmount = 200e6;

        vm.prank(CONSUMER);
        vm.expectEmit(true, true, true, true);
        emit TokenBundleReceiverMock.TopUpCalled(address(provider), address(token), withdrawAmount);
        provider.withdrawLiquidity(address(token), withdrawAmount, address(receiver));
    }

    function test_RevertWhen_withdrawLiquidity_UnauthorizedTarget() public {
        address unauthorized = makeAddr("unauthorized");
        vm.prank(unauthorized);
        vm.expectRevert(ErrorsLib.Unauthorized.selector);
        provider.withdrawLiquidity(address(token), 100e6, address(receiver));
    }

    function test_RevertWhen_withdrawLiquidity_ZeroReceiver() public {
        vm.prank(CONSUMER);
        vm.expectRevert(ErrorsLib.ZeroAddress.selector);
        provider.withdrawLiquidity(address(token), 100e6, address(0));
    }

    function test_RevertWhen_withdrawLiquidity_NoReserveVault() public {
        address unknownAsset = makeAddr("unknownAsset");
        vm.prank(CONSUMER);
        vm.expectRevert(IVaultProvider.ReserveVaultNotConfigured.selector);
        provider.withdrawLiquidity(unknownAsset, 100e6, address(receiver));
    }

    function test_RevertWhen_withdrawLiquidity_InsufficientShares() public {
        uint256 depositAmount = 1000e6;

        deal(address(token), INBOUND_SOURCE, depositAmount);
        vm.startPrank(INBOUND_SOURCE);
        token.approve(address(provider), depositAmount);
        provider.depositLiquidity(address(token), depositAmount);
        vm.stopPrank();

        uint256 withdrawAmount = depositAmount + 1;
        uint256 availableShares = reserveVault.balanceOf(address(provider));
        uint256 requiredShares = reserveVault.previewWithdraw(withdrawAmount);

        vm.prank(CONSUMER);
        vm.expectRevert(
            abi.encodeWithSelector(
                IVaultProvider.InsufficientSharesForWithdraw.selector, availableShares, requiredShares
            )
        );
        provider.withdrawLiquidity(address(token), withdrawAmount, address(receiver));
    }

    function test_RevertWhen_depositLiquidity_UnregisteredCaller() public {
        address unauthorized = makeAddr("unauthorized");

        vm.prank(unauthorized);
        vm.expectRevert(IVaultProvider.InvalidLiquiditySource.selector);
        provider.depositLiquidity(address(token), 100e6);
    }

    function test_RevertWhen_depositLiquidity_NoReserveVault() public {
        address unknownAsset = makeAddr("unknownAsset");

        vm.prank(INBOUND_SOURCE);
        vm.expectRevert(IVaultProvider.ReserveVaultNotConfigured.selector);
        provider.depositLiquidity(unknownAsset, 100e6);
    }

    function test_RevertWhen_withdrawLiquidity_TargetRevoked() public {
        vm.prank(PROVIDER_OWNER);
        provider.removeLiquidityTarget(CONSUMER);

        vm.prank(CONSUMER);
        vm.expectRevert(ErrorsLib.Unauthorized.selector);
        provider.withdrawLiquidity(address(token), 100e6, address(receiver));
    }

    function test_RevertWhen_addVault_NotOwner() public {
        address unauthorized = makeAddr("unauthorized");

        vm.prank(unauthorized);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, unauthorized));
        provider.addVault(address(reserveVault));
    }

    function test_RevertWhen_removeVault_NotOwner() public {
        address unauthorized = makeAddr("unauthorized");

        vm.prank(unauthorized);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, unauthorized));
        provider.removeVault(address(reserveVault));
    }

    function test_RevertWhen_addLiquiditySource_NotOwner() public {
        address unauthorized = makeAddr("unauthorized");

        vm.prank(unauthorized);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, unauthorized));
        provider.addLiquiditySource(INBOUND_SOURCE, IVaultProvider.LiquiditySource.NodCostPrice);
    }

    function test_RevertWhen_addLiquidityTarget_NotOwner() public {
        address unauthorized = makeAddr("unauthorized");

        vm.prank(unauthorized);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, unauthorized));
        provider.addLiquidityTarget(CONSUMER, IVaultProvider.LiquidityTarget.Credis);
    }

    function test_withdrawLiquidity_BlocksUnauthorizedReentrancy() public {
        uint256 depositAmount = 1000e6;

        deal(address(token), INBOUND_SOURCE, depositAmount);
        vm.startPrank(INBOUND_SOURCE);
        token.approve(address(provider), depositAmount);
        provider.depositLiquidity(address(token), depositAmount);
        vm.stopPrank();

        uint256 withdrawAmount = 300e6;
        uint256 reenterAmount = 100e6;
        ReentrantUnauthorizedReceiverMock reenterReceiver =
            new ReentrantUnauthorizedReceiverMock(provider, address(token), reenterAmount);

        vm.prank(CONSUMER);
        provider.withdrawLiquidity(address(token), withdrawAmount, address(reenterReceiver));

        assertTrue(reenterReceiver.reenterAttempted(), "reentrant path should be attempted");
        assertFalse(reenterReceiver.reenterSucceeded(), "unauthorized reentrant withdraw should fail");
        assertEq(token.balanceOf(address(reenterReceiver)), withdrawAmount, "receiver should only get requested amount");
    }

    function test_depositLiquidity() public {
        uint256 depositAmount = 1000e6;

        deal(address(token), INBOUND_SOURCE, depositAmount);
        vm.startPrank(INBOUND_SOURCE);
        token.approve(address(provider), depositAmount);
        uint256 shares = provider.depositLiquidity(address(token), depositAmount);
        vm.stopPrank();

        assertGt(shares, 0, "should have received shares");
        assertEq(provider.sharesBalance(address(reserveVault)), shares, "provider should hold shares");
    }

    function testFuzz_withdrawLiquidity(uint256 depositAmount, uint256 withdrawAmount) public {
        depositAmount = bound(depositAmount, 1, 1e12);
        withdrawAmount = bound(withdrawAmount, 1, depositAmount);

        deal(address(token), INBOUND_SOURCE, depositAmount);
        vm.startPrank(INBOUND_SOURCE);
        token.approve(address(provider), depositAmount);
        provider.depositLiquidity(address(token), depositAmount);
        vm.stopPrank();

        vm.prank(CONSUMER);
        uint256 burnedShares = provider.withdrawLiquidity(address(token), withdrawAmount, address(receiver));

        assertEq(token.balanceOf(address(receiver)), withdrawAmount, "receiver balance mismatch");
        assertGt(burnedShares, 0, "should burn shares");
    }

    function test_gateCallbacks_AllowOnlyProviderAddress() public {
        address outsider = makeAddr("outsider");

        assertTrue(provider.canReceiveShares(address(provider)), "provider should receive shares");
        assertTrue(provider.canSendShares(address(provider)), "provider should send shares");
        assertTrue(provider.canReceiveAssets(address(provider)), "provider should receive assets");
        assertTrue(provider.canSendAssets(address(provider)), "provider should send assets");

        assertFalse(provider.canReceiveShares(outsider), "outsider should not receive shares");
        assertFalse(provider.canSendShares(outsider), "outsider should not send shares");
        assertFalse(provider.canReceiveAssets(outsider), "outsider should not receive assets");
        assertFalse(provider.canSendAssets(outsider), "outsider should not send assets");
    }

    function test_addVault_AppendsAndApproves() public {
        ERC4626Mock secondVault = new ERC4626Mock(address(token));

        vm.prank(PROVIDER_OWNER);
        vm.expectEmit(true, true, false, false);
        emit IVaultProvider.VaultAdded(address(token), address(secondVault));
        provider.addVault(address(secondVault));

        assertEq(provider.assetVaultsCount(address(token)), 2, "vault count should be 2");
        assertEq(provider.assetVaultAt(address(token), 0), address(reserveVault), "first vault unchanged");
        assertEq(provider.assetVaultAt(address(token), 1), address(secondVault), "second vault appended");
        assertEq(
            token.allowance(address(provider), address(secondVault)),
            type(uint256).max,
            "second vault should have max allowance"
        );
    }

    function test_RevertWhen_addVault_DuplicateVault() public {
        vm.prank(PROVIDER_OWNER);
        vm.expectRevert(IVaultProvider.ReserveVaultAlreadyAdded.selector);
        provider.addVault(address(reserveVault));
    }

    function test_RevertWhen_addVault_ZeroVault() public {
        vm.prank(PROVIDER_OWNER);
        vm.expectRevert(ErrorsLib.ZeroAddress.selector);
        provider.addVault(address(0));
    }

    function test_removeVault_DropsAndZerosApproval() public {
        vm.prank(PROVIDER_OWNER);
        vm.expectEmit(true, true, false, false);
        emit IVaultProvider.VaultRemoved(address(token), address(reserveVault));
        provider.removeVault(address(reserveVault));

        assertEq(provider.assetVaultsCount(address(token)), 0, "vault count should be 0");
        assertEq(provider.assetsCount(), 0, "asset registry should drop empty asset");
        assertEq(token.allowance(address(provider), address(reserveVault)), 0, "allowance should be reset to zero");
    }

    function test_RevertWhen_removeVault_NotFound() public {
        ERC4626Mock unknownVault = new ERC4626Mock(address(token));

        vm.prank(PROVIDER_OWNER);
        vm.expectRevert(IVaultProvider.ReserveVaultNotFound.selector);
        provider.removeVault(address(unknownVault));
    }

    function test_firstVault_ReturnsFirstVault_AfterRemoval() public {
        ERC4626Mock secondVault = new ERC4626Mock(address(token));

        vm.prank(PROVIDER_OWNER);
        provider.addVault(address(secondVault));

        assertEq(provider.assetVaultAt(address(token), 0), address(reserveVault), "first vault is initial reserve");

        // Removing the head causes swap-and-pop: secondVault becomes index 0.
        vm.prank(PROVIDER_OWNER);
        provider.removeVault(address(reserveVault));

        assertEq(provider.assetVaultsCount(address(token)), 1, "one vault remains");
        assertEq(provider.assetVaultAt(address(token), 0), address(secondVault), "second vault now first available");

        uint256 depositAmount = 500e6;
        deal(address(token), INBOUND_SOURCE, depositAmount);
        vm.startPrank(INBOUND_SOURCE);
        token.approve(address(provider), depositAmount);
        provider.depositLiquidity(address(token), depositAmount);
        vm.stopPrank();

        assertGt(IERC20(address(secondVault)).balanceOf(address(provider)), 0, "shares should land in second vault");
        assertEq(
            IERC20(address(reserveVault)).balanceOf(address(provider)), 0, "removed vault should hold no new shares"
        );
    }

    function test_depositLiquidity_RoutesToFirstVault_WhenMultipleConfigured() public {
        ERC4626Mock secondVault = new ERC4626Mock(address(token));

        vm.prank(PROVIDER_OWNER);
        provider.addVault(address(secondVault));

        uint256 depositAmount = 750e6;
        deal(address(token), INBOUND_SOURCE, depositAmount);
        vm.startPrank(INBOUND_SOURCE);
        token.approve(address(provider), depositAmount);
        provider.depositLiquidity(address(token), depositAmount);
        vm.stopPrank();

        assertGt(IERC20(address(reserveVault)).balanceOf(address(provider)), 0, "first vault should receive shares");
        assertEq(IERC20(address(secondVault)).balanceOf(address(provider)), 0, "second vault should remain empty");
    }

    function test_assetsCount_TracksUniqueAssets() public {
        // Initial setUp registered one vault for `token`.
        assertEq(provider.assetsCount(), 1, "one asset after setUp");
        assertEq(provider.assetAt(0), address(token), "first asset is token");

        // Adding a second vault for the same asset must not change the asset count.
        ERC4626Mock secondVault = new ERC4626Mock(address(token));
        vm.prank(PROVIDER_OWNER);
        provider.addVault(address(secondVault));
        assertEq(provider.assetsCount(), 1, "asset count unchanged when adding a vault for the same asset");

        // A vault for a fresh asset bumps the asset count.
        ERC20Mock otherToken = new ERC20Mock();
        ERC4626Mock otherVault = new ERC4626Mock(address(otherToken));
        vm.prank(PROVIDER_OWNER);
        provider.addVault(address(otherVault));
        assertEq(provider.assetsCount(), 2, "asset count includes new asset");

        // Removing every vault of one asset drops the asset from the registry.
        vm.startPrank(PROVIDER_OWNER);
        provider.removeVault(address(reserveVault));
        provider.removeVault(address(secondVault));
        vm.stopPrank();
        assertEq(provider.assetsCount(), 1, "asset removed once last vault drops");
        assertEq(provider.assetAt(0), address(otherToken), "remaining asset is otherToken");
    }

    function test_liquiditySources_Enumeration() public {
        // setUp already registered `inboundSource` as NodCostPrice.
        assertEq(provider.liquiditySourcesCount(), 1, "one source after setUp");

        address secondSource = makeAddr("secondSource");
        vm.prank(PROVIDER_OWNER);
        provider.addLiquiditySource(secondSource, IVaultProvider.LiquiditySource.IntexStrikePrice);

        assertEq(provider.liquiditySourcesCount(), 2, "two sources registered");

        (address addr0, IVaultProvider.LiquiditySource type0) = provider.liquiditySourceAt(0);
        (address addr1, IVaultProvider.LiquiditySource type1) = provider.liquiditySourceAt(1);

        assertEq(addr0, INBOUND_SOURCE, "first source address");
        assertEq(uint256(type0), uint256(IVaultProvider.LiquiditySource.NodCostPrice), "first source type");
        assertEq(addr1, secondSource, "second source address");
        assertEq(uint256(type1), uint256(IVaultProvider.LiquiditySource.IntexStrikePrice), "second source type");

        // Removal drops the entry.
        vm.prank(PROVIDER_OWNER);
        provider.removeLiquiditySource(INBOUND_SOURCE);
        assertEq(provider.liquiditySourcesCount(), 1, "source count drops after removal");
        assertEq(uint256(provider.liquiditySourceTypes(INBOUND_SOURCE)), 0, "source type cleared on removal");
    }

    function test_liquidityTargets_Enumeration() public {
        // setUp already registered `consumer` as Credis.
        assertEq(provider.liquidityTargetsCount(), 1, "one target after setUp");

        address secondTarget = makeAddr("secondTarget");
        vm.prank(PROVIDER_OWNER);
        provider.addLiquidityTarget(secondTarget, IVaultProvider.LiquidityTarget.Credis);

        assertEq(provider.liquidityTargetsCount(), 2, "two targets registered");

        (address addr0, IVaultProvider.LiquidityTarget type0) = provider.liquidityTargetAt(0);
        (address addr1, IVaultProvider.LiquidityTarget type1) = provider.liquidityTargetAt(1);

        assertEq(addr0, CONSUMER, "first target address");
        assertEq(uint256(type0), uint256(IVaultProvider.LiquidityTarget.Credis), "first target type");
        assertEq(addr1, secondTarget, "second target address");
        assertEq(uint256(type1), uint256(IVaultProvider.LiquidityTarget.Credis), "second target type");

        vm.prank(PROVIDER_OWNER);
        provider.removeLiquidityTarget(CONSUMER);
        assertEq(provider.liquidityTargetsCount(), 1, "target count drops after removal");
        assertEq(uint256(provider.liquidityTargetTypes(CONSUMER)), 0, "target type cleared on removal");
    }

    function test_RevertWhen_removeLiquiditySource_NotFound() public {
        address unregistered = makeAddr("unregistered");
        vm.prank(PROVIDER_OWNER);
        vm.expectRevert(IVaultProvider.LiquiditySourceNotFound.selector);
        provider.removeLiquiditySource(unregistered);
    }

    function test_RevertWhen_removeLiquidityTarget_NotFound() public {
        address unregistered = makeAddr("unregistered");
        vm.prank(PROVIDER_OWNER);
        vm.expectRevert(IVaultProvider.LiquidityTargetNotFound.selector);
        provider.removeLiquidityTarget(unregistered);
    }

    function test_RevertWhen_addLiquiditySource_UnknownType() public {
        vm.prank(PROVIDER_OWNER);
        vm.expectRevert(IVaultProvider.InvalidLiquiditySource.selector);
        provider.addLiquiditySource(makeAddr("noType"), IVaultProvider.LiquiditySource.Unknown);
    }

    function test_RevertWhen_addLiquidityTarget_UnknownType() public {
        vm.prank(PROVIDER_OWNER);
        vm.expectRevert(IVaultProvider.InvalidLiquidityTarget.selector);
        provider.addLiquidityTarget(makeAddr("noType"), IVaultProvider.LiquidityTarget.Unknown);
    }
}
