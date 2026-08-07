//! Genesis-fixed authority for V1 TEE attestation.
//!
//! Every runnable chain must carry this field. Testnet may explicitly select
//! `DcapRequired` or `GramineDirectDev`; devnet selects `GramineDirectDev`.
//! Both use OST3, and neither can fall back at runtime.

use std::sync::Arc;

use alloy_primitives::B256;
use outbe_primitives::{
    chain::TESTNET_CHAIN_ID,
    tee_attestation_v1::{
        AttestationMode, EnclaveProfile, PlatformTcbStatusSetV1, QvlTcbStatusV1,
        ResourceScheduleV1, TeeAttestationManifestV1, TeePolicyScheduleV1, TeePolicyV1,
        MAX_TEE_POLICY_SCHEDULE_BYTES,
    },
    tee_genesis_v1::{
        initial_tee_policy_v1, is_gramine_direct_dev_chain_id, InitialTeeProfileV1,
        ProductionSgxMeasurementV1, GRAMINE_DIRECT_DEV_CHAIN_ID,
    },
    OutbeHeader,
};
use reth_ethereum::chainspec::ChainSpec;
use serde::Deserialize;

pub const TEE_ATTESTATION_V1_CONFIG_FIELD: &str = "teeAttestationV1";
pub const TEE_ATTESTATION_V1_ACTIVATION_HEIGHT: u64 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeeAttestationActivationV1 {
    pub manifest: TeeAttestationManifestV1,
    pub policy_schedule: TeePolicyScheduleV1,
}

impl TeeAttestationActivationV1 {
    pub fn policy_at(&self, height: u64) -> Result<&TeePolicyV1, String> {
        self.policy_schedule
            .active_policy(height)
            .map_err(|error| format!("invalid active TEE policy at block {height}: {error}"))
    }
}

/// Exact canonical block-1 DCAP authority derived from the testnet ChainSpec.
///
/// Release tooling uses this value instead of reconstructing policy from image
/// measurements. Construction reuses the same fail-closed parser as block
/// execution, so callers cannot supply chain, genesis or policy fields
/// independently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DcapTestnetChainSpecBindingV1 {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub activation_height: u64,
    pub policy_version: u64,
    pub policy: TeePolicyV1,
    pub policy_bytes: Vec<u8>,
    pub policy_hash: B256,
    pub policy_schedule: TeePolicyScheduleV1,
    pub policy_schedule_bytes: Vec<u8>,
    pub policy_schedule_hash: B256,
}

impl DcapTestnetChainSpecBindingV1 {
    pub fn from_genesis_path(path: &std::path::Path) -> Result<Self, String> {
        let path = path
            .to_str()
            .ok_or_else(|| "testnet genesis path is not UTF-8".to_owned())?;
        let spec = reth_ethereum::cli::chainspec::chain_value_parser(path)
            .map_err(|error| format!("parse testnet genesis ChainSpec: {error}"))?
            .as_ref()
            .clone()
            .map_header(OutbeHeader::new);
        Self::from_chain_spec(&spec)
    }

    pub fn from_chain_spec(spec: &ChainSpec<OutbeHeader>) -> Result<Self, String> {
        let state = TeeAttestationChainSpecStateV1::from_chain_spec(spec);
        let activation = state.activation().map_err(str::to_owned)?;
        let policy = activation
            .policy_at(TEE_ATTESTATION_V1_ACTIVATION_HEIGHT)?
            .clone();
        if policy.attestation_mode != AttestationMode::DcapRequired {
            return Err("testnet release ChainSpec requires DcapRequired policy".into());
        }
        let policy_bytes = policy
            .encode_canonical()
            .map_err(|error| format!("encode testnet TEE policy: {error}"))?;
        let policy_hash = policy
            .policy_hash()
            .map_err(|error| format!("hash testnet TEE policy: {error}"))?;
        let policy_schedule = activation.policy_schedule.clone();
        let policy_schedule_bytes = policy_schedule
            .encode_canonical()
            .map_err(|error| format!("encode testnet TEE policy schedule: {error}"))?;
        Ok(Self {
            chain_id: spec.chain().id(),
            genesis_hash: spec.genesis_hash(),
            activation_height: activation.manifest.activation_height,
            policy_version: policy.policy_version,
            policy,
            policy_bytes,
            policy_hash,
            policy_schedule,
            policy_schedule_bytes,
            policy_schedule_hash: activation.manifest.policy_schedule_hash,
        })
    }

