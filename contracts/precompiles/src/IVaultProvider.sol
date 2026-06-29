// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity ^0.8.0;

interface IVaultProvider {
    enum LiquiditySource {
        Unknown,
        NodCostPrice,
        IntexStrikePrice,
        CredisAnadosis,
        IntexBidPrice,
        GemSettle
    }

    enum LiquidityTarget {
        Unknown,
        Credis
    }

    error InvalidLiquiditySource();
    error InvalidLiquidityTarget();
    error ReserveVaultNotConfigured();
    error ReserveVaultAssetMismatch();
    error ReserveVaultAlreadyAdded();
    error ReserveVaultNotFound();
    error LiquiditySourceNotFound();
    error LiquidityTargetNotFound();
    error InsufficientSharesForWithdraw(uint256 availableShares, uint256 requiredShares);

    event VaultAdded(address indexed asset, address indexed vault);
    event VaultRemoved(address indexed asset, address indexed vault);
    event LiquiditySourceAdded(address indexed sourceAddress, LiquiditySource sourceType);
    event LiquiditySourceRemoved(address indexed sourceAddress, LiquiditySource sourceType);
    event LiquidityTargetAdded(address indexed targetAddress, LiquidityTarget targetType);
    event LiquidityTargetRemoved(address indexed targetAddress, LiquidityTarget targetType);

    event LiquidityDeposited(
        address indexed source,
        address indexed vault,
        uint256 assetsAmount,
        uint256 sharesAmount,
        LiquiditySource sourceType
    );

    event LiquidityWithdrawn(
        address indexed target,
        address indexed receiver,
        address indexed vault,
        uint256 assetsAmount,
        uint256 burnedShares
    );

    /// @notice Returns the number of assets.
    function assetsCount() external view returns (uint256);

    /// @notice Returns the asset at `index`. Reverts if out of bounds.
    function assetAt(uint256 index) external view returns (address asset);

    /// @notice Returns the number of vaults registered for `asset`.
    function assetVaultsCount(address asset) external view returns (uint256);

    /// @notice Returns the reserve vault at `index` for `asset`. Reverts if out of bounds.
    function assetVaultAt(address asset, uint256 index) external view returns (address vault);

    /// @notice Returns the number of liquidity sources.
    function liquiditySourcesCount() external view returns (uint256);

    /// @notice Returns the liquidity source at `index`. Reverts if out of bounds.
    function liquiditySourceAt(uint256 index) external view returns (address sourceAddress, LiquiditySource sourceType);

    /// @notice Returns the number of liquidity sources.
    function liquidityTargetsCount() external view returns (uint256);

    /// @notice Returns the liquidity source at `index`. Reverts if out of bounds.
    function liquidityTargetAt(uint256 index) external view returns (address targetAddress, LiquidityTarget targetType);

    /// @notice Registers a vault. Reverts if already registered.
    function addVault(address vault) external;

    /// @notice Removes a previously registered vault for `asset`. Reverts if not found.
    function removeVault(address vault) external;

    function addLiquiditySource(address sourceAddress, LiquiditySource sourceType) external;

    function removeLiquiditySource(address sourceAddress) external;

    function addLiquidityTarget(address targetAddress, LiquidityTarget targetType) external;

    function removeLiquidityTarget(address targetAddress) external;

    function depositLiquidity(address asset, uint256 assetsAmount) external returns (uint256 sharesAmount);

    function withdrawLiquidity(address asset, uint256 amount, address receiver) external returns (uint256 burnedShares);

    // TODO remove these methods after implementation governance
    /// @notice Returns the current owner (admin) of the vault provider.
    function owner() external view returns (address);

    /// @notice Returns vault shares currently held by this provider.
    function sharesBalance(address vault) external view returns (uint256);

    /// @notice VaultV2 gate hook: only the provider itself can receive shares.
    function canReceiveShares(address account) external view returns (bool);

    /// @notice VaultV2 gate hook: only the provider itself can send shares.
    function canSendShares(address account) external view returns (bool);

    /// @notice VaultV2 gate hook: only the provider itself can receive assets.
    function canReceiveAssets(address account) external view returns (bool);

    /// @notice VaultV2 gate hook: only the provider itself can send assets.
    function canSendAssets(address account) external view returns (bool);
}
