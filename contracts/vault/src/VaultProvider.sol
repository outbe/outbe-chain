// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity 0.8.30;

import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {EnumerableSet} from "@openzeppelin/contracts/utils/structs/EnumerableSet.sol";
import {ErrorsLib} from "./libraries/ErrorsLib.sol";
import {IReceiveSharesGate, ISendSharesGate, IReceiveAssetsGate, ISendAssetsGate} from "./interfaces/IGate.sol";
import {ITokenBundle} from "./interfaces/ITokenBundle.sol";
import {IVaultProvider} from "./interfaces/IVaultProvider.sol";
import {IVaultV2} from "./interfaces/IVaultV2.sol";
import {Initializable} from "@openzeppelin/contracts/proxy/utils/Initializable.sol";

contract VaultProvider is
    Initializable,
    OwnableUpgradeable,
    UUPSUpgradeable,
    IVaultProvider,
    IReceiveSharesGate,
    ISendSharesGate,
    IReceiveAssetsGate,
    ISendAssetsGate
{
    using SafeERC20 for IERC20;
    using EnumerableSet for EnumerableSet.AddressSet;

    EnumerableSet.AddressSet private _assets;
    mapping(address asset => EnumerableSet.AddressSet vaults) private _assetVaults;

    EnumerableSet.AddressSet private _liquiditySources;
    mapping(address account => LiquiditySource) public liquiditySourceTypes;

    EnumerableSet.AddressSet private _liquidityTargets;
    mapping(address account => LiquidityTarget) public liquidityTargetTypes;

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        _disableInitializers();
    }

    /// @notice Initializes the proxy. Replaces the constructor for upgradeable deployments.
    function initialize(address initialOwner) external initializer {
        __Ownable_init(initialOwner);
    }

    function _authorizeUpgrade(address) internal override onlyOwner {}

    function addVault(address vault) external onlyOwner {
        require(vault != address(0), ErrorsLib.ZeroAddress());

        address asset = IVaultV2(vault).asset();
        require(asset != address(0), ErrorsLib.ZeroAddress());

        require(_assetVaults[asset].add(vault), ReserveVaultAlreadyAdded());
        _assets.add(asset);

        IERC20(asset).forceApprove(vault, type(uint256).max);

        emit VaultAdded(asset, vault);
    }

    function removeVault(address vault) external onlyOwner {
        require(vault != address(0), ErrorsLib.ZeroAddress());

        address asset = IVaultV2(vault).asset();
        require(_assetVaults[asset].remove(vault), ReserveVaultNotFound());

        if (_assetVaults[asset].length() == 0) {
            _assets.remove(asset);
        }

        SafeERC20.forceApprove(IERC20(asset), vault, 0);

        emit VaultRemoved(asset, vault);
    }

    function addLiquiditySource(address sourceAddress, LiquiditySource sourceType) external onlyOwner {
        require(sourceAddress != address(0), ErrorsLib.ZeroAddress());
        require(sourceType != LiquiditySource.Unknown, InvalidLiquiditySource());

        _liquiditySources.add(sourceAddress);
        liquiditySourceTypes[sourceAddress] = sourceType;

        emit LiquiditySourceAdded(sourceAddress, sourceType);
    }

    function removeLiquiditySource(address sourceAddress) external onlyOwner {
        require(_liquiditySources.remove(sourceAddress), LiquiditySourceNotFound());

        LiquiditySource sourceType = liquiditySourceTypes[sourceAddress];
        delete liquiditySourceTypes[sourceAddress];

        emit LiquiditySourceRemoved(sourceAddress, sourceType);
    }

    function addLiquidityTarget(address targetAddress, LiquidityTarget targetType) external onlyOwner {
        require(targetAddress != address(0), ErrorsLib.ZeroAddress());
        require(targetType != LiquidityTarget.Unknown, InvalidLiquidityTarget());

        _liquidityTargets.add(targetAddress);
        liquidityTargetTypes[targetAddress] = targetType;

        emit LiquidityTargetAdded(targetAddress, targetType);
    }

    function removeLiquidityTarget(address targetAddress) external onlyOwner {
        require(_liquidityTargets.remove(targetAddress), LiquidityTargetNotFound());

        LiquidityTarget targetType = liquidityTargetTypes[targetAddress];
        delete liquidityTargetTypes[targetAddress];

        emit LiquidityTargetRemoved(targetAddress, targetType);
    }

    function depositLiquidity(address asset, uint256 assetsAmount) external returns (uint256 sharesAmount) {
        LiquiditySource sourceType = liquiditySourceTypes[msg.sender];
        require(sourceType != LiquiditySource.Unknown, InvalidLiquiditySource());

        address vault = _firstVault(asset);
        require(vault != address(0), ReserveVaultNotConfigured());

        IERC20(asset).safeTransferFrom(msg.sender, address(this), assetsAmount);

        sharesAmount = IVaultV2(vault).deposit(assetsAmount, address(this));

        emit LiquidityDeposited(msg.sender, vault, assetsAmount, sharesAmount, sourceType);
    }

    function withdrawLiquidity(address asset, uint256 amount, address receiver)
        external
        returns (uint256 burnedShares)
    {
        require(receiver != address(0), ErrorsLib.ZeroAddress());
        require(liquidityTargetTypes[msg.sender] != LiquidityTarget.Unknown, ErrorsLib.Unauthorized());

        address vault = _firstVault(asset);
        require(vault != address(0), ReserveVaultNotConfigured());

        uint256 requiredShares = IVaultV2(vault).previewWithdraw(amount);
        uint256 availableShares = IERC20(vault).balanceOf(address(this));
        require(availableShares >= requiredShares, InsufficientSharesForWithdraw(availableShares, requiredShares));

        burnedShares = IVaultV2(vault).withdraw(amount, address(this), address(this));

        IERC20(asset).forceApprove(receiver, amount);
        ITokenBundle(receiver).topUp(address(this), asset, amount);

        emit LiquidityWithdrawn(msg.sender, receiver, vault, amount, burnedShares);
    }

    function sharesBalance(address vault) external view returns (uint256) {
        return IERC20(vault).balanceOf(address(this));
    }

    function assetsCount() external view returns (uint256) {
        return _assets.length();
    }

    function assetAt(uint256 index) external view returns (address) {
        return _assets.at(index);
    }

    function assetVaultsCount(address asset) external view returns (uint256) {
        return _assetVaults[asset].length();
    }

    function assetVaultAt(address asset, uint256 index) external view returns (address) {
        return _assetVaults[asset].at(index);
    }

    function liquiditySourcesCount() external view returns (uint256) {
        return _liquiditySources.length();
    }

    function liquiditySourceAt(uint256 index)
        external
        view
        returns (address sourceAddress, LiquiditySource sourceType)
    {
        sourceAddress = _liquiditySources.at(index);
        sourceType = liquiditySourceTypes[sourceAddress];
    }

    function liquidityTargetsCount() external view returns (uint256) {
        return _liquidityTargets.length();
    }

    function liquidityTargetAt(uint256 index)
        external
        view
        returns (address targetAddress, LiquidityTarget targetType)
    {
        targetAddress = _liquidityTargets.at(index);
        targetType = liquidityTargetTypes[targetAddress];
    }

    /// @notice VaultV2 gate hook: only VaultProvider itself can receive shares.
    function canReceiveShares(address account) external view returns (bool) {
        return account == address(this);
    }

    /// @notice VaultV2 gate hook: only VaultProvider itself can send shares.
    function canSendShares(address account) external view returns (bool) {
        return account == address(this);
    }

    /// @notice VaultV2 gate hook: only VaultProvider itself can receive assets from vault.
    function canReceiveAssets(address account) external view returns (bool) {
        return account == address(this);
    }

    /// @notice VaultV2 gate hook: only VaultProvider itself can send assets to vault.
    function canSendAssets(address account) external view returns (bool) {
        return account == address(this);
    }

    // TODO implement vaults lookup logic to handle different vaults for the same asset when needed

    /// @dev Returns the first vault registered for `asset`, or address(0) if none.
    function _firstVault(address asset) private view returns (address) {
        EnumerableSet.AddressSet storage vaults = _assetVaults[asset];
        return vaults.length() == 0 ? address(0) : vaults.at(0);
    }
}
