//! Canonical block-1 TEE policy and ChainSpec-manifest construction.
//!
//! The development profile is selected explicitly in genesis, never as a
//! fallback for an unavailable DCAP verifier. It is admitted on the devnet and
//! testnet identities so the same fresh testnet topology can run without SGX.
//! `DcapRequired` construction still requires the exact signed-enclave
//! measurement tuple. This API cannot emit a placeholder DCAP policy.

use alloy_primitives::{keccak256, B256, U256};
use serde_json::{json, Value};

use crate::time::SECONDS_PER_DAY;

use crate::{
    chain::{DEVNET_CHAIN_ID, MAINNET_CHAIN_ID, TESTNET_CHAIN_ID},
    errors::TeeGenesisPolicyError,
    tee_attestation_v1::{
        sev_snp_measurement_rule_value, tdx_image_rule_value, tdx_mrconfigid_rule_value,
        AttestationMode, PlatformTcbStatusSetV1, QvlTcbStatusV1, ResourceScheduleV1,
        TeeMeasurementRuleV1, TeePolicyScheduleEntryV1, TeePolicyScheduleV1, TeePolicyV1,
        INTEL_QE_VENDOR_ID, SEV_SNP_RULE_PRODUCT_ID_V1, SEV_SNP_RULE_SVN_V1, TDX_QUOTE_VERSION_V1,
        TDX_RULE_PRODUCT_ID_V1, TDX_RULE_SVN_V1, TDX_TEE_TYPE_V1,
    },
};

/// Primary chain ID for the runnable Gramine Direct devnet.
pub const GRAMINE_DIRECT_DEV_CHAIN_ID: u64 = DEVNET_CHAIN_ID;

/// Returns whether an explicitly configured development policy may use this
/// fresh-network identity. Testnet keeps requiring an explicit genesis
/// `GramineDirectDev` policy. This is not a runtime fallback from DCAP.
pub const fn is_gramine_direct_dev_chain_id(chain_id: u64) -> bool {
    chain_id == DEVNET_CHAIN_ID || chain_id == TESTNET_CHAIN_ID
}

/// Returns whether the chain has a production DCAP identity.
pub const fn is_dcap_required_chain_id(chain_id: u64) -> bool {
    chain_id == DEVNET_CHAIN_ID || chain_id == TESTNET_CHAIN_ID || chain_id == MAINNET_CHAIN_ID
}

/// Returns whether the chain may require AMD SEV-SNP attestation.
pub const fn is_sev_snp_required_chain_id(chain_id: u64) -> bool {
    chain_id == DEVNET_CHAIN_ID || chain_id == TESTNET_CHAIN_ID
}

/// Returns whether the chain may require Intel TDX attestation.
pub const fn is_tdx_required_chain_id(chain_id: u64) -> bool {
    chain_id == DEVNET_CHAIN_ID || chain_id == TESTNET_CHAIN_ID
}

/// Returns whether a product network may select this explicit attestation
/// mode at genesis. The selected mode is subsequently fixed for the life of
/// the chain by TeeRegistry successor-policy validation.
pub const fn is_attestation_mode_allowed_for_chain_id(
    chain_id: u64,
    mode: AttestationMode,
) -> bool {
    match mode {
        AttestationMode::DcapRequired => is_dcap_required_chain_id(chain_id),
        AttestationMode::GramineDirectDev => is_gramine_direct_dev_chain_id(chain_id),
        AttestationMode::SevSnpRequired => is_sev_snp_required_chain_id(chain_id),
        AttestationMode::TdxRequired => is_tdx_required_chain_id(chain_id),
        AttestationMode::ConfidentialVmRequired => {
            is_sev_snp_required_chain_id(chain_id) && is_tdx_required_chain_id(chain_id)
        }
    }
}

/// SHA-256 of Intel's pinned SGX Root CA DER certificate.
pub const INTEL_SGX_ROOT_CA_DER_SHA256: [u8; 32] = [
    0x44, 0xa0, 0x19, 0x6b, 0x2b, 0x99, 0xf8, 0x89, 0xb8, 0xe1, 0x49, 0xe9, 0x5b, 0x80, 0x7a, 0x35,
    0x0e, 0x74, 0x24, 0x96, 0x43, 0x99, 0xe8, 0x85, 0xa7, 0xcb, 0xb8, 0xcc, 0xfa, 0xb6, 0x74, 0xd3,
];

