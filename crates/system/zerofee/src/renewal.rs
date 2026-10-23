//! Own-lease `renewEnclave` fee waiver.

use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS,
    storage::{dsl::Map, StorageHandle},
    tee_attestation_v1::{
        AttestationEvidenceV1, AttestationMode, AttestationOperationV1, RegistryMutatorV1,
        TeeRegistryGasScheduleV1, MAX_ACTIVE_MEASUREMENT_RULES, MAX_ATTESTATION_EVIDENCE_BYTES,
        MAX_EVIDENCE_CALL_FRAMING_BYTES,
    },
    tee_registry_abi_v1::ITeeRegistryV1,
    zero_fee::VALIDATOR_V1_NODE_HASH_SLOT,
};

use crate::envelope::ZeroFeeEnvelope;
use crate::hooks::{
    ZeroFeeAuthorization, ZeroFeeCandidate, ZeroFeeHook, ZeroFeeHookId, ZeroFeePolicyError,
    ZeroFeeTransaction,
};

/// Own-lease renewal hook. Oracle and checkpoint hooks do not use it.
pub(crate) struct RenewEnclaveHook;

const RENEWAL_CALLDATA_LIMIT: usize =
    MAX_ATTESTATION_EVIDENCE_BYTES + MAX_EVIDENCE_CALL_FRAMING_BYTES;

fn renewal_gas_limit() -> u64 {
    TeeRegistryGasScheduleV1::normative()
        .maximum_transaction_gas(
            RegistryMutatorV1::RenewEnclave,
            RENEWAL_CALLDATA_LIMIT,
            MAX_ATTESTATION_EVIDENCE_BYTES,
            MAX_ACTIVE_MEASUREMENT_RULES,
            AttestationMode::DcapRequired,
        )
        .expect("normative RenewEnclave gas ceiling")
}

fn envelope() -> ZeroFeeEnvelope {
    ZeroFeeEnvelope {
        target: TEE_REGISTRY_ADDRESS,
        min_max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_calldata_bytes: RENEWAL_CALLDATA_LIMIT,
        max_gas_limit: renewal_gas_limit(),
        malformed_reason: "renewEnclave(bytes,bytes,bytes) decode failed",
    }
}

fn malformed(reason: &str) -> ZeroFeePolicyError {
    ZeroFeePolicyError::MalformedCalldata(reason.to_string())
}

fn canonical_node_hash(
    call: &ITeeRegistryV1::renewEnclaveCall,
    input: &[u8],
) -> Result<B256, ZeroFeePolicyError> {
    if call.nodeSignature.len() != 65 || call.enclaveSignature.len() != 64 {
        return Err(malformed("renewEnclave signature length is not canonical"));
    }
    let evidence = call.evidence.as_ref();
    if evidence.len() > MAX_ATTESTATION_EVIDENCE_BYTES {
        return Err(malformed("renewEnclave evidence exceeds its limit"));
    }
    let framing = input
        .len()
        .checked_sub(evidence.len())
        .ok_or_else(|| malformed("renewEnclave evidence is longer than its calldata"))?;
    if framing > MAX_EVIDENCE_CALL_FRAMING_BYTES {
        return Err(malformed("renewEnclave framing exceeds its limit"));
    }
    if call.abi_encode().as_slice() != input {
        return Err(malformed("renewEnclave calldata is not canonical"));
    }
    let decoded = AttestationEvidenceV1::decode_canonical(evidence)
        .map_err(|_| malformed("renewEnclave evidence is not canonical"))?;
    if decoded.intent().operation != AttestationOperationV1::RenewEnclave {
        return Err(malformed("renewEnclave evidence operation is not renewal"));
    }
    decoded
        .intent()
        .node_id
        .node_id_hash()
        .map_err(|_| malformed("renewEnclave node id is not canonical"))
}

fn associated_node(
    storage: StorageHandle<'_>,
    signer: Address,
) -> Result<B256, ZeroFeePolicyError> {
    Ok(Map::<Address, B256>::new(
        U256::from(VALIDATOR_V1_NODE_HASH_SLOT),
        TEE_REGISTRY_ADDRESS,
        storage,
    )
    .read(&signer)?)
}

