//! Error types for DEX pool configuration, EVM RPC, math, and worker polling.

use thiserror::Error;

/// Errors that occur in DEX providers and shared EVM JSON-RPC calls.
#[derive(Debug, Error)]
pub(crate) enum DexError {
    /// The EVM RPC HTTP response body exceeds 16 MiB.
    #[error("DEX RPC response exceeds 16 MiB")]
    RpcResponseTooLarge,
    /// The EVM RPC response envelope has a mismatched ID or JSON-RPC version.
    #[error("invalid DEX RPC response envelope")]
    InvalidRpcEnvelope,
    /// The EVM RPC server returned an error object.
    #[error("DEX RPC {method} error code {code:?}")]
    RpcErrorCode {
        /// The JSON-RPC method name.
        method: String,
        /// The numeric JSON-RPC error code, if present.
        code: Option<i64>,
    },
    /// The EVM RPC response contains no result or a null result.
    #[error("DEX RPC {0} returned no result")]
    RpcNoResult(String),
    /// The block returned by `eth_getBlockByNumber` has a different block number.
    #[error("DEX RPC returned wrong block number")]
    WrongBlockNumber,
    /// A hex quantity string lacks the `0x` prefix.
    #[error("invalid RPC quantity prefix")]
    InvalidQuantityPrefix,
    /// A JSON value lacks a required hex quantity string.
    #[error("missing RPC quantity")]
    MissingQuantity,
    /// The DEX provider name and chain ID pair is not supported.
    #[error("DEX v1 requires uniswap on chain 1 or pancakeswap on chain 56")]
    UnsupportedChainOrName,
    /// The DEX `rpc_endpoint` is not a valid URL.
    #[error("invalid DEX RPC URL")]
    InvalidRpcUrl,
    /// The DEX `rpc_endpoint` does not use HTTP or HTTPS.
    #[error("DEX RPC requires HTTP(S)")]
    NonHttpRpcUrl,
    /// The configured `poll_interval_secs` is outside `1..=10`.
    #[error("DEX poll_interval_secs must be 1..=10")]
    InvalidPollInterval,
    /// The configured `log_chunk_blocks` is outside `1..=10000`.
    #[error("DEX log_chunk_blocks must be 1..=10000")]
    InvalidLogChunkBlocks,
    /// The configured `confirmations` exceeds `1000`.
    #[error("DEX confirmations must be 0..=1000")]
    InvalidConfirmations,
    /// The configured `max_block_age_secs` is zero.
    #[error("DEX max_block_age_secs must be positive")]
    NonPositiveMaxBlockAge,
    /// The DEX pool is not attached to exactly one currency pair.
    #[error("DEX provider {0} pool is not attached to a pair")]
    UnattachedPool(String),
    /// The base and quote token addresses are zero or identical.
    #[error("DEX requires two distinct ERC20 addresses")]
    InvalidTokenAddresses,
    /// The same market key appears more than once in the DEX provider.
    #[error("duplicate DEX market {0}")]
    DuplicateMarket(String),
    /// The configuration lists the same pool address and pool ID pair more than once.
    #[error("DEX pool configured more than once")]
    DuplicatePool,
    /// The pool or manager address is the zero address.
    #[error("DEX pool/manager address must be nonzero")]
    ZeroPoolAddress,
    /// The pool protocol does not match the DEX provider family.
    #[error("DEX protocol does not match provider")]
    ProtocolMismatch,
    /// A Uniswap V4 pool has a zero `StateView` address.
    #[error("Uniswap V4 requires StateView")]
    MissingStateView,
    /// A Uniswap V4 pool has a tick spacing outside `1..=32767`.
    #[error("invalid V4 tick spacing")]
    InvalidV4TickSpacing,
    /// A PancakeSwap Infinity CL pool has invalid encoded parameters.
    #[error("invalid Infinity CL parameters")]
    InvalidInfinityClParameters,
    /// A PancakeSwap Infinity Bin pool has invalid encoded parameters.
    #[error("invalid Infinity Bin parameters")]
    InvalidInfinityBinParameters,
    /// A pool fee exceeds `1_000_000` and is not the dynamic fee flag.
    #[error("invalid pool fee")]
    InvalidPoolFee,
    /// Two `[[dex_providers]]` entries share the same name.
    #[error("duplicate DEX provider {0}")]
    DuplicateDexProvider(String),
    /// A DEX provider also appears in `[[api_providers]]`.
    #[error("configure DEX RPC in dex_providers, not api_providers")]
    ConfiguredInApiProviders,
    /// A currency pair references a DEX provider that is not in the configuration.
    #[error("missing DEX configuration for {0}")]
    MissingDexConfig(String),
    /// The raw pool price numerator or denominator is zero.
    #[error("uninitialized or zero pool price")]
    ZeroPoolPrice,
    /// A token has more than 77 decimals in math scaling.
    #[error("unsupported token decimals (>77)")]
    UnsupportedTokenDecimals,
    /// The scaled rate numerator overflowed `U1024`.
    #[error("DEX rate numerator overflow")]
    RateNumeratorOverflow,
    /// The scaled rate denominator overflowed `U1024`.
    #[error("DEX rate denominator overflow")]
    RateDenominatorOverflow,
    /// The computed DEX rate is zero or exceeds `U256::MAX`.
    #[error("DEX rate outside FP18 range")]
    RateOutOfRange,
    /// The scaled DEX volume exceeds `U256::MAX`.
    #[error("DEX volume outside FP18 range")]
    VolumeOutOfRange,
    /// An Infinity Bin pool has a zero bin step.
    #[error("zero bin step")]
    ZeroBinStep,
    /// An Infinity Bin active ID exponent exceeds the supported range.
    #[error("Infinity bin exponent out of range")]
    BinExponentOutOfRange,
    /// An Infinity Bin price calculation underflowed to zero.
    #[error("Infinity bin price underflow")]
    BinPriceUnderflow,
    /// The Uniswap V4 `StateView` contract points to a different `PoolManager`.
    #[error("StateView belongs to a different PoolManager")]
    StateViewManagerMismatch,
    /// The pool contract `token0` and `token1` do not match the configured tokens.
    #[error("configured DEX tokens do not match pool")]
    PoolTokenMismatch,
    /// A pool token contract returned more than 77 decimals.
    #[error("unsupported DEX token decimals (>77)")]
    UnsupportedDexTokenDecimals,
    /// A swap log has the removed flag or comes from an unexpected contract address.
    #[error("invalid DEX log address/removal flag")]
    InvalidLogAddressOrRemoved,
    /// A swap log does not have 3 topics or its first topic is not the Swap event.
    #[error("invalid DEX Swap topics")]
    InvalidSwapTopics,
    /// A manager Swap event has a pool ID topic from another pool.
    #[error("DEX Swap belongs to another pool")]
    SwapPoolIdMismatch,
    /// The DEX RPC chain ID does not match the configured `chain_id`.
    #[error("DEX RPC chain ID mismatch")]
    ChainIdMismatch,
    /// The target DEX block timestamp is stale or too far in the future.
    #[error("DEX block is stale or in the future")]
    StaleOrFutureBlock,
    /// A reorg changed the previously validated cursor block.
    #[error("DEX chain history changed; rebuilding volume")]
    ChainHistoryChanged,
    /// The target DEX block changed while the worker collected swap logs.
    #[error("DEX block changed during collection")]
    BlockChangedDuringCollection,
    /// The rolling swap volume sum overflowed `U256`.
    #[error("DEX rolling volume overflow")]
    RollingVolumeOverflow,
    /// Incrementing the cursor block number overflowed `u64`.
    #[error("DEX block number overflow")]
    BlockNumberOverflow,
    /// The end block of a log chunk has a timestamp after the head block.
    #[error("DEX log range extends past the head time")]
    LogRangePastHeadTime,
    /// A swap log block number is outside the requested `[from, to]` range.
    #[error("DEX log outside requested range")]
    LogOutsideRange,
    /// Two swap logs share the same block hash and log index with different data.
    #[error("conflicting duplicate DEX log")]
    ConflictingDuplicateLog,
    /// The chunk map does not contain the block header for a swap log.
    #[error("missing DEX log block")]
    MissingLogBlock,
    /// A swap log block hash does not match the canonical block header.
    #[error("DEX log is not in the canonical history")]
    LogNotCanonical,
    /// The swap volume sum for a single block overflowed `U256`.
    #[error("DEX block volume overflow")]
    BlockVolumeOverflow,
}
