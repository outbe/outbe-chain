// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity ^0.8.0;

/// @notice Local stablecoin vault routing and authorization surface.
interface IVaultRouter {
    enum StablesSource {
        Unknown,
        IntexCostAmount,
        CredisCostAmount,
        GemCostAmount,
        NodCostAmount
    }

    enum StablesTarget {
        Unknown,
        Credis
    }

    struct LiquidityReservation {
        address asset;
        /// @dev In `asset`'s own minor units.
        uint256 amountMinor;
        address credisOwner;
        address cca;
        address vault;
        uint64 expiresAt;
        uint256 gratisMinor;
        uint256 snapshotId;
        uint256 entryPriceMinor;
        uint256 valuationPriceMinor;
        uint256 policyRate;
        uint16 issuanceCurrency;
        uint8 assetDecimals;
        uint16 referenceCurrency;
        uint256 callAnchorPriceMinor;
        address source;
    }

    error TokenOperationFailed();
    error InvalidLiquiditySource();
    error InvalidLiquidityTarget();
    error ReserveVaultNotConfigured();
    error ReserveVaultAssetMismatch();
    error ReserveVaultAlreadyAdded();
    error ReserveVaultNotFound();
    error ReserveVaultOwnerNotRenounced(address currentOwner);
    error LiquiditySourceNotFound();
    error LiquidityTargetNotFound();
    error InsufficientSharesForWithdraw(uint256 availableShares, uint256 requiredShares);
    error SameVaultRebalance();
    error InvalidRebalanceAmount();
    error RebalanceVaultNotRegistered(address vault);
    error RebalanceInputExceedsMax(uint256 requiredMinor, uint256 maxAssetsToMinor);
    error UnsupportedAssetDecimals(uint8 decimals);
    error ReservationExists(uint256 id);
    error ReservationNotFound(uint256 id);
    error ReservationExpired(uint256 id);
    error CredisOwnerMismatch();
    error ReservationInsufficient(uint256 availableMinor, uint256 requiredMinor);
    error InvalidReservationAmount();
    error ApprovedVaultActionNotFound(uint256 proposalId);
    error ApprovedVaultActionIndexInconsistent(uint256 proposalId);
    error InvalidApprovedVaultAction();

    event VaultAdded(uint16 indexed isoCode, address indexed asset, address indexed vault);
    event VaultRemoved(uint16 indexed isoCode, address indexed asset, address indexed vault);
    event ApprovedVaultActionExpired(uint256 indexed proposalId, address indexed vault);
    event LiquiditySourceAdded(address indexed sourceAddress, StablesSource sourceType);
    event LiquiditySourceRemoved(address indexed sourceAddress, StablesSource sourceType);
    event LiquidityTargetAdded(address indexed targetAddress, StablesTarget targetType);
    event LiquidityTargetRemoved(address indexed targetAddress, StablesTarget targetType);

    event LiquidityDeposited(
        address indexed source, address indexed vault, uint256 assetsMinor, uint256 shares, StablesSource sourceType
    );

    event LiquidityWithdrawn(
        address indexed target,
        address indexed receiver,
        address indexed vault,
        uint256 assetsMinor,
        uint256 burnedShares,
        StablesTarget targetType
    );

    event LiquidityRebalanced(
        address indexed cca,
        address indexed vaultFrom,
        address indexed vaultTo,
        uint256 assetsWithdrawnMinor,
        uint256 burnedShares,
        uint256 assetsDepositedMinor,
        uint256 mintedShares
    );

