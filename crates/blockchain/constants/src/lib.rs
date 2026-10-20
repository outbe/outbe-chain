//! Immutable protocol parameters shared by every runtime module.

pub mod errors;

pub use errors::ProtocolConstantsError;
use outbe_primitives::time::SECONDS_PER_DAY;
use serde::{Deserialize, Serialize};
#[cfg(feature = "test-protocol-overrides")]
use std::sync::OnceLock;

pub const GENESIS_CONFIG_KEY: &str = "outbeProtocol";
pub const PROTOCOL_CONSTANTS_SCHEMA_VERSION: u16 = 1;

pub const DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS: u64 = 50 * 60 * 60;
pub const DEFAULT_METADOSIS_LOOKBACK_DELAY_SECONDS: u64 = 502 * 60 * 60;
pub const DEFAULT_METADOSIS_OFFERING_PERIOD_SECONDS: u64 = 50 * 60 * 60;
pub const DEFAULT_METADOSIS_WAITING_PERIOD_SECONDS: u64 = 12 * 60 * 60;
pub const DEFAULT_METADOSIS_ADVANCE_INTERVAL_SECONDS: u64 = 60 * 60;
pub const DEFAULT_METADOSIS_LOOKAHEAD_DAYS: u32 = 7;
pub const DEFAULT_GOVERNANCE_VOTING_WINDOW_BLOCKS: u64 = 86_400;
pub const DEFAULT_VWAP_LOOKBACK_SECONDS: u64 = 8 * 60 * 60;
pub const DEFAULT_VWAP_UPDATE_INTERVAL_SECONDS: u64 = 60 * 60;
pub const DEFAULT_VWAP_POLICY_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenesisProtocolParametersV1 {
    pub governance_voting_window_blocks: u64,
    pub metadosis_forming_period_seconds: u64,
    pub metadosis_lookback_delay_seconds: u64,
    pub metadosis_offering_period_seconds: u64,
    pub metadosis_waiting_period_seconds: u64,
    pub metadosis_advance_interval_seconds: u64,
    pub vwap_lookback_seconds: u64,
    pub vwap_update_interval_seconds: u64,
    pub vwap_policy_version: u32,
}

const DEFAULTS: GenesisProtocolParametersV1 = GenesisProtocolParametersV1 {
    governance_voting_window_blocks: DEFAULT_GOVERNANCE_VOTING_WINDOW_BLOCKS,
    metadosis_forming_period_seconds: DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS,
    metadosis_lookback_delay_seconds: DEFAULT_METADOSIS_LOOKBACK_DELAY_SECONDS,
    metadosis_offering_period_seconds: DEFAULT_METADOSIS_OFFERING_PERIOD_SECONDS,
    metadosis_waiting_period_seconds: DEFAULT_METADOSIS_WAITING_PERIOD_SECONDS,
    metadosis_advance_interval_seconds: DEFAULT_METADOSIS_ADVANCE_INTERVAL_SECONDS,
    vwap_lookback_seconds: DEFAULT_VWAP_LOOKBACK_SECONDS,
    vwap_update_interval_seconds: DEFAULT_VWAP_UPDATE_INTERVAL_SECONDS,
    vwap_policy_version: DEFAULT_VWAP_POLICY_VERSION,
};

