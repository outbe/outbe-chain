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

    enum CrosschainOperationKind {
        Unknown,
        Deposit,
        Withdraw
    }

    enum CrosschainOperationStatus {
        Unknown,
        Pending,
        Completed
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
    error InvalidDestinationChain();
    error CrosschainBridgeNotConfigured();
    error CrosschainAssetNotConfigured();
    error CrosschainTokenBridgeNotConfigured();
    error RemoteVaultProviderNotConfigured(uint256 chainId);
    error CrosschainFeeMismatch(uint256 provided, uint256 required);
    error CrosschainOperationNotFound(bytes32 operationId);
    error CrosschainOperationAlreadyCompleted(bytes32 operationId);
    error CrosschainOperationsPending(uint256 count);
    error InvalidCrosschainSender();
    error InvalidCrosschainCallback();
    error InsufficientCrosschainShares(uint256 availableShares, uint256 requiredShares);

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
        uint256 burnedShares,
        LiquidityTarget targetType
    );

    event CrosschainBridgeUpdated(address indexed oldBridge, address indexed newBridge);
    event RemoteVaultProviderUpdated(uint256 indexed chainId, address indexed oldProvider, address indexed newProvider);
    event CrosschainAssetUpdated(
        address indexed oldAsset, address indexed newAsset, address indexed tokenBridge, uint256 destinationChainId
    );
    event CrosschainDepositSent(
        bytes32 indexed operationId,
        address indexed user,
        uint256 assetsAmount,
        uint256 destinationChainId,
        bytes32 sendId
    );
    event CrosschainDepositFinalized(
        bytes32 indexed operationId, address indexed user, uint256 assetsAmount, uint256 receiptShares
    );
    event CrosschainWithdrawalSent(
        bytes32 indexed operationId,
        address indexed user,
        uint256 receiptShares,
        uint256 destinationChainId,
        bytes32 sendId
    );
    event CrosschainWithdrawalFinalized(
        bytes32 indexed operationId, address indexed user, uint256 receiptShares, uint256 assetsAmount
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

    /// @notice Returns the number of liquidity targets.
    function liquidityTargetsCount() external view returns (uint256);

    /// @notice Returns the liquidity target at `index`. Reverts if out of bounds.
    function liquidityTargetAt(uint256 index) external view returns (address targetAddress, LiquidityTarget targetType);

    /// @notice Registers a vault. Reverts if already registered.
    function addVault(address vault) external;

    /// @notice Removes a previously registered vault for `asset`. Reverts if not found.
    function removeVault(address vault) external;

    /// @notice Registers `sourceAddress` as an authorized liquidity source of `sourceType`.
    function addLiquiditySource(address sourceAddress, LiquiditySource sourceType) external;

    /// @notice Deregisters a previously registered liquidity source. Reverts if not found.
    function removeLiquiditySource(address sourceAddress) external;

    /// @notice Registers `targetAddress` as an authorized liquidity target of `targetType`.
    function addLiquidityTarget(address targetAddress, LiquidityTarget targetType) external;

    /// @notice Deregisters a previously registered liquidity target. Reverts if not found.
    function removeLiquidityTarget(address targetAddress) external;

    /// @notice Deposits `assetsAmount` of `asset` into the asset's vault on behalf of the
    ///         caller. The caller (`msg.sender`) must be a registered liquidity source.
    function depositLiquidity(address asset, uint256 assetsAmount) external returns (uint256 sharesAmount);

    /// @notice Redeems `amount` of `asset` from the vault and tops it up into `receiver`.
    ///         The caller (`msg.sender`) must be a registered liquidity target.
    function withdrawLiquidity(address asset, uint256 amount, address receiver) external returns (uint256 burnedShares);

    // TODO remove after implementation governance
    /// @notice Returns the current owner (admin) of the vault provider.
    function owner() external view returns (address);

    /// @notice Returns vault shares currently held by this provider.
    function sharesBalance(address vault) external view returns (uint256);

    /// @notice Returns the generic ERC-7786 bridge used for crosschain vault messages.
    function crosschainBridge() external view returns (address);

    /// @notice Returns the configured remote vault provider for `chainId`.
    function remoteVaultProvider(uint256 chainId) external view returns (address);

    /// @notice Sets the generic ERC-7786 bridge used for crosschain vault messages.
    function setCrosschainBridge(address bridge) external;

    /// @notice Sets the remote vault provider for `chainId`.
    function setRemoteVaultProvider(uint256 chainId, address provider) external;

    /// @notice Returns the Outbe asset used by the crosschain vault flow.
    function crosschainAsset() external view returns (address);

    /// @notice Returns the Outbe token bridge used to send and receive the crosschain asset.
    function crosschainTokenBridge() external view returns (address);

    /// @notice Returns the fixed destination chain hosting the remote vault.
    function crosschainDestinationChainId() external view returns (uint256);

    /// @notice Configures the Outbe asset, token bridge and fixed destination chain.
    function setCrosschainAsset(address asset, address tokenBridge, uint256 destinationChainId) external;

    /// @notice Returns the nonce used to derive crosschain operation identifiers.
    function crosschainOperationNonce() external view returns (uint256);

    /// @notice Returns the number of crosschain operations awaiting authenticated completion.
    function pendingCrosschainOperations() external view returns (uint256);

    /// @notice Returns the finalized 1:1 remote-vault receipt shares owned by `user`.
    function crosschainShares(address user) external view returns (uint256);

    /// @notice Returns the total finalized 1:1 remote-vault receipt shares.
    function totalCrosschainShares() external view returns (uint256);

    /// @notice Returns the stored details and lifecycle state of `operationId`.
    function crosschainOperation(bytes32 operationId)
        external
        view
        returns (address user, uint256 amount, CrosschainOperationKind kind, CrosschainOperationStatus status);

    /// @notice Quotes a crosschain WCOEN deposit and previews its operation identifier.
    function quoteCrosschainDeposit(uint256 assetsAmount, uint256 destinationGasLimit, uint256 acknowledgementGasLimit)
        external
        view
        returns (uint256 nativeFee, bytes32 operationId);

    /// @notice Locks Outbe WCOEN and starts a deposit into the fixed remote 1:1 vault.
    function crosschainDeposit(uint256 assetsAmount, uint256 destinationGasLimit, uint256 acknowledgementGasLimit)
        external
        payable
        returns (bytes32 operationId, bytes32 sendId);

    /// @notice Quotes a crosschain receipt-share withdrawal and previews its operation identifier.
    function quoteCrosschainWithdraw(uint256 sharesAmount, uint256 requestGasLimit, uint256 returnGasLimit)
        external
        view
        returns (uint256 nativeFee, bytes32 operationId);

    /// @notice Removes 1:1 receipt shares and requests the corresponding WCOEN from the remote vault.
    function crosschainWithdraw(uint256 sharesAmount, uint256 requestGasLimit, uint256 returnGasLimit)
        external
        payable
        returns (bytes32 operationId, bytes32 sendId);

    /// @notice Receives the BNB deposit acknowledgement through the generic ERC-7786 bridge.
    function receiveMessage(bytes32 receiveId, bytes calldata sender, bytes calldata payload)
        external
        payable
        returns (bytes4);

    /// @notice Receives returned WCOEN after the Outbe token bridge credits this provider.
    function onCrosschainTokensReceived(
        uint32 sourceDomain,
        bytes calldata from,
        uint256 amount,
        bytes calldata extraData
    ) external returns (bytes4);
}
