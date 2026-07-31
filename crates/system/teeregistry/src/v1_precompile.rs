//! Inactive-until-I9 ABI for TeeRegistry V1 node-enclave registration.
//!
//! The production EVM route deliberately continues to point at the legacy ABI.
//! I3/I4 expose this dispatcher only through the `tee-attestation-v1` feature so
//! its canonical framing, gas and state-machine boundary can be tested before
//! the A0 activation owned by I9.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use outbe_primitives::{
    dispatch::{dispatch_call, reject_value, view},
    error::{PrecompileError, Result},
    storage::{gas::PRECOMPILE_BASE_GAS, StorageHandle},
    tee_attestation_v1::{
        AttestationMode, RegistryMutatorV1, TeeRegistryGasScheduleV1,
        MAX_ATTESTATION_EVIDENCE_BYTES, MAX_EVIDENCE_CALL_FRAMING_BYTES,
    },
};

use crate::{NodeEnclaveBindingV1, TeeRegistry, V1RegistrationOutcome};

sol! {
    #[derive(Debug, PartialEq, Eq)]
    struct NodeEnclaveBindingV1View {
        bool exists;
        bytes32 nodeIdHash;
        bytes32 enclaveId;
        bytes32 bindingId;
        bytes32 intentHash;
        bytes32 evidenceHash;
        bytes32 policyHash;
        uint64 bindingVersion;
        uint64 registrationVersion;
        uint64 renewalNonce;
        uint64 transitionNonce;
        uint64 validUntil;
        uint64 collateralValidUntil;
        bytes32 recipientX25519;
        bytes32 attestationEd25519;
        bytes32 noiseResponderX25519;
        bytes32 mrenclave;
        bytes32 mrsigner;
        uint16 isvProdId;
        uint16 isvSvn;
        uint8 platformTcbStatus;
        bytes32 verdictHash;
    }

    interface ITeeRegistryV1 {
        function registerEnclave(
            bytes calldata evidence,
            bytes calldata nodeSignature,
            bytes calldata enclaveSignature
        ) external returns (bool);

        function validatorEnclaveBinding(address validator)
            external
            view
            returns (NodeEnclaveBindingV1View memory);

        function fullNodeEnclaveBinding(uint8 rethP2pPrefix, bytes32 rethP2pX)
            external
            view
            returns (NodeEnclaveBindingV1View memory);

        function isValidatorEnclaveReady(address validator) external view returns (bool);

        function isFullNodeEnclaveReady(uint8 rethP2pPrefix, bytes32 rethP2pX)
            external
            view
            returns (bool);
    }
}

/// Feature-gated V1 ABI entry point. The caller is deliberately ignored for
/// registration authority: both proofs of possession are bound to the exact
/// canonical intent, so any account may relay the transaction.
pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;

    let register_preflight =
        if data.get(..4) == Some(ITeeRegistryV1::registerEnclaveCall::SELECTOR.as_slice()) {
            if storage.is_static()? {
                return Err(PrecompileError::WriteProtection);
            }
            Some(preflight_register_call(data)?)
        } else {
            None
        };

    let active_policy = if let Some(preflight) = register_preflight {
        let registry = TeeRegistry::new(storage.clone());
        let policy = registry.active_policy_v1()?;
        deduct_register_protocol_gas(
            &storage,
            data.len(),
            preflight.evidence.len(),
            policy.measurement_rules.len(),
        )?;
        Some(policy)
    } else {
        None
    };

    dispatch_call(
        data,
        ITeeRegistryV1::ITeeRegistryV1Calls::abi_decode,
        |call| {
            use ITeeRegistryV1::ITeeRegistryV1Calls::*;
            let mut registry = TeeRegistry::new(storage);
            match call {
                registerEnclave(_) => {
                    let policy = active_policy.as_ref().ok_or_else(|| {
                        PrecompileError::Fatal("V1 registration preflight was bypassed".into())
                    })?;
                    let preflight = register_preflight.ok_or_else(|| {
                        PrecompileError::Fatal("V1 registration preflight was bypassed".into())
                    })?;
                    let node_signature: [u8; 65] =
                        preflight.node_signature.try_into().map_err(|_| {
                            PrecompileError::Fatal("preflight node signature mismatch".into())
                        })?;
                    let enclave_signature: [u8; 64] =
                        preflight.enclave_signature.try_into().map_err(|_| {
                            PrecompileError::Fatal("preflight enclave signature mismatch".into())
                        })?;
                    let outcome = registry.register_enclave_with_active_policy_v1(
                        preflight.evidence,
                        &node_signature,
                        &enclave_signature,
                        policy,
                    )?;
                    Ok(Bytes::from(
                        ITeeRegistryV1::registerEnclaveCall::abi_encode_returns(&matches!(
                            outcome,
                            V1RegistrationOutcome::Created
                        )),
                    ))
                }
                validatorEnclaveBinding(call) => view(call, |call| {
                    Ok(binding_view(
                        registry.validator_enclave_binding_v1(call.validator)?,
                    ))
                }),
                fullNodeEnclaveBinding(call) => view(call, |call| {
                    Ok(binding_view(registry.full_node_enclave_binding_v1(
                        full_node_public_key(call.rethP2pPrefix, call.rethP2pX),
                    )?))
                }),
                isValidatorEnclaveReady(call) => view(call, |call| {
                    registry.is_validator_enclave_ready_v1(call.validator)
                }),
                isFullNodeEnclaveReady(call) => view(call, |call| {
                    registry.is_full_node_enclave_ready_v1(full_node_public_key(
                        call.rethP2pPrefix,
                        call.rethP2pX,
                    ))
                }),
            }
        },
    )
}