impl Default for GenesisProtocolParametersV1 {
    fn default() -> Self {
        DEFAULTS
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenesisProtocolOverridesV1 {
    #[serde(default)]
    governance: GovernanceOverridesV1,
    #[serde(default)]
    schema_version: Option<u16>,
    #[serde(default)]
    metadosis: MetadosisOverridesV1,
    #[serde(default)]
    oracle: OracleOverridesV1,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OracleOverridesV1 {
    vwap_lookback_seconds: Option<u64>,
    vwap_update_interval_seconds: Option<u64>,
    vwap_policy_version: Option<u32>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MetadosisOverridesV1 {
    forming_period_seconds: Option<u64>,
    lookback_delay_seconds: Option<u64>,
    offering_period_seconds: Option<u64>,
    waiting_period_seconds: Option<u64>,
    advance_interval_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GovernanceOverridesV1 {
    voting_window_blocks: Option<u64>,
}

impl GenesisProtocolParametersV1 {
    pub fn resolve(value: Option<&serde_json::Value>) -> Result<Self, ProtocolConstantsError> {
        let overrides = match value {
            Some(value) => serde_json::from_value::<GenesisProtocolOverridesV1>(value.clone())
                .map_err(ProtocolConstantsError::from)?,
            None => GenesisProtocolOverridesV1::default(),
        };
        if let Some(version) = overrides.schema_version {
            if version != PROTOCOL_CONSTANTS_SCHEMA_VERSION {
                return Err(ProtocolConstantsError::UnsupportedSchemaVersion(version));
            }
        }
        let defaults = Self::default();
        let resolved = Self {
            governance_voting_window_blocks: overrides
                .governance
                .voting_window_blocks
                .unwrap_or(defaults.governance_voting_window_blocks),
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
            metadosis_advance_interval_seconds: overrides
                .metadosis
                .advance_interval_seconds
                .unwrap_or(defaults.metadosis_advance_interval_seconds),
            vwap_lookback_seconds: overrides
                .oracle
                .vwap_lookback_seconds
                .unwrap_or(defaults.vwap_lookback_seconds),
            vwap_update_interval_seconds: overrides
                .oracle
                .vwap_update_interval_seconds
                .unwrap_or(defaults.vwap_update_interval_seconds),
            vwap_policy_version: overrides
                .oracle
                .vwap_policy_version
                .unwrap_or(defaults.vwap_policy_version),
        };
        resolved.validate()?;
        Ok(resolved)
    }

    pub fn validate(self) -> Result<(), ProtocolConstantsError> {
        validate_nonzero_at_most(
            "governance.votingWindowBlocks",
            self.governance_voting_window_blocks,
            DEFAULT_GOVERNANCE_VOTING_WINDOW_BLOCKS,
        )?;
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
            "metadosis.advanceIntervalSeconds",
            self.metadosis_advance_interval_seconds,
            DEFAULT_METADOSIS_ADVANCE_INTERVAL_SECONDS,
        )?;
        validate_nonzero_at_most(
            "oracle.vwapLookbackSeconds",
            self.vwap_lookback_seconds,
            DEFAULT_VWAP_LOOKBACK_SECONDS,
        )?;
        validate_nonzero_at_most(
            "oracle.vwapUpdateIntervalSeconds",
            self.vwap_update_interval_seconds,
            DEFAULT_VWAP_UPDATE_INTERVAL_SECONDS,
        )?;
        if !SECONDS_PER_DAY.is_multiple_of(self.vwap_update_interval_seconds) {
            return Err(ProtocolConstantsError::InvalidValue {
                field: "oracle.vwapUpdateIntervalSeconds",
                requirement: "divide a UTC day",
                actual: self.vwap_update_interval_seconds,
            });
        }
        if !self
            .vwap_lookback_seconds
            .is_multiple_of(self.vwap_update_interval_seconds)
        {
            return Err(ProtocolConstantsError::InvalidValue {
                field: "oracle.vwapLookbackSeconds",
                requirement: "be a whole number of update intervals",
                actual: self.vwap_lookback_seconds,
            });
        }
        if self.vwap_policy_version == 0 {
            return Err(ProtocolConstantsError::InvalidValue {
                field: "oracle.vwapPolicyVersion",
                requirement: "be non-zero",
                actual: 0,
            });
        }

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

    /// Resolve protocol parameters directly from `config.outbeProtocol` in a
    /// genesis JSON document. The `alloc` section is deliberately irrelevant.
    pub fn from_genesis(genesis: &serde_json::Value) -> Result<Self, ProtocolConstantsError> {
        let config = genesis
            .get("config")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                ProtocolConstantsError::InvalidJson(
                    "genesis config must be a JSON object".to_owned(),
                )
            })?;
        Self::resolve(config.get(GENESIS_CONFIG_KEY))
    }

    /// Apply the build policy to a parsed `config.outbeProtocol` value.
    pub fn select_for_build(
        value: Option<&serde_json::Value>,
    ) -> Result<Self, ProtocolConstantsError> {
        #[cfg(feature = "test-protocol-overrides")]
        {
            Self::resolve(value)
        }

        #[cfg(not(feature = "test-protocol-overrides"))]
        {
            if value.is_some() {
                return Err(ProtocolConstantsError::UnsupportedInProduction);
            }
            Ok(DEFAULTS)
        }
    }
}

#[cfg(feature = "test-protocol-overrides")]
static PARAMETERS: OnceLock<GenesisProtocolParametersV1> = OnceLock::new();

/// Enforce the build policy and initialize test-only overrides when enabled.
/// Re-validating the same effective parameters is harmless. Changing the
/// parameters of an already running process remains forbidden.
pub fn initialize(value: Option<&serde_json::Value>) -> Result<(), ProtocolConstantsError> {
    let parameters = GenesisProtocolParametersV1::select_for_build(value)?;

    #[cfg(feature = "test-protocol-overrides")]
    {
        initialize_once(&PARAMETERS, parameters)
    }

    #[cfg(not(feature = "test-protocol-overrides"))]
    {
        let _ = parameters;
        Ok(())
    }
}

#[cfg(feature = "test-protocol-overrides")]
fn initialize_once(
    slot: &OnceLock<GenesisProtocolParametersV1>,
    parameters: GenesisProtocolParametersV1,
) -> Result<(), ProtocolConstantsError> {
    if *slot.get_or_init(|| parameters) == parameters {
        Ok(())
    } else {
        Err(ProtocolConstantsError::AlreadyInitialized)
    }
}

#[cfg(not(feature = "test-protocol-overrides"))]
fn parameters() -> &'static GenesisProtocolParametersV1 {
    &DEFAULTS
}

#[cfg(feature = "test-protocol-overrides")]
fn parameters() -> &'static GenesisProtocolParametersV1 {
    PARAMETERS
        .get()
        .expect("protocol constants must be initialized before runtime access")
}

pub fn get_metadosis_forming_period_seconds() -> u64 {
    parameters().metadosis_forming_period_seconds
}

pub fn get_metadosis_lookback_delay_seconds() -> u64 {
    parameters().metadosis_lookback_delay_seconds
}

pub fn get_metadosis_offering_period_seconds() -> u64 {
    parameters().metadosis_offering_period_seconds
}

pub fn get_metadosis_waiting_period_seconds() -> u64 {
    parameters().metadosis_waiting_period_seconds
}

pub fn get_metadosis_advance_interval_seconds() -> u64 {
    parameters().metadosis_advance_interval_seconds
}

pub const fn get_metadosis_lookahead_days() -> u32 {
    DEFAULT_METADOSIS_LOOKAHEAD_DAYS
}

pub fn get_governance_voting_window_blocks() -> u64 {
    parameters().governance_voting_window_blocks
}

pub fn get_vwap_lookback_seconds() -> u64 {
    parameters().vwap_lookback_seconds
}

pub fn get_vwap_update_interval_seconds() -> u64 {
    parameters().vwap_update_interval_seconds
}

pub fn get_vwap_policy_version() -> u32 {
    parameters().vwap_policy_version
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
    use serde_json::json;

    #[cfg(feature = "test-protocol-overrides")]
    #[test]
    fn repeated_initialization_accepts_only_identical_effective_parameters() {
        let slot = OnceLock::new();
        initialize_once(&slot, DEFAULTS).unwrap();
        initialize_once(
            &slot,
            GenesisProtocolParametersV1::resolve(Some(&json!({}))).unwrap(),
        )
        .unwrap();
        let changed = GenesisProtocolParametersV1::resolve(Some(&json!({
            "governance": { "votingWindowBlocks": 20 }
        })))
        .unwrap();
        assert_eq!(
            initialize_once(&slot, changed),
            Err(ProtocolConstantsError::AlreadyInitialized)
        );
        assert_eq!(slot.get(), Some(&DEFAULTS));
    }

    #[cfg(feature = "test-protocol-overrides")]
    #[test]
    fn concurrent_conflicting_initialization_keeps_exactly_one_configuration() {
        let slot = OnceLock::new();
        let other = GenesisProtocolParametersV1 {
            governance_voting_window_blocks: 20,
            ..DEFAULTS
        };
        std::thread::scope(|scope| {
            let a = scope.spawn(|| initialize_once(&slot, DEFAULTS));
            let b = scope.spawn(|| initialize_once(&slot, other));
            assert_ne!(a.join().unwrap().is_ok(), b.join().unwrap().is_ok());
        });
        initialize_once(&slot, *slot.get().unwrap()).unwrap();
    }

    #[test]
    fn governance_window_is_independent_and_strictly_bounded() {
        for blocks in [1, 6, 20, 86_400] {
            let resolved = GenesisProtocolParametersV1::resolve(Some(&json!({
                "governance": { "votingWindowBlocks": blocks },
            })))
            .unwrap();
            assert_eq!(resolved.governance_voting_window_blocks, blocks);
        }
        for blocks in [0, 86_401, u64::MAX] {
            assert_eq!(
                GenesisProtocolParametersV1::resolve(Some(&json!({
                    "governance": { "votingWindowBlocks": blocks }
                }))),
                Err(ProtocolConstantsError::InvalidValue {
                    field: "governance.votingWindowBlocks",
                    requirement: "non-zero and no greater than the production default",
                    actual: blocks,
                })
            );
        }
        for invalid in [
            json!({ "governance": { "votingWindowBlocks": -1 } }),
            json!({ "governance": { "votingWindowBlocks": "6" } }),
            json!({ "governance": { "votingWindowBlock": 6 } }),
        ] {
            assert!(matches!(
                GenesisProtocolParametersV1::resolve(Some(&invalid)),
                Err(ProtocolConstantsError::InvalidJson(_))
            ));
        }
        assert_eq!(
            GenesisProtocolParametersV1::resolve(Some(&json!({
                "governance": {}
            })))
            .unwrap()
            .governance_voting_window_blocks,
            86_400
        );
    }

    #[test]
    fn absent_fields_resolve_once_to_canonical_defaults() {
        let defaults = GenesisProtocolParametersV1::default();
        assert_eq!(
            GenesisProtocolParametersV1::resolve(None).unwrap(),
            defaults
        );
        assert_eq!(
            GenesisProtocolParametersV1::resolve(Some(&json!({}))).unwrap(),
            defaults
        );
        initialize(None).unwrap();
        assert_eq!(
            (
                get_governance_voting_window_blocks(),
                get_metadosis_forming_period_seconds(),
                get_metadosis_lookback_delay_seconds(),
                get_metadosis_offering_period_seconds(),
                get_metadosis_waiting_period_seconds(),
                get_metadosis_advance_interval_seconds(),
                get_vwap_lookback_seconds(),
                get_vwap_update_interval_seconds(),
                get_vwap_policy_version(),
            ),
            (
                defaults.governance_voting_window_blocks,
                defaults.metadosis_forming_period_seconds,
                defaults.metadosis_lookback_delay_seconds,
                defaults.metadosis_offering_period_seconds,
                defaults.metadosis_waiting_period_seconds,
                defaults.metadosis_advance_interval_seconds,
                defaults.vwap_lookback_seconds,
                defaults.vwap_update_interval_seconds,
                defaults.vwap_policy_version,
            )
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
                "advanceIntervalSeconds": 10
            },
        })))
        .unwrap();

        assert_eq!(resolved.metadosis_forming_period_seconds, 60);
        assert_eq!(resolved.metadosis_lookback_delay_seconds, 0);
        assert_eq!(resolved.metadosis_offering_period_seconds, 120);
        assert_eq!(resolved.metadosis_waiting_period_seconds, 30);
        assert_eq!(resolved.metadosis_advance_interval_seconds, 10);
    }

    #[test]
    fn malformed_unknown_and_unsafe_values_are_rejected() {
        assert!(matches!(
            GenesisProtocolParametersV1::resolve(Some(&json!({
                "unknown": 1
            }))),
            Err(ProtocolConstantsError::InvalidJson(_))
        ));
        for version in [0, 2] {
            assert_eq!(
                GenesisProtocolParametersV1::resolve(Some(&json!({
                    "schemaVersion": version
                }))),
                Err(ProtocolConstantsError::UnsupportedSchemaVersion(version))
            );
        }
        assert_eq!(
            GenesisProtocolParametersV1::resolve(Some(&json!({
                "metadosis": { "formingPeriodSeconds": 0 }
            }))),
            Err(ProtocolConstantsError::InvalidValue {
                field: "metadosis.formingPeriodSeconds",
                requirement: "non-zero and no greater than the production default",
                actual: 0,
            })
        );
    }

    #[test]
    fn genesis_reader_resolves_config_without_alloc_state() {
        let expected = GenesisProtocolParametersV1::resolve(Some(&json!({
            "metadosis": { "formingPeriodSeconds": 60 },
        })))
        .unwrap();
        let genesis = json!({
            "config": {
                "outbeProtocol": {
                    "metadosis": { "formingPeriodSeconds": 60 },
                }
            }
        });
        assert_eq!(
            GenesisProtocolParametersV1::from_genesis(&genesis).unwrap(),
            expected
        );
        for invalid in [json!({}), json!({ "config": 1 })] {
            assert_eq!(
                GenesisProtocolParametersV1::from_genesis(&invalid),
                Err(ProtocolConstantsError::InvalidJson(
                    "genesis config must be a JSON object".to_owned(),
                ))
            );
        }
    }
}
