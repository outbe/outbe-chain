// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity 0.8.30;

/// @title IVaultProvider
/// @notice Outbe-vault router interface — vendored from upstream so this repo can compile
///         without depending on outbe-vault as a git submodule or npm package.
/// @dev Upstream source: https://github.com/outbe/outbe-vault — `src/interfaces/IVaultProvider.sol`
///      at commit `39b7494` (2026-05-18 sync). Re-sync this file when outbe-vault publishes
///      an npm package or updates the interface; single canonical copy lives here under
///      `contracts/vendor/outbe-vault/interfaces/` and is imported by both Outbe-side
///      and BNB-side (`contracts/target/EscrowAdapter.sol`).
/// @dev Pre-added on 2026-05-18 ahead of the corresponding outbe-vault PR: `IntexBidPrice`
///      (appended after `CredisAnadosis`) is the slot the outbe-vault owner will register
///      `EscrowAdapter` under via `addLiquiditySource`. Andrey confirmed this name; it may
///      change before the upstream PR merges, in which case re-sync this file and the
///      `EscrowAdapter` NatSpec.
/// @dev We never reference enum slot names in our Solidity (we only call `depositLiquidity`),
///      so the slot literal exists here purely as documentation and as a stable target for
///      the deployment runbook.
interface IVaultProvider {
    enum LiquiditySource {
        Unknown,
        NodCostPrice,
        IntexStrikePrice,
        CredisAnadosis,
        IntexBidPrice
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

    /// @notice Returns vault shares currently held by this provider.
    function sharesBalance(address vault) external view returns (uint256);
}