    pub fn ensure_exact_release_measurements(
        &self,
        mrenclave: B256,
        mrsigner: B256,
        isv_prod_id: u16,
        isv_svn: u16,
    ) -> Result<(), String> {
        let policy = &self.policy;
        if policy.accepted_platform_tcb_statuses
            != PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded
            || policy.accepted_qe_tcb_status != QvlTcbStatusV1::UpToDate
        {
            return Err(
                "active testnet policy must admit Platform UpToDate | SWHardeningNeeded | ConfigurationAndSWHardeningNeeded and QE UpToDate"
                    .into(),
            );
        }
        let matches_rule = |profile| {
            policy.measurement_rules.iter().any(|rule| {
                rule.enclave_profile == profile
                    && rule.mrenclave == mrenclave
                    && rule.mrsigner == mrsigner
                    && rule.isv_prod_id == isv_prod_id
                    && rule.minimum_isv_svn == isv_svn
                    && rule.admit_from_height == policy.activation_height
                    && rule.admit_until_height_exclusive == u64::MAX
            })
        };
        if policy.measurement_rules.len() != 2
            || !matches_rule(EnclaveProfile::Validator)
            || !matches_rule(EnclaveProfile::FullNode)
        {
            return Err(
                "active testnet policy does not exactly bind signed bundle measurements for Validator and FullNode"
                    .into(),
            );
        }
        let expected = initial_tee_policy_v1(
            InitialTeeProfileV1::DcapRequired(ProductionSgxMeasurementV1 {
                mrenclave,
                mrsigner,
                isv_prod_id,
                minimum_isv_svn: isv_svn,
                minimum_tcb_evaluation_data_number: policy.minimum_tcb_evaluation_data_number,
            }),
            self.chain_id,
            self.genesis_hash,
        )
        .map_err(|error| format!("derive canonical initial testnet policy: {error}"))?;
        if *policy != expected {
            return Err(
                "active testnet policy does not equal the canonical initial policy for the signed release"
                    .into(),
            );
        }
        Ok(())
    }
}

/// Parsing never falls open. Constructors that cannot return a Result retain an
/// Invalid state and make block construction/execution fail before mutation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TeeAttestationChainSpecStateV1 {
    #[default]
    Unbound,
    Active(Arc<TeeAttestationActivationV1>),
    Invalid(Arc<str>),
}

impl TeeAttestationChainSpecStateV1 {
    pub fn from_chain_spec(spec: &ChainSpec<OutbeHeader>) -> Self {
        let parsed = spec
            .genesis
            .config
            .extra_fields
            .get_deserialized::<GenesisTeeAttestationV1>(TEE_ATTESTATION_V1_CONFIG_FIELD);
        match parsed {
            None => Self::Invalid(
                format!("genesis config is missing required {TEE_ATTESTATION_V1_CONFIG_FIELD}")
                    .into(),
            ),
            Some(Err(error)) => Self::Invalid(
                format!("invalid genesis config {TEE_ATTESTATION_V1_CONFIG_FIELD}: {error}").into(),
            ),
            Some(Ok(raw)) => {
                match validate_activation(raw, spec.chain().id(), spec.genesis_hash()) {
                    Ok(activation) => Self::Active(Arc::new(activation)),
                    Err(error) => Self::Invalid(error.into()),
                }
            }
        }
    }