fn deduct_register_protocol_gas(
    storage: &StorageHandle<'_>,
    input_len: usize,
    evidence_len: usize,
    active_rule_count: usize,
) -> Result<()> {
    let schedule = TeeRegistryGasScheduleV1::normative();
    let maximum_transaction_gas = schedule
        .maximum_transaction_gas(
            RegistryMutatorV1::RegisterEnclave,
            input_len,
            evidence_len,
            active_rule_count,
            AttestationMode::DcapRequired,
        )
        .map_err(|error| {
            PrecompileError::Revert(format!("invalid V1 registration gas dimensions: {error}"))
        })?;
    let intrinsic = schedule
        .maximum_calldata_intrinsic_gas(input_len)
        .map_err(|error| {
            PrecompileError::Revert(format!("invalid V1 calldata gas dimensions: {error}"))
        })?;
    let protocol = maximum_transaction_gas
        .checked_sub(intrinsic)
        .ok_or_else(|| PrecompileError::Fatal("V1 protocol gas underflow".into()))?;
    let storage_allowance = schedule.register_storage_gas_allowance();
    let prepaid_protocol = protocol.checked_sub(storage_allowance).ok_or_else(|| {
        PrecompileError::Fatal("V1 register storage allowance exceeds fixed gas".into())
    })?;
    let dispatch_charge = prepaid_protocol
        .checked_sub(PRECOMPILE_BASE_GAS)
        .ok_or_else(|| {
            PrecompileError::Fatal("V1 protocol gas is below the precompile base charge".into())
        })?;
    storage.deduct_gas(dispatch_charge)
}

#[derive(Clone, Copy)]
struct RegisterPreflight<'a> {
    evidence: &'a [u8],
    node_signature: &'a [u8],
    enclave_signature: &'a [u8],
}