/// Canonical production enclave lease period: fourteen consensus days.
pub const PRODUCTION_TEE_LEASE_SECONDS_V1: u64 = 14 * SECONDS_PER_DAY;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProductionSgxMeasurementV1 {
    pub mrenclave: B256,
    pub mrsigner: B256,
    pub isv_prod_id: u16,
    pub minimum_isv_svn: u16,
    pub minimum_tcb_evaluation_data_number: u32,
}

/// Identity of the confidential VM that an SEV-SNP network admits at genesis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SevSnpLaunchIdentityV1 {
    /// Launch measurement of the VM image for one VM shape.
    pub measurement: [u8; 48],
    /// Host data that the VMM binds at launch. It commits to the application
    /// manifest of the VM.
    pub host_data: B256,
    /// SHA-256 of the DER certificate of the AMD root key (ARK). One processor
    /// generation has one root.
    pub ark_der_sha256: B256,
}

/// Identity of the trust domain that a TDX network admits at genesis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TdxLaunchIdentityV1 {
    /// Build-time measurement of the TD.
    pub mrtd: [u8; 48],
    /// Runtime measurement registers 0, 1 and 2. `RTMR0` depends on the VM
    /// shape, so one VM shape has one identity.
    pub rtmrs: [[u8; 48]; 3],
    /// Configuration identity that the VMM binds at launch. It commits to the
    /// application manifest of the VM.
    pub mr_config_id: [u8; 48],
    /// Lowest accepted TCB evaluation data number of the Intel collateral.
    pub minimum_tcb_evaluation_data_number: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitialTeeProfileV1 {
    DcapRequired(ProductionSgxMeasurementV1),
    GramineDirectDev,
    SevSnpRequired(SevSnpLaunchIdentityV1),
    TdxRequired(TdxLaunchIdentityV1),
    /// A network with validators on both platforms.
    ConfidentialVmRequired {
        sev_snp: SevSnpLaunchIdentityV1,
        tdx: TdxLaunchIdentityV1,
    },
}

impl InitialTeeProfileV1 {
    pub const fn attestation_mode(self) -> AttestationMode {
        match self {
            Self::DcapRequired(_) => AttestationMode::DcapRequired,
            Self::GramineDirectDev => AttestationMode::GramineDirectDev,
            Self::SevSnpRequired(_) => AttestationMode::SevSnpRequired,
            Self::TdxRequired(_) => AttestationMode::TdxRequired,
            Self::ConfidentialVmRequired { .. } => AttestationMode::ConfidentialVmRequired,
        }
    }
}

/// Measurement rule of an SEV-SNP launch identity. The rule keeps the SGX field
/// names: `mrenclave` holds the projection of the launch measurement,
/// `mrsigner` the host data.
fn sev_snp_rule(
    identity: &SevSnpLaunchIdentityV1,
) -> Result<TeeMeasurementRuleV1, TeeGenesisPolicyError> {
    if identity.measurement == [0; 48]
        || identity.host_data.is_zero()
        || identity.ark_der_sha256.is_zero()
    {
        return Err(TeeGenesisPolicyError::ZeroSevSnpIdentity);
    }
    Ok(TeeMeasurementRuleV1 {
        mrenclave: sev_snp_measurement_rule_value(&identity.measurement),
        mrsigner: identity.host_data,
        isv_prod_id: SEV_SNP_RULE_PRODUCT_ID_V1,
        minimum_isv_svn: SEV_SNP_RULE_SVN_V1,
        admit_from_height: 1,
        admit_until_height_exclusive: u64::MAX,
    })
}

/// Measurement rule of a TDX launch identity: `mrenclave` holds the projection
/// of the image measurements, `mrsigner` the projection of `MRCONFIGID`.
fn tdx_rule(identity: &TdxLaunchIdentityV1) -> Result<TeeMeasurementRuleV1, TeeGenesisPolicyError> {
    if identity.mrtd == [0; 48] || identity.mr_config_id == [0; 48] {
        return Err(TeeGenesisPolicyError::ZeroTdxIdentity);
    }
    Ok(TeeMeasurementRuleV1 {
        mrenclave: tdx_image_rule_value(&identity.mrtd, &identity.rtmrs),
        mrsigner: tdx_mrconfigid_rule_value(&identity.mr_config_id),
        isv_prod_id: TDX_RULE_PRODUCT_ID_V1,
        minimum_isv_svn: TDX_RULE_SVN_V1,
        admit_from_height: 1,
        admit_until_height_exclusive: u64::MAX,
    })
}

