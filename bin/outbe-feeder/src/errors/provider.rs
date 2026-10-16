//! Error types for provider initialization, mock HTTP, on-chain feeds, and RedStone.

use alloy_primitives::{Address, U256};
use reqwest::StatusCode;
use thiserror::Error;

use crate::provider::ObservationError;

/// Errors that occur when the feeder constructs price providers.
#[derive(Debug, Error)]
pub(crate) enum ProviderError {
    /// The `mock_http` provider requires an entry in `[[api_providers]]`.
    #[error("provider mock_http requires a [[api_providers]] entry")]
    MissingMockHttpProviderEntry,
    /// A DEX provider requires an entry in `[[dex_providers]]`.
    #[error("provider {0} requires a [[dex_providers]] entry")]
    MissingDexProviderEntry(String),
}

/// Errors that occur in the `mock_http` provider.
#[derive(Debug, Error)]
pub(crate) enum MockHttpError {
    /// The ticker HTTP endpoint returned a non-success status code.
    #[error("mock_http ticker endpoint returned status {0}")]
    TickerStatus(StatusCode),
    /// The ticker response contains two entries for the same symbol.
    #[error("duplicate mock_http ticker for {0}")]
    DuplicateTicker(String),
    /// A ticker entry has an invalid or zero price or volume.
    #[error("invalid mock_http ticker for {symbol}: {source}")]
    InvalidTicker {
        /// The uppercase symbol name.
        symbol: String,
        /// The underlying observation error.
        source: ObservationError,
    },
    /// The ticker response omitted a requested symbol.
    #[error("missing mock_http ticker for {0}")]
    MissingTicker(String),
    /// The candle HTTP endpoint returned a non-success status code.
    #[error("mock_http candle endpoint returned status {0}")]
    CandleStatus(StatusCode),
    /// A candle entry has an invalid or zero price or volume.
    #[error("invalid mock_http candle for {symbol}: {source}")]
    InvalidCandle {
        /// The uppercase symbol name.
        symbol: String,
        /// The underlying observation error.
        source: ObservationError,
    },
    /// The candle response omitted a requested symbol.
    #[error("missing mock_http candles for {0}")]
    MissingCandles(String),
}

/// Errors that occur in `onchain_feed` configuration or feed reads.
#[derive(Debug, Error)]
pub(crate) enum OnchainFeedError {
    /// The provider name is empty or contains whitespace.
    #[error("onchain_feed provider name must be a single non-empty word")]
    InvalidProviderName,
    /// The configured `chain_id` is zero.
    #[error("onchain_feed chain_id must be positive")]
    NonPositiveChainId,
    /// The configured `rpc_endpoint` is not a valid URL.
    #[error("invalid onchain_feed RPC URL")]
    InvalidRpcUrl,
    /// The configured `rpc_endpoint` does not use HTTP or HTTPS.
    #[error("onchain_feed RPC requires HTTP(S)")]
    NonHttpRpcUrl,
    /// The feed is not attached to any currency pair.
    #[error("onchain_feed provider {0} feed is not attached to a pair")]
    UnattachedFeed(String),
    /// A feed key does not have the `BASE/QUOTE` format.
    #[error("onchain_feed feed key {key:?} in provider {name} must be BASE/QUOTE")]
    InvalidFeedKey {
        /// The invalid feed key.
        key: String,
        /// The provider name.
        name: String,
    },
    /// A feed has the zero address as its contract address.
    #[error("onchain_feed feed {key} in provider {name} has no contract address")]
    ZeroContractAddress {
        /// The feed key.
        key: String,
        /// The provider name.
        name: String,
    },
    /// Two feeds in the same provider use the same contract address.
    #[error("contract {contract} configured twice in onchain_feed provider {name}")]
    DuplicateContractAddress {
        /// The duplicate contract address.
        contract: Address,
        /// The provider name.
        name: String,
    },
    /// Two `[[onchain_providers]]` entries share the same name.
    #[error("duplicate onchain_providers entry '{0}'")]
    DuplicateProviderEntry(String),
    /// An `[[onchain_providers]]` entry uses a reserved built-in provider name.
    #[error("onchain_providers name '{0}' is a built-in provider")]
    ReservedProviderName(String),
    /// An `[[onchain_providers]]` entry also appears in `[[api_providers]]`.
    #[error("configure the feed RPC in onchain_providers, not api_providers")]
    ConfiguredInApiProviders,
    /// The RPC endpoint returned a chain ID that differs from the configuration.
    #[error("RPC chain ID mismatch for chain {0}")]
    ChainIdMismatch(u64),
    /// The feed contract returned more than 77 decimals.
    #[error("unsupported feed decimals (>77)")]
    UnsupportedDecimals,
    /// The latest round answer is zero or negative.
    #[error("feed answer is not positive")]
    NonPositiveAnswer,
    /// The round `updatedAt` timestamp is too far in the future.
    #[error("feed updatedAt is in the future")]
    FutureUpdatedAt,
    /// The round `updatedAt` timestamp is older than the maximum allowed age.
    #[error("feed round is stale ({age} s > {max_age} s)")]
    StaleRound {
        /// The age of the round in seconds.
        age: U256,
        /// The maximum allowed age in seconds.
        max_age: u64,
    },
}

/// Errors that occur in the `redstone` provider.
#[derive(Debug, Error)]
pub(crate) enum RedstoneError {
    /// The gateway returned an empty package list for the feed.
    #[error("no packages")]
    NoPackages,
    /// The newest package timestamp is too far in the future.
    #[error("package timestamp is in the future")]
    FuturePackageTimestamp,
    /// The newest package timestamp is older than the maximum allowed age.
    #[error("packages are stale ({0} s)")]
    StalePackages(u64),
    /// A package from a registered signer has no valid positive price for the feed.
    #[error("package from {0} has no valid value")]
    InvalidPackageValue(Address),
    /// Fewer than the required number of registered signers reported a valid price.
    #[error("{found} registered signers, need {needed}")]
    InsufficientSigners {
        /// The number of valid registered signers found.
        found: usize,
        /// The minimum required number of unique signers.
        needed: usize,
    },
    /// The RedStone HTTP gateway returned a non-success status code.
    #[error("redstone gateway returned status {0}")]
    GatewayStatus(StatusCode),
    /// The gateway response map does not contain the requested feed ID.
    #[error("feed missing from response")]
    FeedMissingFromResponse,
}

/// Errors that occur in the `pyth` provider.
#[derive(Debug, Error)]
pub(crate) enum PythError {
    /// The `pyth` provider configuration has no `api_key`.
    #[error("api provider pyth needs an api_key")]
    MissingApiKey,
    /// Hermes returned a non-success status code.
    #[error("pyth {path} returned status {status}: {body}")]
    Status {
        /// The request path without the query.
        path: String,
        /// The HTTP status code.
        status: StatusCode,
        /// The first 200 characters of the response body.
        body: String,
    },
}