/// Validates the complete canonical ABI layout without allocating dynamic
/// arguments. Signature lengths, aggregate evidence cap, call-framing cap,
/// offsets, zero padding and trailing bytes reject before policy allocation or
/// native QVL execution.
fn preflight_register_call(data: &[u8]) -> Result<RegisterPreflight<'_>> {
    const HEAD_WORDS: usize = 3;
    const HEAD_LEN: usize = HEAD_WORDS * 32;

    let args = data
        .get(4..)
        .ok_or_else(|| invalid_register_abi("missing function selector"))?;
    let head = args
        .get(..HEAD_LEN)
        .ok_or_else(|| invalid_register_abi("truncated argument head"))?;
    let evidence_offset = abi_usize(&head[0..32])?;
    let node_signature_offset = abi_usize(&head[32..64])?;
    let enclave_signature_offset = abi_usize(&head[64..96])?;
    if evidence_offset != HEAD_LEN {
        return Err(invalid_register_abi("non-canonical evidence offset"));
    }

    let (evidence, next_offset) = dynamic_bytes(args, evidence_offset)?;
    if evidence.len() > MAX_ATTESTATION_EVIDENCE_BYTES {
        return Err(PrecompileError::Revert(format!(
            "attestation evidence exceeds {} bytes",
            MAX_ATTESTATION_EVIDENCE_BYTES
        )));
    }
    if node_signature_offset != next_offset {
        return Err(invalid_register_abi("non-canonical node signature offset"));
    }
    let (node_signature, next_offset) = dynamic_bytes(args, node_signature_offset)?;
    if node_signature.len() != 65 {
        return Err(PrecompileError::Revert(
            "node proof-of-possession signature must be 65 bytes".into(),
        ));
    }
    if enclave_signature_offset != next_offset {
        return Err(invalid_register_abi(
            "non-canonical enclave signature offset",
        ));
    }
    let (enclave_signature, final_offset) = dynamic_bytes(args, enclave_signature_offset)?;
    if enclave_signature.len() != 64 {
        return Err(PrecompileError::Revert(
            "enclave proof-of-possession signature must be 64 bytes".into(),
        ));
    }
    if final_offset != args.len() {
        return Err(invalid_register_abi("trailing ABI bytes"));
    }
    let framing_len = data
        .len()
        .checked_sub(evidence.len())
        .ok_or_else(|| invalid_register_abi("evidence length exceeds calldata"))?;
    if framing_len > MAX_EVIDENCE_CALL_FRAMING_BYTES {
        return Err(PrecompileError::Revert(format!(
            "evidence call framing exceeds {} bytes",
            MAX_EVIDENCE_CALL_FRAMING_BYTES
        )));
    }

    Ok(RegisterPreflight {
        evidence,
        node_signature,
        enclave_signature,
    })
}

/// Hardware-free I3 boundary: canonical ABI and full gas precharge stay real;
/// only the already-authenticated enclave outcome is supplied as a typed,
/// test-only capability.
#[cfg(test)]
pub(crate) fn dispatch_register_after_verifier_for_test(
    storage: StorageHandle<'_>,
    data: &[u8],
    intent: &outbe_primitives::tee_attestation_v1::RegistrationIntentV1,
    capability: crate::v1::PostVerifierDcapCapabilityV1,
) -> Result<V1RegistrationOutcome> {
    let preflight = preflight_register_call(data)?;
    let policy = TeeRegistry::new(storage.clone()).active_policy_v1()?;
    deduct_register_protocol_gas(
        &storage,
        data.len(),
        preflight.evidence.len(),
        policy.measurement_rules.len(),
    )?;
    let node_signature: [u8; 65] = preflight
        .node_signature
        .try_into()
        .map_err(|_| PrecompileError::Fatal("preflight node signature mismatch".into()))?;
    let enclave_signature: [u8; 64] = preflight
        .enclave_signature
        .try_into()
        .map_err(|_| PrecompileError::Fatal("preflight enclave signature mismatch".into()))?;
    TeeRegistry::new(storage).register_enclave_after_verifier_with_active_policy_for_test(
        intent,
        &node_signature,
        &enclave_signature,
        &policy,
        capability,
    )
}

fn dynamic_bytes(args: &[u8], offset: usize) -> Result<(&[u8], usize)> {
    let length_word_end = offset
        .checked_add(32)
        .ok_or_else(|| invalid_register_abi("dynamic offset overflow"))?;
    let length = abi_usize(
        args.get(offset..length_word_end)
            .ok_or_else(|| invalid_register_abi("truncated dynamic length"))?,
    )?;
    let value_end = length_word_end
        .checked_add(length)
        .ok_or_else(|| invalid_register_abi("dynamic length overflow"))?;
    let value = args
        .get(length_word_end..value_end)
        .ok_or_else(|| invalid_register_abi("truncated dynamic value"))?;
    let padded_length = length
        .checked_add(31)
        .map(|length| length / 32 * 32)
        .ok_or_else(|| invalid_register_abi("dynamic padding overflow"))?;
    let padded_end = length_word_end
        .checked_add(padded_length)
        .ok_or_else(|| invalid_register_abi("dynamic padding overflow"))?;
    let padding = args
        .get(value_end..padded_end)
        .ok_or_else(|| invalid_register_abi("truncated dynamic padding"))?;
    if padding.iter().any(|byte| *byte != 0) {
        return Err(invalid_register_abi("non-zero dynamic padding"));
    }
    Ok((value, padded_end))
}