    pub fn activation(&self) -> Result<&TeeAttestationActivationV1, &str> {
        match self {
            Self::Unbound => Err("TEE attestation ChainSpec authority is not bound"),
            Self::Active(activation) => Ok(activation),
            Self::Invalid(error) => Err(error),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenesisTeeAttestationV1 {
    activation_height: u64,
    policy_schedule: String,
    policy_schedule_hash: B256,
    resource_schedule_hash: B256,
}

fn validate_activation(
    raw: GenesisTeeAttestationV1,
    chain_id: u64,
    genesis_hash: B256,
) -> Result<TeeAttestationActivationV1, String> {
    if raw.activation_height != TEE_ATTESTATION_V1_ACTIVATION_HEIGHT {
        return Err(format!(
            "{TEE_ATTESTATION_V1_CONFIG_FIELD}.activationHeight must be {}",
            TEE_ATTESTATION_V1_ACTIVATION_HEIGHT
        ));
    }
    let encoded_schedule = decode_bounded_hex(&raw.policy_schedule)?;
    let policy_schedule = TeePolicyScheduleV1::decode_canonical(&encoded_schedule)
        .map_err(|error| format!("invalid canonical TEE policy schedule: {error}"))?;
    if policy_schedule.chain_id != chain_id_word(chain_id)
        || policy_schedule.genesis_hash != genesis_hash
    {
        return Err("TEE policy schedule does not match ChainSpec identity".into());
    }
    if policy_schedule.entries.len() != 1 {
        return Err("genesis TEE policy schedule must contain exactly one initial policy".into());
    }
    let computed_policy_schedule_hash = policy_schedule
        .schedule_hash()
        .map_err(|error| format!("cannot hash TEE policy schedule: {error}"))?;
    if raw.policy_schedule_hash != computed_policy_schedule_hash {
        return Err("TEE policy schedule hash does not match canonical bytes".into());
    }
    let normative_resource_hash = ResourceScheduleV1::normative()
        .and_then(|schedule| schedule.schedule_hash())
        .map_err(|error| format!("cannot derive normative resource schedule: {error}"))?;
    if raw.resource_schedule_hash != normative_resource_hash {
        return Err("TEE manifest does not bind the normative resource schedule".into());
    }
    let initial_policy = policy_schedule
        .active_policy(TEE_ATTESTATION_V1_ACTIVATION_HEIGHT)
        .map_err(|error| format!("TEE schedule has no block-1 policy: {error}"))?;
    match initial_policy.attestation_mode {
        AttestationMode::DcapRequired if chain_id == GRAMINE_DIRECT_DEV_CHAIN_ID => {
            return Err(format!(
                "DcapRequired may not use reserved GramineDirectDev chain ID {GRAMINE_DIRECT_DEV_CHAIN_ID}"
            ));
        }
        AttestationMode::DcapRequired if chain_id != TESTNET_CHAIN_ID => {
            return Err(format!(
                "DcapRequired requires testnet chain ID {TESTNET_CHAIN_ID}"
            ));
        }
        AttestationMode::GramineDirectDev if !is_gramine_direct_dev_chain_id(chain_id) => {
            return Err(format!(
                "GramineDirectDev requires devnet or testnet chain ID ({GRAMINE_DIRECT_DEV_CHAIN_ID} or {TESTNET_CHAIN_ID})"
            ));
        }
        AttestationMode::DcapRequired | AttestationMode::GramineDirectDev => {}
    }
    if initial_policy.resource_schedule_hash != raw.resource_schedule_hash {
        return Err("initial TEE policy does not bind the manifest resource schedule".into());
    }
    Ok(TeeAttestationActivationV1 {
        manifest: TeeAttestationManifestV1 {
            activation_height: raw.activation_height,
            policy_schedule_hash: raw.policy_schedule_hash,
            resource_schedule_hash: raw.resource_schedule_hash,
        },
        policy_schedule,
    })
}

fn decode_bounded_hex(value: &str) -> Result<Vec<u8>, String> {
    let encoded = value
        .strip_prefix("0x")
        .ok_or_else(|| "TEE policy schedule must be 0x-prefixed hexadecimal".to_owned())?;
    let maximum_hex_len = MAX_TEE_POLICY_SCHEDULE_BYTES
        .checked_mul(2)
        .ok_or_else(|| "TEE policy schedule cap overflow".to_owned())?;
    if encoded.len() > maximum_hex_len {
        return Err(format!(
            "TEE policy schedule exceeds {MAX_TEE_POLICY_SCHEDULE_BYTES} bytes"
        ));
    }
    if encoded.len() % 2 != 0 || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("TEE policy schedule is not canonical hexadecimal bytes".into());
    }
    hex::decode(encoded).map_err(|error| format!("decode TEE policy schedule: {error}"))
}

fn chain_id_word(chain_id: u64) -> [u8; 32] {
    let mut word = [0_u8; 32];
    word[24..].copy_from_slice(&chain_id.to_be_bytes());
    word
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::{
        tee_attestation_v1::{
            AttestationMode, EnclaveProfile, PlatformTcbStatusSetV1, QvlTcbStatusV1,
            TeeMeasurementRuleV1, TeePolicyScheduleEntryV1,
        },
        tee_genesis_v1::{
            initial_tee_policy_v1, tee_attestation_v1_genesis_field, InitialTeeProfileV1,
            ProductionSgxMeasurementV1,
        },
    };

    fn testnet_chain_spec() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        ChainSpec<OutbeHeader>,
    ) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("testnet-genesis.json");
        let mut genesis = serde_json::json!({
            "config": {
                "chainId": TESTNET_CHAIN_ID,
                "homesteadBlock": 0,
                "eip150Block": 0,
                "eip155Block": 0,
                "eip158Block": 0,
                "byzantiumBlock": 0,
                "constantinopleBlock": 0,
                "petersburgBlock": 0,
                "istanbulBlock": 0,
                "berlinBlock": 0,
                "londonBlock": 0,
                "terminalTotalDifficultyPassed": true
            },
            "nonce": "0x0",
            "timestamp": "0x0",
            "extraData": "0x",
            "gasLimit": "0x1c9c380",
            "difficulty": "0x0",
            "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "coinbase": "0x0000000000000000000000000000000000000000",
            "alloc": {}
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&genesis).unwrap()).unwrap();
        let base =
            reth_ethereum::cli::chainspec::chain_value_parser(path.to_str().unwrap()).unwrap();
        let policy = initial_tee_policy_v1(
            InitialTeeProfileV1::DcapRequired(ProductionSgxMeasurementV1 {
                mrenclave: B256::repeat_byte(0x22),
                mrsigner: B256::repeat_byte(0x33),
                isv_prod_id: 1,
                minimum_isv_svn: 2,
                minimum_tcb_evaluation_data_number: 17,
            }),
            TESTNET_CHAIN_ID,
            base.genesis_hash(),
        )
        .unwrap();
        genesis["config"][TEE_ATTESTATION_V1_CONFIG_FIELD] =
            tee_attestation_v1_genesis_field(&policy).unwrap();
        std::fs::write(&path, serde_json::to_vec_pretty(&genesis).unwrap()).unwrap();
        let spec = reth_ethereum::cli::chainspec::chain_value_parser(path.to_str().unwrap())
            .unwrap()
            .as_ref()
            .clone()
            .map_header(OutbeHeader::new);
        (root, path, spec)
    }