fn tdx_minimum_tcb_evaluation_data_number(
    identity: &TdxLaunchIdentityV1,
) -> Result<u32, TeeGenesisPolicyError> {
    if identity.minimum_tcb_evaluation_data_number == 0 {
        return Err(TeeGenesisPolicyError::ZeroMinimumTcbEvaluationDataNumber);
    }
    Ok(identity.minimum_tcb_evaluation_data_number)
}

/// Build the only policy shape permitted at block 1.
///
/// NodeHost admission is role-neutral because every role executes the same
/// pinned enclave binary.
pub fn initial_tee_policy_v1(
    profile: InitialTeeProfileV1,
    chain_id: u64,
    genesis_hash: B256,
) -> Result<TeePolicyV1, TeeGenesisPolicyError> {
    if genesis_hash.is_zero() {
        return Err(TeeGenesisPolicyError::ZeroGenesisHash);
    }
    match profile.attestation_mode() {
        AttestationMode::DcapRequired
            if !is_attestation_mode_allowed_for_chain_id(
                chain_id,
                AttestationMode::DcapRequired,
            ) =>
        {
            return Err(TeeGenesisPolicyError::InvalidDcapChainId {
                devnet: DEVNET_CHAIN_ID,
                testnet: TESTNET_CHAIN_ID,
                mainnet: MAINNET_CHAIN_ID,
            });
        }
        AttestationMode::GramineDirectDev
            if !is_attestation_mode_allowed_for_chain_id(
                chain_id,
                AttestationMode::GramineDirectDev,
            ) =>
        {
            return Err(TeeGenesisPolicyError::InvalidGramineDirectDevChainId {
                devnet: DEVNET_CHAIN_ID,
                testnet: TESTNET_CHAIN_ID,
            });
        }
        AttestationMode::SevSnpRequired
            if !is_attestation_mode_allowed_for_chain_id(
                chain_id,
                AttestationMode::SevSnpRequired,
            ) =>
        {
            return Err(TeeGenesisPolicyError::InvalidSevSnpChainId {
                devnet: DEVNET_CHAIN_ID,
                testnet: TESTNET_CHAIN_ID,
            });
        }
        AttestationMode::TdxRequired
            if !is_attestation_mode_allowed_for_chain_id(
                chain_id,
                AttestationMode::TdxRequired,
            ) =>
        {
            return Err(TeeGenesisPolicyError::InvalidTdxChainId {
                devnet: DEVNET_CHAIN_ID,
                testnet: TESTNET_CHAIN_ID,
            });
        }
        AttestationMode::ConfidentialVmRequired
            if !is_attestation_mode_allowed_for_chain_id(
                chain_id,
                AttestationMode::ConfidentialVmRequired,
            ) =>
        {
            return Err(TeeGenesisPolicyError::InvalidTdxChainId {
                devnet: DEVNET_CHAIN_ID,
                testnet: TESTNET_CHAIN_ID,
            });
        }
        _ => {}
    }

    let resource_schedule_hash = ResourceScheduleV1::normative()
        .and_then(|schedule| schedule.schedule_hash())
        .map_err(TeeGenesisPolicyError::derive_normative_resource_schedule)?;
    let (minimum_tcb_evaluation_data_number, measurement_rules) = match profile {
        InitialTeeProfileV1::DcapRequired(measurement) => {
            if measurement.mrenclave.is_zero() || measurement.mrsigner.is_zero() {
                return Err(TeeGenesisPolicyError::ZeroDcapMeasurement);
            }
            if measurement.minimum_tcb_evaluation_data_number == 0 {
                return Err(TeeGenesisPolicyError::ZeroMinimumTcbEvaluationDataNumber);
            }
            let rule = TeeMeasurementRuleV1 {
                mrenclave: measurement.mrenclave,
                mrsigner: measurement.mrsigner,
                isv_prod_id: measurement.isv_prod_id,
                minimum_isv_svn: measurement.minimum_isv_svn,
                admit_from_height: 1,
                admit_until_height_exclusive: u64::MAX,
            };
            (measurement.minimum_tcb_evaluation_data_number, vec![rule])
        }
        InitialTeeProfileV1::GramineDirectDev => {
            // These are domain-separated non-hardware projections used only by
            // the explicitly separate development policy. They are not SGX
            // measurements and cannot be selected by DcapRequired construction.
            let rule = TeeMeasurementRuleV1 {
                mrenclave: keccak256(b"outbe/tee/gramine-direct-dev/mrenclave/v1"),
                mrsigner: keccak256(b"outbe/tee/gramine-direct-dev/mrsigner/v1"),
                isv_prod_id: 1,
                minimum_isv_svn: 1,
                admit_from_height: 1,
                admit_until_height_exclusive: u64::MAX,
            };
            (1, vec![rule])
        }
        InitialTeeProfileV1::SevSnpRequired(identity) => (1, vec![sev_snp_rule(&identity)?]),
        InitialTeeProfileV1::TdxRequired(identity) => (
            tdx_minimum_tcb_evaluation_data_number(&identity)?,
            vec![tdx_rule(&identity)?],
        ),
        InitialTeeProfileV1::ConfidentialVmRequired { sev_snp, tdx } => {
            // One rule for each platform. The policy keeps its rules sorted.
            let mut rules = vec![sev_snp_rule(&sev_snp)?, tdx_rule(&tdx)?];
            rules.sort_by_key(|rule| (rule.mrenclave, rule.mrsigner));
            (tdx_minimum_tcb_evaluation_data_number(&tdx)?, rules)
        }
    };
    // The policy has one field for the hash of the root certificate. An SEV-SNP
    // policy stores the AMD root there. The Intel collateral fields below keep
    // their DCAP constants and are not read in the SEV-SNP mode.
    let root_der_hash = match profile {
        InitialTeeProfileV1::SevSnpRequired(identity) => identity.ark_der_sha256,
        // A TDX quote chains to the same Intel SGX root as an SGX quote.
        InitialTeeProfileV1::DcapRequired(_)
        | InitialTeeProfileV1::GramineDirectDev
        | InitialTeeProfileV1::TdxRequired(_)
        | InitialTeeProfileV1::ConfidentialVmRequired { .. } => {
            B256::from(INTEL_SGX_ROOT_CA_DER_SHA256)
        }
    };
    // The mixed mode has a second root: the AMD root of its SEV-SNP rule.
    let amd_root_der_hash = match profile {
        InitialTeeProfileV1::ConfidentialVmRequired { sev_snp, .. } => sev_snp.ark_der_sha256,
        _ => B256::ZERO,
    };
    let (quote_version, tee_type) = match profile {
        InitialTeeProfileV1::TdxRequired(_)
        | InitialTeeProfileV1::ConfidentialVmRequired { .. } => {
            (TDX_QUOTE_VERSION_V1, TDX_TEE_TYPE_V1)
        }
        InitialTeeProfileV1::DcapRequired(_)
        | InitialTeeProfileV1::GramineDirectDev
        | InitialTeeProfileV1::SevSnpRequired(_) => (3, 0),
    };

    let policy = TeePolicyV1 {
        policy_version: 1,
        chain_id: U256::from(chain_id).to_be_bytes(),
        genesis_hash,
        activation_height: 1,
        predecessor_policy_hash: B256::ZERO,
        attestation_mode: profile.attestation_mode(),
        intel_root_der_hash: root_der_hash,
        amd_root_der_hash,
        quote_version,
        tee_type,
        attestation_key_type: 2,
        qe_vendor_id: INTEL_QE_VENDOR_ID,
        certification_data_type: 5,
        tcb_info_schema_version: 3,
        qe_identity_schema_version: 2,
        minimum_tcb_evaluation_data_number,
        accepted_platform_tcb_statuses: PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
        accepted_qe_tcb_status: QvlTcbStatusV1::UpToDate,
        minimum_lease: 3_600,
        maximum_lease: PRODUCTION_TEE_LEASE_SECONDS_V1,
        collateral_margin: 3_600,
        resource_schedule_hash,
        measurement_rules,
    };
    policy
        .encode_canonical()
        .map_err(TeeGenesisPolicyError::encode_initial_policy)?;
    Ok(policy)
}