impl ZeroFeeHook for RenewEnclaveHook {
    fn id(&self) -> ZeroFeeHookId {
        ZeroFeeHookId::RenewEnclave
    }

    fn classify(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
        let Some(candidate) =
            envelope().classify::<ITeeRegistryV1::renewEnclaveCall>(self.id(), tx)?
        else {
            return Ok(None);
        };
        let call = ITeeRegistryV1::renewEnclaveCall::abi_decode(tx.call.input)
            .map_err(|_| malformed("renewEnclave(bytes,bytes,bytes) decode failed"))?;
        let node_id_hash = canonical_node_hash(&call, tx.call.input)?;
        Ok(Some(ZeroFeeCandidate {
            hook: candidate.hook,
            signer: candidate.signer,
            node_id_hash,
        }))
    }

    fn authorize_fee_waiver(
        &self,
        storage: StorageHandle<'_>,
        candidate: ZeroFeeCandidate,
    ) -> Result<ZeroFeeAuthorization, ZeroFeePolicyError> {
        if candidate.node_id_hash.is_zero()
            || associated_node(storage, candidate.signer)? != candidate.node_id_hash
        {
            return Err(ZeroFeePolicyError::UnauthorizedSigner);
        }
        Ok(ZeroFeeAuthorization {
            hook: self.id(),
            subject: candidate.signer,
        })
    }
}

#[cfg(test)]
mod tests {
    use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
    use alloy_primitives::{Address, Bytes, B256, U256};
    use alloy_sol_types::SolCall;
    use outbe_primitives::{
        addresses::TEE_REGISTRY_ADDRESS,
        error::PrecompileError,
        storage::{dsl::Map, hashmap::HashMapStorageProvider, StorageHandle},
        tee_attestation_v1::{
            AttestationEvidenceV1, AttestationMode, AttestationOperationV1,
            GramineDirectEvidenceV1, NodeIdV1, RegistrationIntentV1,
            MAX_ATTESTATION_EVIDENCE_BYTES, MAX_EVIDENCE_CALL_FRAMING_BYTES,
        },
        tee_registry_abi_v1::ITeeRegistryV1,
        transaction_call::TransactionCallFields,
        zero_fee::VALIDATOR_V1_NODE_HASH_SLOT,
    };

    use crate::hooks::{
        ZeroFeeAuthorization, ZeroFeeCandidate, ZeroFeeHookId, ZeroFeePolicyError,
        ZeroFeeTransaction,
    };

    const SIGNER: Address = Address::repeat_byte(0x44);
    const NODE_PUBLIC: [u8; 33] = [
        0x02, 0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0, 0x62, 0x95, 0xce, 0x87,
        0x0b, 0x07, 0x02, 0x9b, 0xfc, 0xdb, 0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81, 0x5b, 0x16,
        0xf8, 0x17, 0x98,
    ];
    const CALLDATA_LIMIT: usize = 933_888;
    const GAS_LIMIT: u64 = 28_668_784;

    fn intent(operation: AttestationOperationV1) -> RegistrationIntentV1 {
        RegistrationIntentV1 {
            chain_id: [1; 32],
            genesis_hash: B256::repeat_byte(1),
            operation,
            attestation_mode: AttestationMode::GramineDirectDev,
            policy_hash: B256::repeat_byte(2),
            node_id: NodeIdV1 {
                reth_p2p_public: NODE_PUBLIC,
            },
            enclave_id: B256::repeat_byte(3),
            binding_id: B256::repeat_byte(4),
            binding_version: 1,
            registration_version: 1,
            renewal_nonce: 1,
            transition_nonce: 0,
            requested_valid_until: 1,
            recipient_x25519: [5; 32],
            attestation_ed25519: [6; 32],
            noise_responder_x25519: [7; 32],
            node_host_authorization_hash: B256::repeat_byte(8),
        }
    }

    fn node_hash() -> B256 {
        intent(AttestationOperationV1::RenewEnclave)
            .node_id
            .node_id_hash()
            .unwrap()
    }