    fn activation(
        chain_id: u64,
        genesis_hash: B256,
        mode: AttestationMode,
    ) -> GenesisTeeAttestationV1 {
        let resource_schedule_hash = ResourceScheduleV1::normative()
            .unwrap()
            .schedule_hash()
            .unwrap();
        let policy = TeePolicyV1 {
            policy_version: 1,
            chain_id: chain_id_word(chain_id),
            genesis_hash,
            activation_height: 1,
            predecessor_policy_hash: B256::ZERO,
            attestation_mode: mode,
            intel_root_der_hash: B256::repeat_byte(0x11),
            quote_version: 3,
            tee_type: 0,
            attestation_key_type: 2,
            qe_vendor_id: [
                0x93, 0x9a, 0x72, 0x33, 0xf7, 0x9c, 0x4c, 0xa9, 0x94, 0x0a, 0x0d, 0xb3, 0x95, 0x7f,
                0x06, 0x07,
            ],
            certification_data_type: 5,
            tcb_info_schema_version: 3,
            qe_identity_schema_version: 2,
            minimum_tcb_evaluation_data_number: 1,
            accepted_platform_tcb_statuses: PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
            accepted_qe_tcb_status: QvlTcbStatusV1::UpToDate,
            minimum_lease: 3_600,
            maximum_lease: 604_800,
            collateral_margin: 3_600,
            resource_schedule_hash,
            measurement_rules: vec![TeeMeasurementRuleV1 {
                enclave_profile: EnclaveProfile::Validator,
                mrenclave: B256::repeat_byte(0x22),
                mrsigner: B256::repeat_byte(0x33),
                isv_prod_id: 1,
                minimum_isv_svn: 1,
                admit_from_height: 1,
                admit_until_height_exclusive: u64::MAX,
            }],
        };
        let policy_schedule = TeePolicyScheduleV1 {
            chain_id: chain_id_word(chain_id),
            genesis_hash,
            entries: vec![TeePolicyScheduleEntryV1 {
                activation_height: 1,
                policy,
            }],
        };
        let encoded = policy_schedule.encode_canonical().unwrap();
        GenesisTeeAttestationV1 {
            activation_height: 1,
            policy_schedule: format!("0x{}", hex::encode(encoded)),
            policy_schedule_hash: policy_schedule.schedule_hash().unwrap(),
            resource_schedule_hash,
        }
    }

