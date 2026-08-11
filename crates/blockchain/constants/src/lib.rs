//! Immutable, genesis-backed protocol parameters shared by every runtime module.

use alloy_primitives::{address, keccak256, Address, B256, U256};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result as RuntimeResult},
    storage::{types::Slot, StorageHandle},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock, RwLock},
};
use thiserror::Error;

pub const GENESIS_CONFIG_KEY: &str = "outbeProtocol";
pub const PROTOCOL_CONSTANTS_SCHEMA_VERSION: u16 = 1;

pub const DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS: u64 = 50 * 60 * 60;
pub const DEFAULT_METADOSIS_LOOKBACK_DELAY_SECONDS: u64 = 502 * 60 * 60;
pub const DEFAULT_METADOSIS_OFFERING_PERIOD_SECONDS: u64 = 50 * 60 * 60;
pub const DEFAULT_METADOSIS_WAITING_PERIOD_SECONDS: u64 = 12 * 60 * 60;
pub const DEFAULT_METADOSIS_BOOTSTRAP_DURATION_SECONDS: u64 = 504 * 60 * 60;
pub const DEFAULT_METADOSIS_ADVANCE_INTERVAL_SECONDS: u64 = 12 * 60 * 60;
pub const DEFAULT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS: u64 = 1_800;

/// Immutable system-owned account containing the resolved genesis parameters.
/// It has marker bytecode but no public precompile dispatch or runtime writer.
pub const CHAIN_CONSTANTS_ADDRESS: Address = address!("0x000000000000000000000000000000000000EE11");
pub const CHAIN_CONSTANTS_MARKER_CODE: [u8; 1] = [0xef];

pub const SLOT_SCHEMA_VERSION: u64 = 0;
pub const SLOT_METADOSIS_FORMING_PERIOD_SECONDS: u64 = 1;
pub const SLOT_METADOSIS_LOOKBACK_DELAY_SECONDS: u64 = 2;
pub const SLOT_METADOSIS_OFFERING_PERIOD_SECONDS: u64 = 3;
pub const SLOT_METADOSIS_WAITING_PERIOD_SECONDS: u64 = 4;
pub const SLOT_METADOSIS_BOOTSTRAP_DURATION_SECONDS: u64 = 5;
pub const SLOT_METADOSIS_ADVANCE_INTERVAL_SECONDS: u64 = 6;
pub const SLOT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS: u64 = 7;
pub const SLOT_PARAMETERS_HASH: u64 = 8;
pub const SLOT_INSTALLED: u64 = 9;
pub const STORAGE_SLOT_COUNT: usize = 10;

const PARAMETERS_HASH_DOMAIN_V1: &[u8] = b"OUTBE_CHAIN_PROTOCOL_CONSTANTS_V1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenesisProtocolParametersV1 {
    pub metadosis_forming_period_seconds: u64,
    pub metadosis_lookback_delay_seconds: u64,
    pub metadosis_offering_period_seconds: u64,
    pub metadosis_waiting_period_seconds: u64,
    pub metadosis_bootstrap_duration_seconds: u64,
    pub metadosis_advance_interval_seconds: u64,
    pub ocomp_compute_vote_window_blocks: u64,
}