/// Encode the canonical one-entry policy schedule into the genesis config
/// field consumed by every testnet node.
pub fn tee_attestation_v1_genesis_field(
    policy: &TeePolicyV1,
) -> Result<Value, TeeGenesisPolicyError> {
    let schedule = TeePolicyScheduleV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        entries: vec![TeePolicyScheduleEntryV1 {
            activation_height: policy.activation_height,
            policy: policy.clone(),
        }],
    };
    let encoded = schedule
        .encode_canonical()
        .map_err(TeeGenesisPolicyError::encode_initial_policy_schedule)?;
    let policy_schedule_hash = schedule
        .schedule_hash()
        .map_err(TeeGenesisPolicyError::hash_initial_policy_schedule)?;
    Ok(json!({
        "activationHeight": 1,
        "policySchedule": format!("0x{}", hex::encode(encoded)),
        "policyScheduleHash": policy_schedule_hash,
        "resourceScheduleHash": policy.resource_schedule_hash,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::{DEVNET_CHAIN_ID, MAINNET_CHAIN_ID, TESTNET_CHAIN_ID};

    fn dcap_measurement() -> ProductionSgxMeasurementV1 {
        ProductionSgxMeasurementV1 {
            mrenclave: B256::repeat_byte(0x11),
            mrsigner: B256::repeat_byte(0x22),
            isv_prod_id: 7,
            minimum_isv_svn: 3,
            minimum_tcb_evaluation_data_number: 17,
        }
    }

    fn tdx_identity() -> TdxLaunchIdentityV1 {
        TdxLaunchIdentityV1 {
            mrtd: [0x41; 48],
            rtmrs: [[0x42; 48], [0x43; 48], [0x44; 48]],
            mr_config_id: [0x45; 48],
            minimum_tcb_evaluation_data_number: 17,
        }
    }

    #[test]
    fn tdx_policy_projects_the_td_identity_and_takes_the_tdx_quote_profile() {
        let policy = initial_tee_policy_v1(
            InitialTeeProfileV1::TdxRequired(tdx_identity()),
            TESTNET_CHAIN_ID,
            B256::repeat_byte(0x77),
        )
        .unwrap();

        assert_eq!(policy.attestation_mode, AttestationMode::TdxRequired);
        assert_eq!(policy.quote_version, 4);
        assert_eq!(policy.tee_type, 0x81);
        assert_eq!(policy.certification_data_type, 5);
        // A TDX quote chains to the Intel SGX root.
        assert_eq!(
            policy.intel_root_der_hash,
            B256::from(INTEL_SGX_ROOT_CA_DER_SHA256)
        );
        assert_eq!(policy.minimum_tcb_evaluation_data_number, 17);
        let rule = &policy.measurement_rules[0];
        assert_eq!(
            rule.mrenclave,
            tdx_image_rule_value(&[0x41; 48], &[[0x42; 48], [0x43; 48], [0x44; 48]])
        );
        assert_eq!(rule.mrsigner, tdx_mrconfigid_rule_value(&[0x45; 48]));
        assert_eq!(rule.isv_prod_id, TDX_RULE_PRODUCT_ID_V1);
        assert_eq!(rule.minimum_isv_svn, TDX_RULE_SVN_V1);
        // The policy round-trips: the TDX profile is canonical in this mode.
        let encoded = policy.encode_canonical().unwrap();
        assert_eq!(TeePolicyV1::decode_canonical(&encoded).unwrap(), policy);
    }

    #[test]
    fn quote_profile_belongs_to_its_attestation_mode() {
        let tdx = initial_tee_policy_v1(
            InitialTeeProfileV1::TdxRequired(tdx_identity()),
            TESTNET_CHAIN_ID,
            B256::repeat_byte(0x77),
        )
        .unwrap();
        // The SGX profile in a TDX policy is not canonical.
        let mut sgx_profile = tdx.clone();
        sgx_profile.quote_version = 3;
        sgx_profile.tee_type = 0;
        assert!(sgx_profile.encode_canonical().is_err());
        // The TDX profile in a DCAP policy is not canonical.
        let mut dcap_mode = tdx;
        dcap_mode.attestation_mode = AttestationMode::DcapRequired;
        assert!(dcap_mode.encode_canonical().is_err());
    }

    #[test]
    fn each_rtmr_and_the_mrconfigid_change_the_rule() {
        let base = tdx_identity();
        let rule = |identity: TdxLaunchIdentityV1| {
            initial_tee_policy_v1(
                InitialTeeProfileV1::TdxRequired(identity),
                TESTNET_CHAIN_ID,
                B256::repeat_byte(0x77),
            )
            .unwrap()
            .measurement_rules[0]
                .clone()
        };
        let base_rule = rule(base);
        for index in 0..3 {
            let mut other = base;
            other.rtmrs[index][0] ^= 1;
            assert_ne!(rule(other).mrenclave, base_rule.mrenclave);
            assert_eq!(rule(other).mrsigner, base_rule.mrsigner);
        }
        let mut other_mrtd = base;
        other_mrtd.mrtd[0] ^= 1;
        assert_ne!(rule(other_mrtd).mrenclave, base_rule.mrenclave);
        let mut other_manifest = base;
        other_manifest.mr_config_id[0] ^= 1;
        assert_ne!(rule(other_manifest).mrsigner, base_rule.mrsigner);
        assert_eq!(rule(other_manifest).mrenclave, base_rule.mrenclave);
    }

    #[test]
    fn mixed_policy_has_two_roots_and_a_rule_for_each_platform() {
        let profile = InitialTeeProfileV1::ConfidentialVmRequired {
            sev_snp: sev_snp_identity(),
            tdx: tdx_identity(),
        };
        let policy =
            initial_tee_policy_v1(profile, TESTNET_CHAIN_ID, B256::repeat_byte(0x77)).unwrap();

        assert_eq!(
            policy.attestation_mode,
            AttestationMode::ConfidentialVmRequired
        );
        assert_eq!(
            policy.intel_root_der_hash,
            B256::from(INTEL_SGX_ROOT_CA_DER_SHA256)
        );
        assert_eq!(policy.amd_root_der_hash, sev_snp_identity().ark_der_sha256);
        assert_eq!(policy.amd_root(), sev_snp_identity().ark_der_sha256);
        assert_eq!((policy.quote_version, policy.tee_type), (4, 0x81));
        assert_eq!(policy.measurement_rules.len(), 2);
        // The policy round-trips with its second root.
        let encoded = policy.encode_canonical().unwrap();
        assert_eq!(TeePolicyV1::decode_canonical(&encoded).unwrap(), policy);

        // The second root belongs to the mixed mode only.
        let mut no_amd_root = policy.clone();
        no_amd_root.amd_root_der_hash = B256::ZERO;
        assert!(no_amd_root.encode_canonical().is_err());
        let mut tdx_with_amd_root = initial_tee_policy_v1(
            InitialTeeProfileV1::TdxRequired(tdx_identity()),
            TESTNET_CHAIN_ID,
            B256::repeat_byte(0x77),
        )
        .unwrap();
        let tdx_len = tdx_with_amd_root.encode_canonical().unwrap().len();
        // One more rule and one more root than the TDX policy.
        assert!(encoded.len() > tdx_len + 32);
        tdx_with_amd_root.amd_root_der_hash = B256::repeat_byte(0x01);
        assert!(tdx_with_amd_root.encode_canonical().is_err());

        // In the SEV-SNP mode the single root field holds the AMD root.
        let snp = initial_tee_policy_v1(
            InitialTeeProfileV1::SevSnpRequired(sev_snp_identity()),
            TESTNET_CHAIN_ID,
            B256::repeat_byte(0x77),
        )
        .unwrap();
        assert_eq!(snp.amd_root(), sev_snp_identity().ark_der_sha256);
        assert!(snp.amd_root_der_hash.is_zero());

        assert!(initial_tee_policy_v1(profile, MAINNET_CHAIN_ID, B256::repeat_byte(0x77)).is_err());
    }

    #[test]
    fn tdx_policy_is_allowed_on_devnet_and_testnet_only() {
        let profile = InitialTeeProfileV1::TdxRequired(tdx_identity());
        for chain_id in [DEVNET_CHAIN_ID, TESTNET_CHAIN_ID] {
            assert!(initial_tee_policy_v1(profile, chain_id, B256::repeat_byte(0x77)).is_ok());
        }
        assert!(matches!(
            initial_tee_policy_v1(profile, MAINNET_CHAIN_ID, B256::repeat_byte(0x77)),
            Err(TeeGenesisPolicyError::InvalidTdxChainId { .. })
        ));
    }

    #[test]
    fn tdx_policy_rejects_a_zero_identity_value() {
        let zero_mrtd = TdxLaunchIdentityV1 {
            mrtd: [0; 48],
            ..tdx_identity()
        };
        let zero_manifest = TdxLaunchIdentityV1 {
            mr_config_id: [0; 48],
            ..tdx_identity()
        };
        for identity in [zero_mrtd, zero_manifest] {
            assert!(matches!(
                initial_tee_policy_v1(
                    InitialTeeProfileV1::TdxRequired(identity),
                    TESTNET_CHAIN_ID,
                    B256::repeat_byte(0x77),
                ),
                Err(TeeGenesisPolicyError::ZeroTdxIdentity)
            ));
        }
        let zero_number = TdxLaunchIdentityV1 {
            minimum_tcb_evaluation_data_number: 0,
            ..tdx_identity()
        };
        assert!(matches!(
            initial_tee_policy_v1(
                InitialTeeProfileV1::TdxRequired(zero_number),
                TESTNET_CHAIN_ID,
                B256::repeat_byte(0x77),
            ),
            Err(TeeGenesisPolicyError::ZeroMinimumTcbEvaluationDataNumber)
        ));
    }

    fn sev_snp_identity() -> SevSnpLaunchIdentityV1 {
        SevSnpLaunchIdentityV1 {
            measurement: [0x33; 48],
            host_data: B256::repeat_byte(0x5a),
            ark_der_sha256: B256::repeat_byte(0x69),
        }
    }

    #[test]
    fn sev_snp_policy_projects_the_launch_identity_into_one_rule() {
        let policy = initial_tee_policy_v1(
            InitialTeeProfileV1::SevSnpRequired(sev_snp_identity()),
            TESTNET_CHAIN_ID,
            B256::repeat_byte(0x33),
        )
        .unwrap();

        assert_eq!(policy.attestation_mode, AttestationMode::SevSnpRequired);
        assert_eq!(policy.intel_root_der_hash, B256::repeat_byte(0x69));
        let [rule] = policy.measurement_rules.as_slice() else {
            panic!("the policy must hold exactly one rule");
        };
        let mut preimage = b"outbe/tee/sev-snp/measurement/v1".to_vec();
        preimage.extend_from_slice(&[0x33; 48]);
        assert_eq!(rule.mrenclave, keccak256(preimage));
        assert_eq!(rule.mrsigner, B256::repeat_byte(0x5a));
        assert_eq!((rule.isv_prod_id, rule.minimum_isv_svn), (1, 1));
        assert_eq!(
            (rule.admit_from_height, rule.admit_until_height_exclusive),
            (1, u64::MAX)
        );
    }

    #[test]
    fn sev_snp_policy_is_allowed_on_devnet_and_testnet_only() {
        let profile = InitialTeeProfileV1::SevSnpRequired(sev_snp_identity());
        let genesis_hash = B256::repeat_byte(0x33);

        assert!(initial_tee_policy_v1(profile, DEVNET_CHAIN_ID, genesis_hash).is_ok());
        assert!(initial_tee_policy_v1(profile, TESTNET_CHAIN_ID, genesis_hash).is_ok());
        assert!(matches!(
            initial_tee_policy_v1(profile, MAINNET_CHAIN_ID, genesis_hash),
            Err(TeeGenesisPolicyError::InvalidSevSnpChainId { .. })
        ));
    }

    #[test]
    fn sev_snp_policy_rejects_a_zero_identity_value() {
        let zero_measurement = SevSnpLaunchIdentityV1 {
            measurement: [0; 48],
            ..sev_snp_identity()
        };
        let zero_host_data = SevSnpLaunchIdentityV1 {
            host_data: B256::ZERO,
            ..sev_snp_identity()
        };
        let zero_root = SevSnpLaunchIdentityV1 {
            ark_der_sha256: B256::ZERO,
            ..sev_snp_identity()
        };

        for identity in [zero_measurement, zero_host_data, zero_root] {
            assert!(matches!(
                initial_tee_policy_v1(
                    InitialTeeProfileV1::SevSnpRequired(identity),
                    TESTNET_CHAIN_ID,
                    B256::repeat_byte(0x33),
                ),
                Err(TeeGenesisPolicyError::ZeroSevSnpIdentity)
            ));
        }
    }

    #[test]
    fn devnet_and_testnet_accept_both_modes_while_mainnet_requires_dcap() {
        let genesis_hash = B256::repeat_byte(0x33);
        assert_eq!(GRAMINE_DIRECT_DEV_CHAIN_ID, DEVNET_CHAIN_ID);
        assert_ne!(GRAMINE_DIRECT_DEV_CHAIN_ID, TESTNET_CHAIN_ID);
        assert!(initial_tee_policy_v1(
            InitialTeeProfileV1::DcapRequired(dcap_measurement()),
            TESTNET_CHAIN_ID,
            genesis_hash,
        )
        .is_ok());
        assert!(initial_tee_policy_v1(
            InitialTeeProfileV1::DcapRequired(dcap_measurement()),
            MAINNET_CHAIN_ID,
            genesis_hash,
        )
        .is_ok());
        assert!(initial_tee_policy_v1(
            InitialTeeProfileV1::DcapRequired(dcap_measurement()),
            DEVNET_CHAIN_ID,
            genesis_hash,
        )
        .is_ok());
        assert!(initial_tee_policy_v1(
            InitialTeeProfileV1::DcapRequired(dcap_measurement()),
            DEVNET_CHAIN_ID + 1,
            genesis_hash,
        )
        .unwrap_err()
        .contains("devnet, testnet, or mainnet"));
        assert!(initial_tee_policy_v1(
            InitialTeeProfileV1::GramineDirectDev,
            TESTNET_CHAIN_ID,
            genesis_hash,
        )
        .is_ok());
        assert!(initial_tee_policy_v1(
            InitialTeeProfileV1::GramineDirectDev,
            DEVNET_CHAIN_ID,
            genesis_hash,
        )
        .is_ok());
        assert!(initial_tee_policy_v1(
            InitialTeeProfileV1::GramineDirectDev,
            MAINNET_CHAIN_ID,
            genesis_hash,
        )
        .is_err());
        assert!(initial_tee_policy_v1(
            InitialTeeProfileV1::GramineDirectDev,
            TESTNET_CHAIN_ID + 1,
            genesis_hash,
        )
        .unwrap_err()
        .contains("devnet or testnet chain ID"));
    }

    #[test]
    fn dcap_policy_pins_one_role_neutral_release_measurement() {
        let measurement = dcap_measurement();
        let policy = initial_tee_policy_v1(
            InitialTeeProfileV1::DcapRequired(measurement),
            TESTNET_CHAIN_ID,
            B256::repeat_byte(0x33),
        )
        .unwrap();
        assert_eq!(policy.attestation_mode, AttestationMode::DcapRequired);
        assert_eq!(policy.measurement_rules.len(), 1);
        for rule in &policy.measurement_rules {
            assert_eq!(rule.mrenclave, measurement.mrenclave);
            assert_eq!(rule.mrsigner, measurement.mrsigner);
            assert_eq!(rule.isv_prod_id, measurement.isv_prod_id);
            assert_eq!(rule.minimum_isv_svn, measurement.minimum_isv_svn);
        }
        assert_eq!(
            policy.accepted_platform_tcb_statuses,
            PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded
        );
        assert_eq!(policy.accepted_qe_tcb_status, QvlTcbStatusV1::UpToDate);
        assert_eq!(policy.minimum_lease, 3_600);
        assert_eq!(policy.maximum_lease, 14 * SECONDS_PER_DAY);
    }

    #[test]
    fn dcap_policy_rejects_zero_release_measurements() {
        let genesis_hash = B256::repeat_byte(0x33);
        for measurement in [
            ProductionSgxMeasurementV1 {
                mrenclave: B256::ZERO,
                ..dcap_measurement()
            },
            ProductionSgxMeasurementV1 {
                mrsigner: B256::ZERO,
                ..dcap_measurement()
            },
            ProductionSgxMeasurementV1 {
                minimum_tcb_evaluation_data_number: 0,
                ..dcap_measurement()
            },
        ] {
            assert!(initial_tee_policy_v1(
                InitialTeeProfileV1::DcapRequired(measurement),
                TESTNET_CHAIN_ID,
                genesis_hash,
            )
            .is_err());
        }
    }

    #[test]
    fn development_field_is_canonical_but_not_hardware_evidence() {
        let policy = initial_tee_policy_v1(
            InitialTeeProfileV1::GramineDirectDev,
            GRAMINE_DIRECT_DEV_CHAIN_ID,
            B256::repeat_byte(0x44),
        )
        .unwrap();
        let field = tee_attestation_v1_genesis_field(&policy).unwrap();
        let encoded = field["policySchedule"]
            .as_str()
            .unwrap()
            .strip_prefix("0x")
            .unwrap();
        let schedule =
            TeePolicyScheduleV1::decode_canonical(&hex::decode(encoded).unwrap()).unwrap();
        assert_eq!(
            schedule.active_policy(1).unwrap().attestation_mode,
            AttestationMode::GramineDirectDev
        );
        assert_eq!(schedule.active_policy(1).unwrap(), &policy);
        assert_eq!(
            field["policyScheduleHash"],
            json!(schedule.schedule_hash().unwrap())
        );
        assert_eq!(policy.measurement_rules.len(), 1);
        let rule = &policy.measurement_rules[0];
        assert_eq!(
            rule.mrenclave,
            keccak256(b"outbe/tee/gramine-direct-dev/mrenclave/v1")
        );
        assert_eq!(
            rule.mrsigner,
            keccak256(b"outbe/tee/gramine-direct-dev/mrsigner/v1")
        );
        assert_ne!(rule.mrenclave, rule.mrsigner);
    }
}