fn abi_usize(word: &[u8]) -> Result<usize> {
    let width = core::mem::size_of::<usize>();
    if word.len() != 32 || word[..32 - width].iter().any(|byte| *byte != 0) {
        return Err(invalid_register_abi("ABI integer exceeds host usize"));
    }
    let mut value = [0_u8; core::mem::size_of::<usize>()];
    value.copy_from_slice(&word[32 - width..]);
    Ok(usize::from_be_bytes(value))
}

fn invalid_register_abi(reason: &'static str) -> PrecompileError {
    PrecompileError::Revert(format!("invalid canonical V1 registration ABI: {reason}"))
}

fn full_node_public_key(prefix: u8, x: B256) -> [u8; 33] {
    let mut public = [0_u8; 33];
    public[0] = prefix;
    public[1..].copy_from_slice(x.as_slice());
    public
}

fn binding_view(binding: Option<NodeEnclaveBindingV1>) -> NodeEnclaveBindingV1View {
    let Some(binding) = binding else {
        return NodeEnclaveBindingV1View {
            exists: false,
            nodeIdHash: B256::ZERO,
            enclaveId: B256::ZERO,
            bindingId: B256::ZERO,
            intentHash: B256::ZERO,
            evidenceHash: B256::ZERO,
            policyHash: B256::ZERO,
            bindingVersion: 0,
            registrationVersion: 0,
            renewalNonce: 0,
            transitionNonce: 0,
            validUntil: 0,
            collateralValidUntil: 0,
            recipientX25519: B256::ZERO,
            attestationEd25519: B256::ZERO,
            noiseResponderX25519: B256::ZERO,
            mrenclave: B256::ZERO,
            mrsigner: B256::ZERO,
            isvProdId: 0,
            isvSvn: 0,
            platformTcbStatus: 0,
            verdictHash: B256::ZERO,
        };
    };
    NodeEnclaveBindingV1View {
        exists: true,
        nodeIdHash: binding.node_id_hash,
        enclaveId: binding.enclave_id,
        bindingId: binding.binding_id,
        intentHash: binding.intent_hash,
        evidenceHash: binding.evidence_hash,
        policyHash: binding.policy_hash,
        bindingVersion: binding.binding_version,
        registrationVersion: binding.registration_version,
        renewalNonce: binding.renewal_nonce,
        transitionNonce: binding.transition_nonce,
        validUntil: binding.valid_until,
        collateralValidUntil: binding.collateral_valid_until,
        recipientX25519: binding.recipient_x25519,
        attestationEd25519: binding.attestation_ed25519,
        noiseResponderX25519: binding.noise_responder_x25519,
        mrenclave: binding.mrenclave,
        mrsigner: binding.mrsigner,
        isvProdId: binding.isv_prod_id,
        isvSvn: binding.isv_svn,
        platformTcbStatus: binding.platform_tcb_status,
        verdictHash: binding.verdict_hash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_sol_types::SolValue;
    use outbe_primitives::{
        storage::{hashmap::HashMapStorageProvider, PrecompileStorageProvider},
        tee_attestation_v1::{
            EnclaveProfile, PlatformTcbStatusSetV1, QvlTcbStatusV1, ResourceScheduleV1,
            TeeMeasurementRuleV1, TeePolicyV1,
        },
    };

    const GENESIS: B256 = B256::repeat_byte(0x31);

    fn policy() -> TeePolicyV1 {
        let gas = TeeRegistryGasScheduleV1::normative();
        let resources =
            ResourceScheduleV1::new(B256::repeat_byte(0x42), gas.schedule_hash().unwrap());
        let mut chain_id = [0_u8; 32];
        chain_id[24..].copy_from_slice(&31337_u64.to_be_bytes());
        TeePolicyV1 {
            policy_version: 1,
            chain_id,
            genesis_hash: GENESIS,
            activation_height: 1,
            predecessor_policy_hash: B256::ZERO,
            attestation_mode: AttestationMode::DcapRequired,
            intel_root_der_hash: B256::repeat_byte(0x43),
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
            accepted_platform_tcb_statuses: PlatformTcbStatusSetV1::UpToDateOrSWHardeningNeeded,
            accepted_qe_tcb_status: QvlTcbStatusV1::UpToDate,
            minimum_lease: 3_600,
            maximum_lease: 604_800,
            collateral_margin: 3_600,
            resource_schedule_hash: resources.schedule_hash().unwrap(),
            measurement_rules: vec![TeeMeasurementRuleV1 {
                enclave_profile: EnclaveProfile::Validator,
                mrenclave: B256::repeat_byte(0x45),
                mrsigner: B256::repeat_byte(0x46),
                isv_prod_id: 7,
                minimum_isv_svn: 2,
                admit_from_height: 1,
                admit_until_height_exclusive: 1_000,
            }],
        }
    }

    fn call(evidence: Vec<u8>, node_len: usize, enclave_len: usize) -> Vec<u8> {
        ITeeRegistryV1::registerEnclaveCall {
            evidence: Bytes::from(evidence),
            nodeSignature: Bytes::from(vec![0x51; node_len]),
            enclaveSignature: Bytes::from(vec![0x52; enclave_len]),
        }
        .abi_encode()
    }

    #[test]
    fn canonical_register_preflight_is_borrowed_and_rejects_noncanonical_framing() {
        let encoded = call(vec![1, 2, 3], 65, 64);
        assert_eq!(
            preflight_register_call(&encoded).unwrap().evidence,
            [1, 2, 3]
        );

        let mut wrong_offset = encoded.clone();
        wrong_offset[35] = 0x80;
        assert!(preflight_register_call(&wrong_offset).is_err());

        let mut nonzero_padding = encoded;
        let evidence_value_start = 4 + 96 + 32;
        nonzero_padding[evidence_value_start + 3] = 1;
        assert!(preflight_register_call(&nonzero_padding).is_err());
        assert!(preflight_register_call(&call(vec![1], 64, 64)).is_err());
        assert!(preflight_register_call(&call(vec![1], 65, 63)).is_err());
    }

    #[test]
    fn register_precharges_exact_protocol_gas_before_evidence_decode() {
        let mut provider = HashMapStorageProvider::new_with_chain_identity(31337, GENESIS);
        provider.set_block_number(1);
        provider
            .enter(|storage| TeeRegistry::new(storage).install_initial_policy_v1(&policy()))
            .unwrap();
        provider.enable_production_storage_gas_metering();

        // Canonical outer ABI, deliberately malformed canonical evidence. The
        // decoder rejection must occur only after the complete QVL/register
        // protocol charge has been reserved.
        let input = call(vec![1, 1, 0, 0, 0, 0], 65, 64);
        let schedule = TeeRegistryGasScheduleV1::normative();
        let total = schedule
            .maximum_transaction_gas(
                RegistryMutatorV1::RegisterEnclave,
                input.len(),
                6,
                1,
                AttestationMode::DcapRequired,
            )
            .unwrap();
        let intrinsic = schedule
            .maximum_calldata_intrinsic_gas(input.len())
            .unwrap();
        let storage_allowance = schedule.register_storage_gas_allowance();
        let dispatch_charge = total - intrinsic - PRECOMPILE_BASE_GAS - storage_allowance;

        // Actual production-shaped storage gas consumes the allowance already
        // included in `register_fixed`; it must never sit above the normative
        // maximum. No state write is reachable when the production enclave
        // verifier is unavailable.
        provider.set_gas_limit(u64::MAX);
        let result = provider
            .enter(|storage| dispatch(storage, &input, Address::repeat_byte(0x77), U256::ZERO));
        assert!(matches!(result, Err(PrecompileError::Fatal(_))));
        let (policy_reads, writes) = provider.metered_storage_operations();
        assert!(policy_reads > 0);
        assert_eq!(writes, 0);
        let storage_gas = policy_reads * 100;
        let exact_dispatch_gas = dispatch_charge + storage_gas;
        assert_eq!(provider.gas_used(), exact_dispatch_gas);
        assert!(storage_gas <= storage_allowance);
        assert_eq!(
            intrinsic + PRECOMPILE_BASE_GAS + provider.gas_used(),
            total - storage_allowance + storage_gas
        );
        assert!(intrinsic + PRECOMPILE_BASE_GAS + provider.gas_used() <= total);

        provider.set_gas_limit(exact_dispatch_gas);
        let result = provider
            .enter(|storage| dispatch(storage, &input, Address::repeat_byte(0x88), U256::ZERO));
        assert!(matches!(result, Err(PrecompileError::Fatal(_))));
        assert_eq!(provider.gas_used(), exact_dispatch_gas);
        assert_eq!(provider.metered_storage_operations(), (policy_reads, 0));

        provider.set_gas_limit(exact_dispatch_gas - 1);
        let result = provider
            .enter(|storage| dispatch(storage, &input, Address::repeat_byte(0x99), U256::ZERO));
        assert!(matches!(result, Err(PrecompileError::OutOfGas)));
        assert_eq!(provider.gas_used(), storage_gas);
        assert_eq!(provider.metered_storage_operations(), (policy_reads, 0));
    }

    #[test]
    fn cap_plus_one_rejects_before_policy_read_or_gas_charge() {
        let input = call(vec![0; MAX_ATTESTATION_EVIDENCE_BYTES + 1], 65, 64);
        let mut provider = HashMapStorageProvider::new_with_chain_identity(31337, GENESIS);
        provider.set_gas_limit(u64::MAX);
        let result = provider
            .enter(|storage| dispatch(storage, &input, Address::repeat_byte(0x99), U256::ZERO));
        assert!(matches!(result, Err(PrecompileError::Revert(_))));
        assert_eq!(provider.gas_used(), 0);
    }

    #[test]
    fn empty_binding_views_are_fixed_and_not_ready() {
        let validator = Address::repeat_byte(0x61);
        let mut provider = HashMapStorageProvider::new_with_chain_identity(31337, GENESIS);
        let ready_call = ITeeRegistryV1::isValidatorEnclaveReadyCall { validator };
        let ready = provider
            .enter(|storage| dispatch(storage, &ready_call.abi_encode(), Address::ZERO, U256::ZERO))
            .unwrap();
        assert!(!bool::abi_decode(&ready).unwrap());

        let binding_call = ITeeRegistryV1::validatorEnclaveBindingCall { validator };
        let encoded = provider
            .enter(|storage| {
                dispatch(
                    storage,
                    &binding_call.abi_encode(),
                    Address::ZERO,
                    U256::ZERO,
                )
            })
            .unwrap();
        let binding = NodeEnclaveBindingV1View::abi_decode(&encoded).unwrap();
        assert!(!binding.exists);
        assert_eq!(binding.nodeIdHash, B256::ZERO);
        assert_eq!(encoded.len(), 22 * 32);

        let p2p_signing = k256::ecdsa::SigningKey::from_bytes((&[0x62; 32]).into()).unwrap();
        let encoded_public = p2p_signing.verifying_key().to_encoded_point(true);
        let reth_p2p_prefix = encoded_public.as_bytes()[0];
        let reth_p2p_x = B256::from_slice(&encoded_public.as_bytes()[1..]);
        let ready_call = ITeeRegistryV1::isFullNodeEnclaveReadyCall {
            rethP2pPrefix: reth_p2p_prefix,
            rethP2pX: reth_p2p_x,
        };
        let ready = provider
            .enter(|storage| dispatch(storage, &ready_call.abi_encode(), Address::ZERO, U256::ZERO))
            .unwrap();
        assert!(!bool::abi_decode(&ready).unwrap());

        let binding_call = ITeeRegistryV1::fullNodeEnclaveBindingCall {
            rethP2pPrefix: reth_p2p_prefix,
            rethP2pX: reth_p2p_x,
        };
        let encoded = provider
            .enter(|storage| {
                dispatch(
                    storage,
                    &binding_call.abi_encode(),
                    Address::ZERO,
                    U256::ZERO,
                )
            })
            .unwrap();
        assert!(
            !NodeEnclaveBindingV1View::abi_decode(&encoded)
                .unwrap()
                .exists
        );

        let invalid = ITeeRegistryV1::isFullNodeEnclaveReadyCall {
            rethP2pPrefix: 0x04,
            rethP2pX: reth_p2p_x,
        };
        assert!(provider
            .enter(|storage| dispatch(storage, &invalid.abi_encode(), Address::ZERO, U256::ZERO))
            .is_err());
    }
}
