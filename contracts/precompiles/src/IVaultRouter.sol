// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity ^0.8.0;

/// @notice Local stablecoin vault routing and authorization surface.
interface IVaultRouter {
    enum StablesSource {
        Unknown,
        NodCostAmount,
        IntexCostAmount,
        CredisCostAmount,
        GemCostAmount
    }

    enum StablesTarget {
        Unknown,
        Credis
    }

    error InvalidLiquiditySource();
    error InvalidLiquidityTarget();
    error InvalidReferenceCurrency();
    error ReserveVaultNotConfigured();
    error ReserveVaultAssetMismatch();
    error ReserveVaultAlreadyAdded();
    error ReserveVaultNotFound();
    error ReserveVaultOwnerNotRenounced(address currentOwner);
    error LiquiditySourceNotFound();
    error LiquidityTargetNotFound();
    error InsufficientSharesForWithdraw(uint256 availableShares, uint256 requiredShares);

    event VaultAdded(uint16 indexed isoCode, address indexed asset, address indexed vault);
    event VaultRemoved(uint16 indexed isoCode, address indexed asset, address indexed vault);
    event LiquiditySourceAdded(address indexed sourceAddress, StablesSource sourceType);
    event LiquiditySourceRemoved(address indexed sourceAddress, StablesSource sourceType);
    event LiquidityTargetAdded(address indexed targetAddress, StablesTarget targetType);
    event LiquidityTargetRemoved(address indexed targetAddress, StablesTarget targetType);

    event LiquidityDeposited(
        address indexed source,
        address indexed vault,
        uint256 assetsAmount,
        uint256 sharesAmount,
        StablesSource sourceType
    );

    event LiquidityWithdrawn(
        address indexed target,
        address indexed receiver,
        address indexed vault,
        uint256 assetsAmount,
        uint256 burnedShares,
        StablesTarget targetType
    );

    /// @notice Returns the number of assets.
    function assetsCount() external view returns (uint256);

    /// @notice Returns the asset at `index`. Reverts if out of bounds.
    function assetAt(uint256 index) external view returns (address asset);

    /// @notice Returns the number of vaults registered for `asset`.
    function assetVaultsCount(address asset) external view returns (uint256);

    /// @notice Returns the reserve vault at `index` for `asset`. Reverts if out of bounds.
    function assetVaultAt(address asset, uint256 index) external view returns (address vault);

    /// @notice Returns the number of vaults registered for an ISO 4217 currency code.
    function referenceCurrencyVaultsCount(uint16 isoCode) external view returns (uint256);

    /// @notice Returns the vault at `index` for an ISO 4217 currency code. Reverts if out of bounds.
    function referenceCurrencyVaultAt(uint16 isoCode, uint256 index) external view returns (address vault);

    /// @notice Returns the ISO 4217 currency code recorded when `vault` was registered.
    function vaultReferenceCurrency(address vault) external view returns (uint16 isoCode);

    /// @notice Returns the number of liquidity sources.
    function liquiditySourcesCount() external view returns (uint256);

    /// @notice Returns the liquidity source at `index`. Reverts if out of bounds.
    function liquiditySourceAt(uint256 index) external view returns (address sourceAddress, StablesSource sourceType);

    /// @notice Returns the number of liquidity targets.
    function liquidityTargetsCount() external view returns (uint256);

    /// @notice Returns the liquidity target at `index`. Reverts if out of bounds.
    function liquidityTargetAt(uint256 index) external view returns (address targetAddress, StablesTarget targetType);

    // TODO remove after implementation governance
    /// @notice Registers a vault. Reverts if already registered.
    function addVault(address vault) external;

    // TODO remove after implementation governance
    /// @notice Removes a previously registered vault for `asset`. Reverts if not found.
    function removeVault(address vault) external;

    // TODO remove after implementation governance
    /// @notice Registers `sourceAddress` as an authorized liquidity source of `sourceType`.
    function addLiquiditySource(address sourceAddress, StablesSource sourceType) external;

    // TODO remove after implementation governance
    /// @notice Deregisters a previously registered liquidity source. Reverts if not found.
    function removeLiquiditySource(address sourceAddress) external;

    // TODO remove after implementation governance
    /// @notice Registers `targetAddress` as an authorized liquidity target of `targetType`.
    function addLiquidityTarget(address targetAddress, StablesTarget targetType) external;

    // TODO remove after implementation governance
    /// @notice Deregisters a previously registered liquidity target. Reverts if not found.
    function removeLiquidityTarget(address targetAddress) external;

    /// @notice Deposits `assetsAmount` of `asset` into the asset's vault on behalf of the
    ///         caller. The caller (`msg.sender`) must be a registered liquidity source.
    function deposit(address asset, uint256 assetsAmount) external returns (uint256 sharesAmount);

    /// @notice Redeems `amount` of `asset` from the vault and tops it up into `receiver`.
    ///         The caller (`msg.sender`) must be a registered liquidity target.
    function withdraw(address asset, uint256 amount, address receiver) external returns (uint256 burnedShares);

    // TODO remove after implementation governance
    /// @notice Returns the current owner (admin) of the vault router.
    function owner() external view returns (address);

    /// @notice Returns vault shares currently held by this provider.
    function sharesBalance(address vault) external view returns (uint256);
}
