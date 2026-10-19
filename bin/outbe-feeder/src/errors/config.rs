//! Error types for feeder TOML configuration validation.

use thiserror::Error;

/// Errors that occur when the feeder resolves or validates its configuration.
#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    /// The top-level `pairs` key is not a string.
    #[error("`pairs` must be a file path")]
    PairsPathNotString,
    /// A top-level key appears in both the main config and the pairs file.
    #[error("`{key}` is set in both {main} and {pairs}")]
    SectionInBothFiles {
        /// The duplicated top-level key.
        key: String,
        /// The main config path.
        main: String,
        /// The pairs file path.
        pairs: String,
    },
    /// The node has no sources file for its network.
    #[error("the node has no feeder sources for this network. Set `oracle.sources = \"local\"`")]
    NoNodeSources,
    /// The local config sets a section that the node supplies in `chain` sources mode.
    #[error(
        "remove `{0}` from the local config: the node supplies it when `oracle.sources` is `chain`"
    )]
    SharedSectionInLocalConfig(&'static str),
    /// A DEX or on-chain feed entry has no RPC URL.
    #[error("`{name}` has no `rpc_endpoint`, and `rpc_endpoints` has no URL for chain {chain_id}")]
    MissingRpcEndpoint {
        /// The entry name.
        name: String,
        /// The entry's EVM chain id.
        chain_id: u64,
    },
    /// A cross pair does not have a COEN or token base, an ISO quote and an ISO cross.
    #[error("cross pair {base}/{quote} with cross '{cross}' needs a COEN or token base, an ISO quote and an ISO cross")]
    InvalidCrossPair {
        /// The base asset symbol.
        base: String,
        /// The quote asset symbol.
        quote: String,
        /// The cross currency.
        cross: String,
    },
    /// A cross pair has no direct `<base>/<cross>` pair to divide.
    #[error("cross pair {base}/{quote} needs a direct {base}/{cross} pair")]
    MissingCrossLeg {
        /// The base asset symbol.
        base: String,
        /// The quote asset symbol.
        quote: String,
        /// The cross currency.
        cross: String,
    },
    /// An exchange or a DEX pool feeds a currency rate.
    #[error("provider '{provider}' cannot feed {base}/{quote}: exchanges and DEX pools are not for cross-currency rates")]
    CurrencyRateFromMarket {
        /// The provider name.
        provider: String,
        /// The pair base symbol.
        base: String,
        /// The pair quote symbol.
        quote: String,
    },
    /// A market is priced in another currency than its pair.
    #[error(
        "provider '{provider}' market quote '{quote}' does not price pair {base}/{pair_quote}"
    )]
    MarketQuoteMismatch {
        /// The provider name.
        provider: String,
        /// The market quote symbol.
        quote: String,
        /// The pair base symbol.
        base: String,
        /// The pair quote symbol.
        pair_quote: String,
    },
    /// The configured `oracle.vote_period` is zero.
    #[error("oracle.vote_period must be > 0, got 0 (would cause division by zero)")]
    ZeroVotePeriod,
    /// The configured `account.validator_address` is not a 20-byte hex address.
    #[error("account.validator_address is not a valid 20-byte hex address: {0}")]
    InvalidValidatorAddress(String),
    /// A currency pair uses the same asset as both base and quote.
    #[error("Oracle pair {base}/{quote} uses the same asset twice")]
    SameAssetPair {
        /// The base asset symbol.
        base: String,
        /// The quote asset symbol.
        quote: String,
    },
    /// A currency pair places an ISO currency as base and COEN as quote.
    #[error("ISO/COEN pair {base}/{quote} is invalid; configure COEN/ISO")]
    InvalidIsoCoenOrientation {
        /// The base asset symbol.
        base: String,
        /// The quote asset symbol.
        quote: String,
    },
    /// A currency pair duplicates or inverts an earlier pair.
    #[error("duplicate or inverse Oracle pair {base}/{quote}")]
    DuplicateOrInversePair {
        /// The base asset symbol.
        base: String,
        /// The quote asset symbol.
        quote: String,
    },
    /// A currency pair has no configured market sources.
    #[error("currency pair {base}/{quote} has no sources configured")]
    NoSourcesConfigured {
        /// The base asset symbol.
        base: String,
        /// The quote asset symbol.
        quote: String,
    },
    /// A currency pair source references an unknown provider.
    #[error("unknown provider '{provider}' for pair {base}/{quote}. Known: {known:?} or an onchain_providers entry name")]
    UnknownPairProvider {
        /// The unknown provider name.
        provider: String,
        /// The base asset symbol.
        base: String,
        /// The quote asset symbol.
        quote: String,
        /// The list of known built-in provider names.
        known: &'static [&'static str],
    },
    /// A currency pair lists the same provider market more than once.
    #[error("duplicate provider source '{provider}' {source_base}/{source_quote} for pair {pair_base}/{pair_quote}")]
    DuplicateProviderSource {
        /// The provider name.
        provider: String,
        /// The source base symbol.
        source_base: String,
        /// The source quote symbol.
        source_quote: String,
        /// The pair base symbol.
        pair_base: String,
        /// The pair quote symbol.
        pair_quote: String,
    },
    /// An `api_providers` entry uses an unknown provider name.
    #[error("unknown api provider '{name}'. Known: {known:?}")]
    UnknownApiProvider {
        /// The unknown provider name.
        name: String,
        /// The list of known built-in provider names.
        known: &'static [&'static str],
    },
    /// Two `api_providers` entries share the same name.
    #[error("duplicate api provider '{0}'")]
    DuplicateApiProvider(String),
    /// A provider other than `redstone` or `pyth` sets `api_key`.
    #[error("provider '{0}' takes no api_key")]
    UnexpectedApiKey(String),
    /// A non-streaming provider sets a WebSocket endpoint override.
    #[error("provider '{0}' does not support websocket market streams")]
    WebsocketNotSupported(String),
    /// A provider sets an invalid WebSocket endpoint URL or host.
    #[error("provider '{name}' has invalid websocket endpoint '{websocket}'")]
    InvalidWebsocketEndpoint {
        /// The provider name.
        name: String,
        /// The invalid WebSocket endpoint string.
        websocket: String,
    },
    /// An asset string is neither `COEN`, a valid ISO 4217 numeric code, nor an address.
    #[error("unknown on-chain Oracle asset '{0}'")]
    UnknownOracleAsset(String),
}