    #[test]
    fn valid_activation_binds_chain_policy_and_normative_resources() {
        let chain_id = TESTNET_CHAIN_ID;
        let genesis_hash = B256::repeat_byte(0x44);
        let parsed = validate_activation(
            activation(chain_id, genesis_hash, AttestationMode::DcapRequired),
            chain_id,
            genesis_hash,
        )
        .unwrap();
        assert_eq!(parsed.manifest.activation_height, 1);
        assert_eq!(
            parsed.policy_at(1).unwrap().attestation_mode,
            AttestationMode::DcapRequired
        );
    }

    #[test]
    fn dcap_testnet_binding_exposes_exact_chainspec_policy_bytes() {
        let (_, _, spec) = testnet_chain_spec();
        let binding = DcapTestnetChainSpecBindingV1::from_chain_spec(&spec).unwrap();
        assert_eq!(binding.chain_id, TESTNET_CHAIN_ID);
        assert_eq!(binding.genesis_hash, spec.genesis_hash());
        assert_eq!(
            binding.activation_height,
            TEE_ATTESTATION_V1_ACTIVATION_HEIGHT
        );
        assert_eq!(binding.policy_version, 1);
        assert_eq!(
            binding.policy_bytes,
            binding.policy.encode_canonical().unwrap()
        );
        assert_eq!(binding.policy_hash, binding.policy.policy_hash().unwrap());
        assert_eq!(
            binding.policy_schedule_hash,
            binding.policy_schedule.schedule_hash().unwrap()
        );
        assert_eq!(
            binding.policy_schedule_bytes,
            binding.policy_schedule.encode_canonical().unwrap()
        );
    }

    #[test]
    fn dcap_testnet_binding_loads_the_exact_genesis_file() {
        let (_root, path, expected) = testnet_chain_spec();
        let binding = DcapTestnetChainSpecBindingV1::from_genesis_path(&path).unwrap();
        assert_eq!(binding.chain_id, TESTNET_CHAIN_ID);
        assert_eq!(binding.genesis_hash, expected.genesis_hash());
    }