    fn renew_call(
        operation: AttestationOperationV1,
        node_len: usize,
        enclave_len: usize,
    ) -> Vec<u8> {
        let evidence = AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
            intent: intent(operation),
            dev_attestation_public: [6; 32],
            dev_signature: [9; 64],
            transition_key_ready_proof: None,
        })
        .encode_canonical()
        .unwrap();
        ITeeRegistryV1::renewEnclaveCall {
            evidence: Bytes::from(evidence),
            nodeSignature: Bytes::from(vec![1; node_len]),
            enclaveSignature: Bytes::from(vec![2; enclave_len]),
        }
        .abi_encode()
    }

    fn accepted<'a>(input: &'a [u8]) -> ZeroFeeTransaction<'a> {
        ZeroFeeTransaction {
            signer: SIGNER,
            call: TransactionCallFields {
                to: Some(TEE_REGISTRY_ADDRESS),
                value: U256::ZERO,
                input,
                gas_limit: 1_000_000,
                max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
                max_priority_fee_per_gas: Some(0),
            },
        }
    }

    fn classify(input: &[u8]) -> Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
        crate::registry().classify(&accepted(input))
    }

    fn with_binding(stored: Option<B256>, body: impl FnOnce(StorageHandle<'_>)) {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            if let Some(hash) = stored {
                Map::<Address, B256>::new(
                    U256::from(VALIDATOR_V1_NODE_HASH_SLOT),
                    TEE_REGISTRY_ADDRESS,
                    storage.clone(),
                )
                .write(&SIGNER, hash)
                .unwrap();
            }
            body(storage);
            Ok::<(), PrecompileError>(())
        })
        .unwrap();
    }

    fn word(input: &[u8], at: usize) -> usize {
        let bytes: [u8; 8] = input[at + 24..at + 32].try_into().unwrap();
        usize::try_from(u64::from_be_bytes(bytes)).unwrap()
    }

    fn put_word(input: &mut [u8], at: usize, value: usize) {
        let encoded = u64::try_from(value).unwrap().to_be_bytes();
        input[at..at + 32].fill(0);
        input[at + 24..at + 32].copy_from_slice(&encoded);
    }

    fn with_gap(canonical: &[u8], gap: usize) -> Vec<u8> {
        let evidence_offset = word(canonical, 4);
        let node_offset = word(canonical, 36);
        let evidence_len = word(canonical, 4 + evidence_offset);
        let padded = evidence_len.div_ceil(32) * 32;
        let insert_at = 4 + evidence_offset + 32 + padded;
        assert_eq!(insert_at, 4 + node_offset);
        let mut out = Vec::with_capacity(canonical.len() + gap);
        out.extend_from_slice(&canonical[..insert_at]);
        out.extend(std::iter::repeat_n(0, gap));
        out.extend_from_slice(&canonical[insert_at..]);
        put_word(&mut out, 36, node_offset + gap);
        put_word(&mut out, 68, word(canonical, 68) + gap);
        out
    }

    #[test]
    fn associated_signer_renewal_is_waived() {
        let input = renew_call(AttestationOperationV1::RenewEnclave, 65, 64);
        let hash = node_hash();
        let candidate = classify(&input).unwrap().unwrap();
        assert_eq!(
            candidate,
            ZeroFeeCandidate {
                hook: ZeroFeeHookId::RenewEnclave,
                signer: SIGNER,
                node_id_hash: hash,
            }
        );
        with_binding(Some(hash), |storage| {
            assert_eq!(
                crate::registry().authorize_fee_waiver(storage, candidate),
                Ok(ZeroFeeAuthorization {
                    hook: ZeroFeeHookId::RenewEnclave,
                    subject: SIGNER,
                })
            );
        });
    }

    #[test]
    fn wrong_missing_and_zero_associations_are_rejected() {
        let input = renew_call(AttestationOperationV1::RenewEnclave, 65, 64);
        let candidate = classify(&input).unwrap().unwrap();
        with_binding(Some(B256::repeat_byte(0xab)), |storage| {
            assert_eq!(
                crate::registry().authorize_fee_waiver(storage, candidate),
                Err(ZeroFeePolicyError::UnauthorizedSigner)
            );
        });
        with_binding(None, |storage| {
            assert_eq!(
                crate::registry().authorize_fee_waiver(storage, candidate),
                Err(ZeroFeePolicyError::UnauthorizedSigner)
            );
        });
        with_binding(Some(B256::ZERO), |storage| {
            assert_eq!(
                crate::registry().authorize_fee_waiver(
                    storage,
                    ZeroFeeCandidate {
                        node_id_hash: B256::ZERO,
                        ..candidate
                    },
                ),
                Err(ZeroFeePolicyError::UnauthorizedSigner)
            );
        });
    }

    #[test]
    fn paid_and_unrelated_renewal_shapes_stay_unclaimed() {
        let input = renew_call(AttestationOperationV1::RenewEnclave, 65, 64);
        let mut tipped = accepted(&input);
        tipped.call.max_priority_fee_per_gas = Some(1);
        let mut legacy = accepted(&input);
        legacy.call.max_priority_fee_per_gas = None;
        let register = ITeeRegistryV1::registerEnclaveCall::SELECTOR.to_vec();
        assert_eq!(crate::registry().classify(&tipped), Ok(None));
        assert_eq!(crate::registry().classify(&legacy), Ok(None));
        assert_eq!(classify(&register), Ok(None));
    }

    #[test]
    fn wrong_operation_and_signature_lengths_are_malformed() {
        assert_eq!(
            classify(&renew_call(AttestationOperationV1::RegisterEnclave, 65, 64)),
            Err(ZeroFeePolicyError::MalformedCalldata(
                "renewEnclave evidence operation is not renewal".to_string()
            ))
        );
        assert_eq!(
            classify(&renew_call(AttestationOperationV1::RenewEnclave, 64, 64)),
            Err(ZeroFeePolicyError::MalformedCalldata(
                "renewEnclave signature length is not canonical".to_string()
            ))
        );
        assert_eq!(
            classify(&renew_call(AttestationOperationV1::RenewEnclave, 65, 63)),
            Err(ZeroFeePolicyError::MalformedCalldata(
                "renewEnclave signature length is not canonical".to_string()
            ))
        );
    }

    #[test]
    fn canonical_bounds_use_distinct_typed_errors() {
        let canonical = renew_call(AttestationOperationV1::RenewEnclave, 65, 64);
        let gapped = with_gap(&canonical, 32);
        assert!(gapped.len() < CALLDATA_LIMIT);
        assert_eq!(
            classify(&gapped),
            Err(ZeroFeePolicyError::MalformedCalldata(
                "renewEnclave calldata is not canonical".to_string()
            ))
        );

        let wide = with_gap(&canonical, MAX_EVIDENCE_CALL_FRAMING_BYTES);
        assert!(wide.len() < CALLDATA_LIMIT);
        assert_eq!(
            classify(&wide),
            Err(ZeroFeePolicyError::MalformedCalldata(
                "renewEnclave framing exceeds its limit".to_string()
            ))
        );

        let mut oversized_evidence = vec![0; MAX_ATTESTATION_EVIDENCE_BYTES + 1];
        oversized_evidence[..4].copy_from_slice(b"evid");
        let oversized = ITeeRegistryV1::renewEnclaveCall {
            evidence: Bytes::from(oversized_evidence),
            nodeSignature: Bytes::from(vec![1; 65]),
            enclaveSignature: Bytes::from(vec![2; 64]),
        }
        .abi_encode();
        assert!(oversized.len() < CALLDATA_LIMIT);
        assert_eq!(
            classify(&oversized),
            Err(ZeroFeePolicyError::MalformedCalldata(
                "renewEnclave evidence exceeds its limit".to_string()
            ))
        );

        let mut too_long = ITeeRegistryV1::renewEnclaveCall::SELECTOR.to_vec();
        too_long.resize(CALLDATA_LIMIT + 1, 0);
        assert_eq!(
            classify(&too_long),
            Err(ZeroFeePolicyError::CalldataTooLarge {
                size: CALLDATA_LIMIT + 1,
                limit: CALLDATA_LIMIT,
            })
        );

        let mut greedy = accepted(&canonical);
        greedy.call.gas_limit = GAS_LIMIT + 1;
        assert_eq!(
            crate::registry().classify(&greedy),
            Err(ZeroFeePolicyError::GasLimitTooHigh {
                gas_limit: GAS_LIMIT + 1,
                limit: GAS_LIMIT,
            })
        );
    }
}