    event ReservationCreated(
        uint256 indexed id,
        address indexed credisOwner,
        address indexed cca,
        address asset,
        address vault,
        uint256 amountMinor,
        uint64 expiresAt
    );
    event ReservationReleased(uint256 indexed id, address indexed asset, address indexed receiver, uint256 amountMinor);
    event ReservationReturned(
        uint256 indexed id, address indexed asset, address indexed vault, uint256 amountMinor, uint256 mintedShares
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

    /// @notice Returns every asset registered under `isoCode`. Order is not
    ///         stable across removals and carries no meaning.
    function referenceCurrencyAssets(uint16 isoCode) external view returns (address[] memory assets);

    /// @notice Returns the number of liquidity sources.
    function liquiditySourcesCount() external view returns (uint256);

    /// @notice Returns the liquidity source at `index`. Reverts if out of bounds.
    function liquiditySourceAt(uint256 index) external view returns (address sourceAddress, StablesSource sourceType);

    /// @notice Returns the number of liquidity targets.
    function liquidityTargetsCount() external view returns (uint256);

    /// @notice Returns the liquidity target at `index`. Reverts if out of bounds.
    function liquidityTargetAt(uint256 index) external view returns (address targetAddress, StablesTarget targetType);

    /// @notice Applies a pending governance-approved vault action once. Anyone may call.
    ///         A failed application remains retriable. Strictly after the governance
    ///         window, deletes the record and emits ApprovedVaultActionExpired instead.
    function applyApproved(uint256 proposalId) external;

    /// @notice Deposits `assetsMinor` of `asset` into the asset's vault on behalf of the
    ///         caller. The caller (`msg.sender`) must be a registered liquidity source.
    function deposit(address asset, uint256 assetsMinor) external returns (uint256 shares);

    /// @notice Redeems `amountMinor` of `asset` from the vault and tops it up into `receiver`.
    ///         The caller (`msg.sender`) must be a registered liquidity target.
    function withdraw(address asset, uint256 amountMinor, address receiver) external returns (uint256 burnedShares);

    /// @notice Returns vault shares currently held by this provider.
    function sharesBalance(address vault) external view returns (uint256);

    /// @notice Moves `assetsMinor` of liquidity from `vaultFrom` to `vaultTo`. The caller
    ///         supplies the destination asset at the oracle cross rate and receives the source
    ///         asset in exchange. The caller must have approved this router for at least the
    ///         required amount. `maxAssetsToMinor` bounds what the router may pull if the rate
    ///         moved since the caller quoted it. Caller must be an active CCA.
    function rebalance(address vaultFrom, address vaultTo, uint256 assetsMinor, uint256 maxAssetsToMinor)
        external
        returns (uint256 assetsToMinor);

    /// @notice What a {rebalance} of `assetsMinor` would require the caller to supply, so it
    ///         can approve exactly that before calling.
    function previewRebalance(address vaultFrom, address vaultTo, uint256 assetsMinor)
        external
        view
        returns (address assetFrom, address assetTo, uint256 assetsToMinor);

    /// @notice Whether the asset's reserve vault can currently fund a withdrawal of
    ///         `amountMinor` of `asset`. `withdraw` and `reserveStables` enforce the same
    ///         predicate. Returns false rather than reverting when `asset` has no vault.
    function hasLiquidity(address asset, uint256 amountMinor) external view returns (bool sufficient);

    /// @notice Redeems `amountMinor` of `asset` from its origin vault and holds it in this
    ///         router's custody for `credisOwner`, guaranteeing it can later be
    ///         delivered for 15 minutes. Caller must be an active CCA. Only `source`
    ///         may pledge the Gratis collateral for this reservation. The quote fixes the
    ///         pledge valuation and the Credis call terms: `referenceCurrency`, its previous
    ///         closed UTC-day VWAP as the call anchor, and the issuance currency's policy rate.
    function reserveStables(
        address credisOwner,
        address source,
        address asset,
        uint256 amountMinor,
        uint16 referenceCurrency
    ) external returns (uint256 reservationId);

    /// @notice Validates `receiver` as the reservation's Credis owner, pays `amountMinor` to
    ///         the recorded CCA to cover COEN delivered to that owner, and
    ///         returns any unused remainder to the origin vault. Caller must be a
    ///         registered liquidity target. `receiver` must be the Credis
    ///         owner. Reverts if the reservation is missing, expired, or too small.
    /// @dev Only CredisFactory may call. `receiver` binds the Credis owner.
    ///      `receiver` cannot redirect the payout.
    function releaseReservation(uint256 id, address receiver, uint256 amountMinor)
        external
        returns (uint256 deliveredMinor);

    /// @notice Deposits an unused reservation back into its origin vault and deletes it.
    ///         The originating CCA may call at any time. After expiry, anyone may call.
    ///         Idempotent. An `id` that holds nothing returns 0 instead of reverting.
    function returnReservation(uint256 id) external returns (uint256 mintedShares);

    /// @notice The reservation held under `id`, or a zeroed struct if none.
    function reservationOf(uint256 id) external view returns (LiquidityReservation memory reservation);
}
