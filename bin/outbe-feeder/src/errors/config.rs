//! Error types for feeder TOML configuration validation.

use thiserror::Error;

/// Errors that occur when the feeder resolves or validates its configuration.
#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    /// A provider market references a currency pair that does not exist.
    #[error("provider {provider} names unknown pair {named}")]
    UnknownNamedPair {
        /// The provider name.
        provider: String,
        /// The pair name from the market entry.
        named: String,
    },
    /// A provider market quote does not map to any Oracle quote currency.
    #[error("provider {provider} market quote '{quote}' names no Oracle pair")]
    MarketQuoteNamesNoPair {
        /// The provider name.
        provider: String,
        /// The market quote symbol.
        quote: String,
    },
    /// No currency pair uses the Oracle quote that the provider market maps to.
    #[error("provider {provider} market {quote} needs a currency pair quoted in {oracle_quote}")]
    MissingPairForQuote {
        /// The provider name.
        provider: String,
        /// The market quote symbol.
        quote: String,
        /// The resolved Oracle quote symbol.
        oracle_quote: String,
    },
    /// Multiple currency pairs share the same Oracle quote without an explicit pair name.
    #[error("several pairs are quoted in {oracle_quote}: give provider {provider} market {quote} a pair")]
    AmbiguousPairForQuote {
        /// The resolved Oracle quote symbol.
        oracle_quote: String,
        /// The provider name.
        provider: String,
        /// The market quote symbol.
        quote: String,
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
    /// A provider other than `redstone` sets `api_key`.
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