impl Default for GenesisProtocolParametersV1 {
    fn default() -> Self {
        Self {
            metadosis_forming_period_seconds: DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS,
            metadosis_lookback_delay_seconds: DEFAULT_METADOSIS_LOOKBACK_DELAY_SECONDS,
            metadosis_offering_period_seconds: DEFAULT_METADOSIS_OFFERING_PERIOD_SECONDS,
            metadosis_waiting_period_seconds: DEFAULT_METADOSIS_WAITING_PERIOD_SECONDS,
            metadosis_bootstrap_duration_seconds: DEFAULT_METADOSIS_BOOTSTRAP_DURATION_SECONDS,
            metadosis_advance_interval_seconds: DEFAULT_METADOSIS_ADVANCE_INTERVAL_SECONDS,
            ocomp_compute_vote_window_blocks: DEFAULT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenesisProtocolOverridesV1 {
    #[serde(default)]
    schema_version: Option<u16>,
    #[serde(default)]
    metadosis: MetadosisOverridesV1,
    #[serde(default)]
    ocomp: OcompOverridesV1,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MetadosisOverridesV1 {
    forming_period_seconds: Option<u64>,
    lookback_delay_seconds: Option<u64>,
    offering_period_seconds: Option<u64>,
    waiting_period_seconds: Option<u64>,
    bootstrap_duration_seconds: Option<u64>,
    advance_interval_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OcompOverridesV1 {
    compute_vote_window_blocks: Option<u64>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProtocolConstantsError {
    #[error("invalid genesis {GENESIS_CONFIG_KEY}: {0}")]
    InvalidJson(String),
    #[error("unsupported protocol constants schema version {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("protocol constant {field} must be {requirement}, got {actual}")]
    InvalidValue {
        field: &'static str,
        requirement: &'static str,
        actual: u64,
    },
    #[error("invalid materialized protocol constants genesis record: {0}")]
    InvalidGenesisStorage(String),
}

impl GenesisProtocolParametersV1 {
    pub fn resolve(value: Option<&serde_json::Value>) -> Result<Self, ProtocolConstantsError> {
        let overrides = match value {
            Some(value) => serde_json::from_value::<GenesisProtocolOverridesV1>(value.clone())
                .map_err(|error| ProtocolConstantsError::InvalidJson(error.to_string()))?,
            None => GenesisProtocolOverridesV1::default(),
        };
        if let Some(version) = overrides.schema_version {
            if version != PROTOCOL_CONSTANTS_SCHEMA_VERSION {
                return Err(ProtocolConstantsError::UnsupportedSchemaVersion(version));
            }
        }
        let defaults = Self::default();
        let resolved = Self {
            metadosis_forming_period_seconds: overrides
                .metadosis
                .forming_period_seconds
                .unwrap_or(defaults.metadosis_forming_period_seconds),
            metadosis_lookback_delay_seconds: overrides
                .metadosis
                .lookback_delay_seconds
                .unwrap_or(defaults.metadosis_lookback_delay_seconds),
            metadosis_offering_period_seconds: overrides
                .metadosis
                .offering_period_seconds
                .unwrap_or(defaults.metadosis_offering_period_seconds),
            metadosis_waiting_period_seconds: overrides
                .metadosis
                .waiting_period_seconds
                .unwrap_or(defaults.metadosis_waiting_period_seconds),
            metadosis_bootstrap_duration_seconds: overrides
                .metadosis
                .bootstrap_duration_seconds
                .unwrap_or(defaults.metadosis_bootstrap_duration_seconds),
            metadosis_advance_interval_seconds: overrides
                .metadosis
                .advance_interval_seconds
                .unwrap_or(defaults.metadosis_advance_interval_seconds),
            ocomp_compute_vote_window_blocks: overrides
                .ocomp
                .compute_vote_window_blocks
                .unwrap_or(defaults.ocomp_compute_vote_window_blocks),
        };
        resolved.validate()?;
        Ok(resolved)
    }

    pub fn validate(self) -> Result<(), ProtocolConstantsError> {
        validate_nonzero_at_most(
            "metadosis.formingPeriodSeconds",
            self.metadosis_forming_period_seconds,
            DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS,
        )?;
        validate_at_most(
            "metadosis.lookbackDelaySeconds",
            self.metadosis_lookback_delay_seconds,
            DEFAULT_METADOSIS_LOOKBACK_DELAY_SECONDS,
        )?;
        validate_nonzero_at_most(
            "metadosis.offeringPeriodSeconds",
            self.metadosis_offering_period_seconds,
            DEFAULT_METADOSIS_OFFERING_PERIOD_SECONDS,
        )?;
        validate_nonzero_at_most(
            "metadosis.waitingPeriodSeconds",
            self.metadosis_waiting_period_seconds,
            DEFAULT_METADOSIS_WAITING_PERIOD_SECONDS,
        )?;
        validate_nonzero_at_most(
            "metadosis.bootstrapDurationSeconds",
            self.metadosis_bootstrap_duration_seconds,
            DEFAULT_METADOSIS_BOOTSTRAP_DURATION_SECONDS,
        )?;
        validate_nonzero_at_most(
            "metadosis.advanceIntervalSeconds",
            self.metadosis_advance_interval_seconds,
            DEFAULT_METADOSIS_ADVANCE_INTERVAL_SECONDS,
        )?;
        validate_nonzero_at_most(
            "ocomp.computeVoteWindowBlocks",
            self.ocomp_compute_vote_window_blocks,
            DEFAULT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS,
        )?;

        self.metadosis_forming_period_seconds
            .checked_add(self.metadosis_lookback_delay_seconds)
            .and_then(|value| value.checked_add(self.metadosis_offering_period_seconds))
            .and_then(|value| value.checked_add(self.metadosis_waiting_period_seconds))
            .ok_or(ProtocolConstantsError::InvalidValue {
                field: "metadosis pipeline",
                requirement: "fit u64 seconds",
                actual: u64::MAX,
            })?;
        Ok(())
    }

    #[must_use]
    pub fn parameters_hash(self) -> B256 {
        let mut encoded = Vec::with_capacity(PARAMETERS_HASH_DOMAIN_V1.len() + 2 + 7 * 8);
        encoded.extend_from_slice(PARAMETERS_HASH_DOMAIN_V1);
        encoded.extend_from_slice(&PROTOCOL_CONSTANTS_SCHEMA_VERSION.to_be_bytes());
        for value in [
            self.metadosis_forming_period_seconds,
            self.metadosis_lookback_delay_seconds,
            self.metadosis_offering_period_seconds,
            self.metadosis_waiting_period_seconds,
            self.metadosis_bootstrap_duration_seconds,
            self.metadosis_advance_interval_seconds,
            self.ocomp_compute_vote_window_blocks,
        ] {
            encoded.extend_from_slice(&value.to_be_bytes());
        }
        keccak256(encoded)
    }

    /// Exact genesis storage words in ordinal order. Genesis tooling is the only
    /// production caller: runtime mutation is deliberately not exposed.
    #[must_use]
    pub fn genesis_storage_words(self) -> [(U256, U256); STORAGE_SLOT_COUNT] {
        [
            (
                U256::from(SLOT_SCHEMA_VERSION),
                U256::from(PROTOCOL_CONSTANTS_SCHEMA_VERSION),
            ),
            (
                U256::from(SLOT_METADOSIS_FORMING_PERIOD_SECONDS),
                U256::from(self.metadosis_forming_period_seconds),
            ),
            (
                U256::from(SLOT_METADOSIS_LOOKBACK_DELAY_SECONDS),
                U256::from(self.metadosis_lookback_delay_seconds),
            ),
            (
                U256::from(SLOT_METADOSIS_OFFERING_PERIOD_SECONDS),
                U256::from(self.metadosis_offering_period_seconds),
            ),
            (
                U256::from(SLOT_METADOSIS_WAITING_PERIOD_SECONDS),
                U256::from(self.metadosis_waiting_period_seconds),
            ),
            (
                U256::from(SLOT_METADOSIS_BOOTSTRAP_DURATION_SECONDS),
                U256::from(self.metadosis_bootstrap_duration_seconds),
            ),
            (
                U256::from(SLOT_METADOSIS_ADVANCE_INTERVAL_SECONDS),
                U256::from(self.metadosis_advance_interval_seconds),
            ),
            (
                U256::from(SLOT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS),
                U256::from(self.ocomp_compute_vote_window_blocks),
            ),
            (
                U256::from(SLOT_PARAMETERS_HASH),
                U256::from_be_slice(self.parameters_hash().as_slice()),
            ),
            (U256::from(SLOT_INSTALLED), U256::from(1)),
        ]
    }

    /// Read the already-materialized immutable record from genesis JSON.
    /// Genesis tools use this after `constants genesis`; runtime modules use
    /// [`load`] and never parse the JSON configuration source.
    pub fn from_materialized_genesis(
        genesis: &serde_json::Value,
    ) -> Result<Self, ProtocolConstantsError> {
        let alloc = genesis
            .get("alloc")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| invalid_genesis("alloc must be an object"))?;
        let wanted = normalized_address(&format!("{CHAIN_CONSTANTS_ADDRESS:#x}"));
        let account = alloc
            .iter()
            .find(|(address, _)| normalized_address(address) == wanted)
            .map(|(_, account)| account)
            .ok_or_else(|| invalid_genesis("constants account is missing"))?;
        if account.get("code").and_then(serde_json::Value::as_str) != Some("0xef") {
            return Err(invalid_genesis("constants account marker code is invalid"));
        }
        let storage = account
            .get("storage")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| invalid_genesis("constants account storage must be an object"))?;
        let read = |ordinal: u64| -> Result<U256, ProtocolConstantsError> {
            let wanted = U256::from(ordinal);
            let raw = storage
                .iter()
                .find_map(|(slot, value)| {
                    parse_u256(slot)
                        .ok()
                        .filter(|slot| *slot == wanted)
                        .map(|_| value)
                })
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| invalid_genesis(format!("slot {ordinal} is missing")))?;
            parse_u256(raw)
        };

        let schema = read(SLOT_SCHEMA_VERSION)?;
        if schema != U256::from(PROTOCOL_CONSTANTS_SCHEMA_VERSION) {
            return Err(invalid_genesis("schema version is unsupported"));
        }
        if read(SLOT_INSTALLED)? != U256::from(1) {
            return Err(invalid_genesis("installed marker is not one"));
        }
        let resolved = Self {
            metadosis_forming_period_seconds: word_u64(
                read(SLOT_METADOSIS_FORMING_PERIOD_SECONDS)?,
                "forming period",
            )?,
            metadosis_lookback_delay_seconds: word_u64(
                read(SLOT_METADOSIS_LOOKBACK_DELAY_SECONDS)?,
                "lookback delay",
            )?,
            metadosis_offering_period_seconds: word_u64(
                read(SLOT_METADOSIS_OFFERING_PERIOD_SECONDS)?,
                "offering period",
            )?,
            metadosis_waiting_period_seconds: word_u64(
                read(SLOT_METADOSIS_WAITING_PERIOD_SECONDS)?,
                "waiting period",
            )?,
            metadosis_bootstrap_duration_seconds: word_u64(
                read(SLOT_METADOSIS_BOOTSTRAP_DURATION_SECONDS)?,
                "bootstrap duration",
            )?,
            metadosis_advance_interval_seconds: word_u64(
                read(SLOT_METADOSIS_ADVANCE_INTERVAL_SECONDS)?,
                "advance interval",
            )?,
            ocomp_compute_vote_window_blocks: word_u64(
                read(SLOT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS)?,
                "OCOMP vote window",
            )?,
        };
        resolved.validate()?;
        let stored_hash = B256::from(read(SLOT_PARAMETERS_HASH)?.to_be_bytes::<32>());
        if stored_hash != resolved.parameters_hash() {
            return Err(invalid_genesis("parameters hash mismatch"));
        }
        Ok(resolved)
    }
}

fn normalized_address(value: &str) -> String {
    value
        .strip_prefix("0x")
        .unwrap_or(value)
        .trim_start_matches('0')
        .to_ascii_lowercase()
}

fn parse_u256(value: &str) -> Result<U256, ProtocolConstantsError> {
    U256::from_str_radix(value.strip_prefix("0x").unwrap_or(value), 16)
        .map_err(|error| invalid_genesis(format!("invalid U256 {value}: {error}")))
}

fn word_u64(value: U256, label: &str) -> Result<u64, ProtocolConstantsError> {
    u64::try_from(value).map_err(|_| invalid_genesis(format!("{label} does not fit u64")))
}

fn invalid_genesis(message: impl Into<String>) -> ProtocolConstantsError {
    ProtocolConstantsError::InvalidGenesisStorage(message.into())
}

static RESOLVED_BY_GENESIS: OnceLock<RwLock<BTreeMap<B256, Arc<GenesisProtocolParametersV1>>>> =
    OnceLock::new();

/// Load the immutable parameters for this chain. A non-zero genesis identity
/// is cached process-wide by genesis hash; because the record is part of the
/// genesis state root, equal keys necessarily name equal parameter records.
pub fn load(ctx: &BlockRuntimeContext<'_>) -> RuntimeResult<Arc<GenesisProtocolParametersV1>> {
    let genesis_hash = ctx.block.genesis_hash;
    // `BlockContext::empty_for_tests` deliberately has no chain identity and
    // predates genesis materialization. Preserve those isolated unit contexts
    // without weakening production: every real node supplies a non-zero
    // ChainSpec genesis hash and therefore takes the strict storage path below.
    if genesis_hash.is_zero() && !slot::<bool>(ctx.storage.clone(), SLOT_INSTALLED).read()? {
        return Ok(Arc::new(GenesisProtocolParametersV1::default()));
    }
    if !genesis_hash.is_zero() {
        let cache = RESOLVED_BY_GENESIS.get_or_init(|| RwLock::new(BTreeMap::new()));
        let cached = cache
            .read()
            .map_err(|_| fatal("chain constants cache read lock poisoned"))?
            .get(&genesis_hash)
            .cloned();
        if let Some(cached) = cached {
            return Ok(cached);
        }
    }

    let loaded = Arc::new(load_uncached(ctx.storage.clone())?);
    if genesis_hash.is_zero() {
        return Ok(loaded);
    }
    let cache = RESOLVED_BY_GENESIS.get_or_init(|| RwLock::new(BTreeMap::new()));
    let mut entries = cache
        .write()
        .map_err(|_| fatal("chain constants cache write lock poisoned"))?;
    match entries.get(&genesis_hash) {
        Some(existing) if existing.as_ref() == loaded.as_ref() => Ok(existing.clone()),
        Some(_) => Err(fatal(
            "one genesis hash resolved to conflicting chain constants",
        )),
        None => {
            entries.insert(genesis_hash, loaded.clone());
            Ok(loaded)
        }
    }
}

pub fn load_uncached(storage: StorageHandle<'_>) -> RuntimeResult<GenesisProtocolParametersV1> {
    let installed = slot::<bool>(storage.clone(), SLOT_INSTALLED).read()?;
    if !installed {
        return Err(fatal("chain constants are absent from genesis storage"));
    }
    let schema_version = slot::<u16>(storage.clone(), SLOT_SCHEMA_VERSION).read()?;
    if schema_version != PROTOCOL_CONSTANTS_SCHEMA_VERSION {
        return Err(fatal(format!(
            "chain constants schema version {schema_version} is unsupported"
        )));
    }
    let resolved = GenesisProtocolParametersV1 {
        metadosis_forming_period_seconds: slot(
            storage.clone(),
            SLOT_METADOSIS_FORMING_PERIOD_SECONDS,
        )
        .read()?,
        metadosis_lookback_delay_seconds: slot(
            storage.clone(),
            SLOT_METADOSIS_LOOKBACK_DELAY_SECONDS,
        )
        .read()?,
        metadosis_offering_period_seconds: slot(
            storage.clone(),
            SLOT_METADOSIS_OFFERING_PERIOD_SECONDS,
        )
        .read()?,
        metadosis_waiting_period_seconds: slot(
            storage.clone(),
            SLOT_METADOSIS_WAITING_PERIOD_SECONDS,
        )
        .read()?,
        metadosis_bootstrap_duration_seconds: slot(
            storage.clone(),
            SLOT_METADOSIS_BOOTSTRAP_DURATION_SECONDS,
        )
        .read()?,
        metadosis_advance_interval_seconds: slot(
            storage.clone(),
            SLOT_METADOSIS_ADVANCE_INTERVAL_SECONDS,
        )
        .read()?,
        ocomp_compute_vote_window_blocks: slot(
            storage.clone(),
            SLOT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS,
        )
        .read()?,
    };
    resolved
        .validate()
        .map_err(|error| fatal(format!("invalid chain constants in storage: {error}")))?;
    let stored_hash: B256 = slot::<B256>(storage, SLOT_PARAMETERS_HASH).read()?;
    if stored_hash != resolved.parameters_hash() {
        return Err(fatal("chain constants parameters hash mismatch"));
    }
    Ok(resolved)
}

fn slot<T>(storage: StorageHandle<'_>, ordinal: u64) -> Slot<'_, T> {
    Slot::new(U256::from(ordinal), CHAIN_CONSTANTS_ADDRESS, storage)
}

fn fatal(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(message.into())
}

fn validate_nonzero_at_most(
    field: &'static str,
    value: u64,
    maximum: u64,
) -> Result<(), ProtocolConstantsError> {
    if value == 0 || value > maximum {
        return Err(ProtocolConstantsError::InvalidValue {
            field,
            requirement: "non-zero and no greater than the production default",
            actual: value,
        });
    }
    Ok(())
}

fn validate_at_most(
    field: &'static str,
    value: u64,
    maximum: u64,
) -> Result<(), ProtocolConstantsError> {
    if value > maximum {
        return Err(ProtocolConstantsError::InvalidValue {
            field,
            requirement: "no greater than the production default",
            actual: value,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::{
        block::{BlockContext, BlockRuntimeContext},
        storage::{hashmap::HashMapStorageProvider, StorageHandle},
    };
    use serde_json::json;

    #[test]
    fn absent_fields_resolve_once_to_canonical_defaults() {
        assert_eq!(
            GenesisProtocolParametersV1::resolve(None).unwrap(),
            GenesisProtocolParametersV1::default()
        );
        assert_eq!(
            GenesisProtocolParametersV1::resolve(Some(&json!({}))).unwrap(),
            GenesisProtocolParametersV1::default()
        );
    }

    #[test]
    fn explicit_fields_override_independently_without_exposing_origin() {
        let resolved = GenesisProtocolParametersV1::resolve(Some(&json!({
            "schemaVersion": 1,
            "metadosis": {
                "formingPeriodSeconds": 60,
                "lookbackDelaySeconds": 0,
                "offeringPeriodSeconds": 120,
                "waitingPeriodSeconds": 30,
                "bootstrapDurationSeconds": 300,
                "advanceIntervalSeconds": 10
            },
            "ocomp": { "computeVoteWindowBlocks": 120 }
        })))
        .unwrap();

        assert_eq!(resolved.metadosis_forming_period_seconds, 60);
        assert_eq!(resolved.metadosis_lookback_delay_seconds, 0);
        assert_eq!(resolved.metadosis_offering_period_seconds, 120);
        assert_eq!(resolved.metadosis_waiting_period_seconds, 30);
        assert_eq!(resolved.metadosis_bootstrap_duration_seconds, 300);
        assert_eq!(resolved.metadosis_advance_interval_seconds, 10);
        assert_eq!(resolved.ocomp_compute_vote_window_blocks, 120);
    }

    #[test]
    fn malformed_unknown_and_unsafe_values_are_rejected() {
        assert!(GenesisProtocolParametersV1::resolve(Some(&json!({
            "unknown": 1
        })))
        .is_err());
        assert!(GenesisProtocolParametersV1::resolve(Some(&json!({
            "metadosis": { "formingPeriodSeconds": 0 }
        })))
        .is_err());
        assert!(GenesisProtocolParametersV1::resolve(Some(&json!({
            "ocomp": { "computeVoteWindowBlocks": 1801 }
        })))
        .is_err());
    }

    #[test]
    fn parameters_hash_is_stable_and_covers_every_field() {
        let defaults = GenesisProtocolParametersV1::default();
        assert_eq!(defaults.parameters_hash(), defaults.parameters_hash());

        let mut changed = defaults;
        changed.metadosis_lookback_delay_seconds -= 1;
        assert_ne!(defaults.parameters_hash(), changed.parameters_hash());
        changed = defaults;
        changed.ocomp_compute_vote_window_blocks -= 1;
        assert_ne!(defaults.parameters_hash(), changed.parameters_hash());
    }

    #[test]
    fn genesis_words_roundtrip_through_the_runtime_reader() {
        let expected = GenesisProtocolParametersV1::resolve(Some(&json!({
            "metadosis": { "formingPeriodSeconds": 60 },
            "ocomp": { "computeVoteWindowBlocks": 120 }
        })))
        .unwrap();
        let mut provider = HashMapStorageProvider::new(1);
        provider
            .enter(|storage| {
                install_words(storage.clone(), expected)?;
                assert_eq!(load_uncached(storage)?, expected);
                Ok::<_, PrecompileError>(())
            })
            .unwrap();
    }

    #[test]
    fn materialized_genesis_reader_uses_the_same_storage_schema() {
        let expected = GenesisProtocolParametersV1::resolve(Some(&json!({
            "metadosis": { "formingPeriodSeconds": 60 },
            "ocomp": { "computeVoteWindowBlocks": 120 }
        })))
        .unwrap();
        let storage = expected
            .genesis_storage_words()
            .into_iter()
            .map(|(slot, value)| {
                (
                    format!("{slot:#066x}"),
                    serde_json::json!(format!("{value:#066x}")),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let address = format!("{CHAIN_CONSTANTS_ADDRESS:#x}");
        let genesis = json!({
            "alloc": {
                address: { "code": "0xef", "storage": storage }
            }
        });
        assert_eq!(
            GenesisProtocolParametersV1::from_materialized_genesis(&genesis).unwrap(),
            expected
        );
    }

    #[test]
    fn cache_is_bound_to_genesis_and_avoids_reloading_mutated_state() {
        let first = GenesisProtocolParametersV1::default();
        let mut second = first;
        second.metadosis_forming_period_seconds = 60;
        let first_hash = B256::repeat_byte(0x41);
        let second_hash = B256::repeat_byte(0x42);

        let mut provider = HashMapStorageProvider::new(1);
        provider
            .enter(|storage| {
                install_words(storage.clone(), first)?;
                let first_ctx = BlockRuntimeContext::new(
                    BlockContext::new_with_genesis_hash(
                        1,
                        1,
                        1,
                        first_hash,
                        Address::ZERO,
                        Vec::new(),
                    ),
                    storage.clone(),
                );
                assert_eq!(load(&first_ctx)?.as_ref(), &first);

                install_words(storage.clone(), second)?;
                assert_eq!(load(&first_ctx)?.as_ref(), &first);
                let second_ctx = BlockRuntimeContext::new(
                    BlockContext::new_with_genesis_hash(
                        1,
                        1,
                        1,
                        second_hash,
                        Address::ZERO,
                        Vec::new(),
                    ),
                    storage,
                );
                assert_eq!(load(&second_ctx)?.as_ref(), &second);
                Ok::<_, PrecompileError>(())
            })
            .unwrap();
    }

    #[test]
    fn missing_or_corrupt_storage_never_falls_back_to_defaults() {
        let mut provider = HashMapStorageProvider::new(1);
        provider
            .enter(|storage| {
                assert!(matches!(
                    load_uncached(storage.clone()),
                    Err(PrecompileError::Fatal(_))
                ));
                install_words(storage.clone(), GenesisProtocolParametersV1::default())?;
                slot::<B256>(storage.clone(), SLOT_PARAMETERS_HASH)
                    .write(B256::repeat_byte(0x99))?;
                assert!(matches!(
                    load_uncached(storage),
                    Err(PrecompileError::Fatal(_))
                ));
                Ok::<_, PrecompileError>(())
            })
            .unwrap();
    }

    #[test]
    fn identity_free_unit_context_uses_defaults_without_populating_global_cache() {
        let mut provider = HashMapStorageProvider::new(1);
        provider
            .enter(|storage| {
                let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(1, 1, 1), storage);
                assert_eq!(
                    load(&ctx)?.as_ref(),
                    &GenesisProtocolParametersV1::default()
                );
                Ok::<_, PrecompileError>(())
            })
            .unwrap();
    }

    fn install_words(
        storage: StorageHandle<'_>,
        constants: GenesisProtocolParametersV1,
    ) -> RuntimeResult<()> {
        for (ordinal, value) in constants.genesis_storage_words() {
            Slot::<U256>::new(ordinal, CHAIN_CONSTANTS_ADDRESS, storage.clone()).write(value)?;
        }
        Ok(())
    }
}