    #[test]
    fn dcap_testnet_binding_requires_exact_release_measurements_for_both_roles() {
        let (_, _, spec) = testnet_chain_spec();
        let binding = DcapTestnetChainSpecBindingV1::from_chain_spec(&spec).unwrap();
        binding
            .ensure_exact_release_measurements(
                B256::repeat_byte(0x22),
                B256::repeat_byte(0x33),
                1,
                2,
            )
            .unwrap();
        assert!(binding
            .ensure_exact_release_measurements(
                B256::repeat_byte(0x44),
                B256::repeat_byte(0x33),
                1,
                2,
            )
            .unwrap_err()
            .contains("signed bundle measurements"));

        let mut non_intel_root = binding.clone();
        non_intel_root.policy.intel_root_der_hash = B256::repeat_byte(0x99);
        assert!(non_intel_root
            .ensure_exact_release_measurements(
                B256::repeat_byte(0x22),
                B256::repeat_byte(0x33),
                1,
                2,
            )
            .unwrap_err()
            .contains("canonical initial policy"));

        let mut tightened = binding;
        tightened.policy.accepted_platform_tcb_statuses = PlatformTcbStatusSetV1::UpToDateOnly;
        assert!(tightened
            .ensure_exact_release_measurements(
                B256::repeat_byte(0x22),
                B256::repeat_byte(0x33),
                1,
                2,
            )
            .unwrap_err()
            .contains("UpToDate | SWHardeningNeeded | ConfigurationAndSWHardeningNeeded"));
    }

    #[test]
    fn mismatched_identity_or_hash_fails_closed() {
        let chain_id = TESTNET_CHAIN_ID;
        let genesis_hash = B256::repeat_byte(0x44);
        assert!(validate_activation(
            activation(chain_id, genesis_hash, AttestationMode::DcapRequired),
            chain_id + 1,
            genesis_hash
        )
        .unwrap_err()
        .contains("identity"));

        let mut wrong_hash = activation(chain_id, genesis_hash, AttestationMode::DcapRequired);
        wrong_hash.policy_schedule_hash = B256::repeat_byte(0x55);
        assert!(validate_activation(wrong_hash, chain_id, genesis_hash)
            .unwrap_err()
            .contains("schedule hash"));
    }

    #[test]
    fn dev_mode_is_valid_for_devnet_and_testnet_but_not_unknown_identities() {
        let dev_chain_id = GRAMINE_DIRECT_DEV_CHAIN_ID;
        let dev_genesis_hash = B256::repeat_byte(0x66);
        let dev = validate_activation(
            activation(
                dev_chain_id,
                dev_genesis_hash,
                AttestationMode::GramineDirectDev,
            ),
            dev_chain_id,
            dev_genesis_hash,
        )
        .unwrap();
        assert_eq!(
            dev.policy_at(1).unwrap().attestation_mode,
            AttestationMode::GramineDirectDev
        );
        let testnet = validate_activation(
            activation(
                TESTNET_CHAIN_ID,
                dev_genesis_hash,
                AttestationMode::GramineDirectDev,
            ),
            TESTNET_CHAIN_ID,
            dev_genesis_hash,
        )
        .unwrap();
        assert_eq!(
            testnet.policy_at(1).unwrap().attestation_mode,
            AttestationMode::GramineDirectDev
        );

        let unknown_chain_id = TESTNET_CHAIN_ID + 1;
        assert!(validate_activation(
            activation(
                unknown_chain_id,
                dev_genesis_hash,
                AttestationMode::GramineDirectDev,
            ),
            unknown_chain_id,
            dev_genesis_hash,
        )
        .unwrap_err()
        .contains("devnet or testnet chain ID"));

        assert!(validate_activation(
            activation(
                dev_chain_id,
                dev_genesis_hash,
                AttestationMode::DcapRequired
            ),
            dev_chain_id,
            dev_genesis_hash,
        )
        .unwrap_err()
        .contains("may not use reserved GramineDirectDev chain ID"));

        assert!(validate_activation(
            activation(
                unknown_chain_id,
                dev_genesis_hash,
                AttestationMode::DcapRequired,
            ),
            unknown_chain_id,
            dev_genesis_hash,
        )
        .unwrap_err()
        .contains("testnet chain ID"));
    }

    #[test]
    fn malformed_or_oversized_schedule_is_rejected_before_decode() {
        assert!(decode_bounded_hex("abcd")
            .unwrap_err()
            .contains("0x-prefixed"));
        assert!(decode_bounded_hex("0x0").unwrap_err().contains("canonical"));
        let oversized = format!("0x{}", "00".repeat(MAX_TEE_POLICY_SCHEDULE_BYTES + 1));
        assert!(decode_bounded_hex(&oversized)
            .unwrap_err()
            .contains("exceeds"));
    }
}
