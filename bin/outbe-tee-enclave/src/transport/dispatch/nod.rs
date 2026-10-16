//! Network-key-only encrypted NOD commands.
use super::tribute::{resident_chain_id, response};
use crate::{
    errors::{EnclaveResult, TeeError, TransportDispatchError},
    keys::EnclaveKeys,
    transport::SharedTributeOfferKey,
};
use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    nod_materialization::ProtectedNodMaterializationV2, profile::poc_schema_limits,
};
use outbe_primitives::nod_encryption::EncryptedNodV2;
use outbe_tee::nod_state_materialization::{PrepareStateNodsRequest, StateNodAuthority};
use outbe_tee::{
    nod_materialization,
    nod_mine::{self, MineEncryptedNodRequestV2},
    protocol::EnclaveResponse,
};

pub(super) fn prepare_state(
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
    request: &PrepareStateNodsRequest,
) -> EnclaveResponse {
    response((|| -> EnclaveResult<EnclaveResponse> {
        let key = offer
            .get()
            .ok_or(TransportDispatchError::NoResidentNetworkKey)?;
        if request.authority.source.chain_id != resident_chain_id(chain)? {
            return Err(TransportDispatchError::NodMaterializationChainMismatch.into());
        }
        let inputs = nod_materialization::hash(b"outbe/nod/state-prepare-inputs", request)
            .map_err(|_| TransportDispatchError::InvalidNodPrepareInputs)?;
        if request.sources.is_empty() {
            let lookup = match crate::nod_state_materialization::request_sources(request) {
                Ok(lookup) => lookup,
                Err(error) => return materialization_failure(keys, inputs, error),
            };
            let preimage = nod_materialization::attestation(
                b"outbe/nod/state-source-request",
                inputs,
                &lookup,
            )
            .map_err(|_| TransportDispatchError::InvalidNodPrepareResult)?;
            return Ok(EnclaveResponse::TributeSourcesRequested {
                lookup,
                inputs_canonical_hash: inputs,
                attestation_tag: keys.sign_attestation(&preimage).to_vec(),
            });
        }
        let carrier = match crate::nod_state_materialization::prepare(key.secret(), request) {
            Ok(carrier) => carrier,
            Err(error) => return materialization_failure(keys, inputs, error),
        };
        let carrier = match carrier.encode_canonical(&poc_schema_limits()) {
            Ok(carrier) => carrier,
            Err(outbe_ocomp_protocol::error::ProtocolError::CapacityExceeded {
                what,
                limit,
                actual,
            }) => {
                return materialization_capacity(
                    keys,
                    inputs,
                    format!("{what}: {actual} > {limit}"),
                );
            }
            Err(_) => return Err(TransportDispatchError::InvalidProtectedNodCarrier.into()),
        };
        let preimage =
            nod_materialization::attestation(b"outbe/nod/state-prepare-result", inputs, &carrier)
                .map_err(|_| TransportDispatchError::InvalidNodPrepareResult)?;
        Ok(EnclaveResponse::EncryptedNodsPreparedV2 {
            carrier,
            inputs_canonical_hash: inputs,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}

pub(super) fn open_state(
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
    authority: &StateNodAuthority,
    carrier: &[u8],
) -> EnclaveResponse {
    response((|| -> EnclaveResult<EnclaveResponse> {
        let key = offer
            .get()
            .ok_or(TransportDispatchError::NoResidentNetworkKey)?;
        if authority.source.chain_id != resident_chain_id(chain)? {
            return Err(TransportDispatchError::NodMaterializationChainMismatch.into());
        }
        let inputs =
            nod_materialization::hash(b"outbe/nod/state-open-inputs", &(authority, carrier))
                .map_err(|_| TransportDispatchError::InvalidNodOpenInputs)?;
        let decoded =
            match ProtectedNodMaterializationV2::decode_canonical(carrier, &poc_schema_limits()) {
                Ok(carrier) => carrier,
                Err(error) => {
                    return materialization_failure(
                        keys,
                        inputs,
                        TeeError::tribute_offer_reject(error),
                    )
                }
            };
        let nods = match crate::nod_state_materialization::open(key.secret(), authority, &decoded) {
            Ok(nods) => nods,
            Err(error) => return materialization_failure(keys, inputs, error),
        };
        let preimage =
            nod_materialization::attestation(b"outbe/nod/state-open-result", inputs, &nods)
                .map_err(|_| TransportDispatchError::InvalidNodOpenResult)?;
        Ok(EnclaveResponse::EncryptedNodsOpenedV2 {
            nods,
            inputs_canonical_hash: inputs,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}

pub(super) fn mine(
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
    request: &MineEncryptedNodRequestV2,
) -> EnclaveResponse {
    response((|| -> EnclaveResult<EnclaveResponse> {
        let key = offer
            .get()
            .ok_or(TransportDispatchError::NoResidentNetworkKey)?;
        if request.nod.terms.chain_id != resident_chain_id(chain)? {
            return Err(TransportDispatchError::NodMineChainMismatch.into());
        }
        let gratis = crate::gratis::derive_gratis_state_key(key.group_sig(), chain, 0)
            .map_err(|_| TransportDispatchError::GratisKeyUnavailable)?;
        let fidelity = crate::fidelity::derive_fidelity_state_key(key.group_sig(), chain, 0)
            .map_err(|_| TransportDispatchError::FidelityKeyUnavailable)?;
        let mut result = match crate::nod_mine::apply(key.secret(), &gratis, &fidelity, request) {
            Ok(result) => result,
            Err(TeeError::TributeOfferReject(reason)) => {
                let inputs = nod_mine::inputs_hash(request)
                    .map_err(|_| TransportDispatchError::InvalidNodMintInputs)?;
                let preimage = nod_materialization::attestation(
                    b"outbe/nod/mint-rejected/v2",
                    inputs,
                    &reason,
                )
                .map_err(|_| TransportDispatchError::InvalidNodRejectionResult)?;
                return Ok(EnclaveResponse::EncryptedNodMintRejectedV2 {
                    reason,
                    inputs_canonical_hash: inputs,
                    attestation_tag: keys.sign_attestation(&preimage).to_vec(),
                });
            }
            Err(_) => return Err(TransportDispatchError::EncryptedNodMintFailed.into()),
        };
        let preimage = nod_mine::result_preimage(&result)
            .map_err(|_| TransportDispatchError::InvalidNodMineResult)?;
        result.attestation_tag = keys.sign_attestation(&preimage).to_vec();
        Ok(EnclaveResponse::EncryptedNodMinedV2 {
            result: Box::new(result),
        })
    })())
}
pub(super) fn read_amount(
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
    nod: &EncryptedNodV2,
) -> EnclaveResponse {
    let Some(key) = offer.get() else {
        return EnclaveResponse::NotReady {
            message: "no resident network key".into(),
        };
    };
    response((|| -> EnclaveResult<EnclaveResponse> {
        if nod.terms.chain_id != resident_chain_id(chain)? {
            return Err(TransportDispatchError::NodReadChainMismatch.into());
        }
        let amount = crate::nod_encryption::decrypt_nod(key.secret(), nod)
            .map_err(|_| TransportDispatchError::InvalidEncryptedNod)?;
        let inputs = nod_mine::nod_read_hash(nod)
            .map_err(|_| TransportDispatchError::InvalidNodReadInputs)?;
        let preimage = nod_mine::nod_read_preimage(inputs, amount);
        Ok(EnclaveResponse::NodAmountReadV2 {
            amount,
            inputs_canonical_hash: inputs,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}

#[cfg(feature = "e2e-test")]
pub(super) struct NodFixtureInput {
    pub terms: outbe_primitives::nod_encryption::NodTermsV2,
    pub creator_public: [u8; 32],
    pub amount: alloy_primitives::U256,
}
#[cfg(feature = "e2e-test")]
pub(super) fn create_for_test(
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
    fixture: NodFixtureInput,
) -> EnclaveResponse {
    let NodFixtureInput {
        terms,
        creator_public,
        amount,
    } = fixture;
    response((|| -> EnclaveResult<EnclaveResponse> {
        let key = offer
            .get()
            .ok_or(TransportDispatchError::NoResidentNetworkKey)?;
        if terms.chain_id != resident_chain_id(chain)? {
            return Err(TransportDispatchError::NodFixtureChainMismatch.into());
        }
        let inputs = nod_materialization::hash(
            b"outbe/nod/test-create-inputs/v2",
            &(&terms, &creator_public, amount),
        )
        .map_err(|_| TransportDispatchError::InvalidNodFixtureInputs)?;
        let nod = crate::nod_encryption::encrypt_nod(key.secret(), &creator_public, terms, amount)
            .map_err(|_| TransportDispatchError::InvalidNodFixture)?;
        let preimage =
            nod_materialization::attestation(b"outbe/nod/test-create-result/v2", inputs, &nod)
                .map_err(|_| TransportDispatchError::InvalidNodFixtureResult)?;
        Ok(EnclaveResponse::NodCreatedForTestV2 {
            nod,
            inputs_canonical_hash: inputs,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}

fn materialization_capacity(
    keys: &EnclaveKeys,
    inputs: B256,
    reason: String,
) -> EnclaveResult<EnclaveResponse> {
    let preimage =
        nod_materialization::attestation(b"outbe/nod/materialization-capacity/v2", inputs, &reason)
            .map_err(|_| TransportDispatchError::InvalidNodCapacityResult)?;
    Ok(EnclaveResponse::EncryptedNodsCapacityExceededV2 {
        reason,
        inputs_canonical_hash: inputs,
        attestation_tag: keys.sign_attestation(&preimage).to_vec(),
    })
}

fn materialization_failure(
    keys: &EnclaveKeys,
    inputs: B256,
    error: TeeError,
) -> EnclaveResult<EnclaveResponse> {
    let reason = match error {
        TeeError::TributeOfferReject(reason) => reason,
        TeeError::DecryptFailed => "invalid protected NOD witness or source ciphertext".to_owned(),
        _ => return Err(TransportDispatchError::ProtectedNodMaterializationFailed.into()),
    };
    let preimage =
        nod_materialization::attestation(b"outbe/nod/materialization-rejected/v2", inputs, &reason)
            .map_err(|_| TransportDispatchError::InvalidNodRejectionResult)?;
    Ok(EnclaveResponse::EncryptedNodsRejectedV2 {
        reason,
        inputs_canonical_hash: inputs,
        attestation_tag: keys.sign_attestation(&preimage).to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::DerivedTributeOfferKey;
    use std::sync::{Arc, OnceLock};

    #[test]
    fn lysis_dispatch_requires_resident_chain_and_attests_ready_nods() {
        use outbe_tee::protocol::EnclaveRequest;
        let request = crate::lysis::tests::fixture();
        let keys = EnclaveKeys::new([0x43; 32], None).unwrap();
        let offer: SharedTributeOfferKey = Arc::new(OnceLock::new());
        let chain = B256::from(alloy_primitives::U256::ONE);
        assert!(matches!(
            execute_lysis(&keys, &offer, chain, &request),
            EnclaveResponse::Error { .. }
        ));
        assert!(offer
            .set(DerivedTributeOfferKey::for_test(
                [0x5a; 32],
                vec![9; 48],
                0,
                0
            ))
            .is_ok());
        assert!(matches!(
            execute_lysis(&keys, &offer, B256::ZERO, &request),
            EnclaveResponse::Error { .. }
        ));
        let mut dkg = crate::dkg::DkgSessionStore::new();
        let response = crate::transport::dispatch(
            EnclaveRequest::ExecuteLysis {
                request: Box::new(request.clone()),
            },
            &keys,
            &mut dkg,
            &offer,
            chain,
        );
        let EnclaveResponse::LysisExecuted {
            result,
            inputs_canonical_hash,
            attestation_tag,
        } = response
        else {
            panic!("expected ready encrypted Nods");
        };
        assert_eq!(
            inputs_canonical_hash,
            outbe_tee::lysis::inputs_hash(&request).unwrap()
        );
        assert_eq!(result.nods.len(), request.tributes.len());
        outbe_tee::tribute_v2::verify_attestation(
            &keys.attestation_pub(),
            &outbe_tee::lysis::result_preimage(inputs_canonical_hash, &result).unwrap(),
            &attestation_tag,
        )
        .unwrap();
        let mut different = request.clone();
        different.timestamp += 1;
        assert!(outbe_tee::tribute_v2::verify_attestation(
            &keys.attestation_pub(),
            &outbe_tee::lysis::result_preimage(
                outbe_tee::lysis::inputs_hash(&different).unwrap(),
                &result
            )
            .unwrap(),
            &attestation_tag
        )
        .is_err());
    }

    #[test]
    fn lysis_dispatch_attests_only_authenticated_calculation_rejection() {
        let keys = EnclaveKeys::new([0x43; 32], None).unwrap();
        let offer: SharedTributeOfferKey = Arc::new(OnceLock::new());
        assert!(offer
            .set(DerivedTributeOfferKey::for_test(
                [0x5a; 32],
                vec![9; 48],
                0,
                0
            ))
            .is_ok());
        let chain = B256::from(alloy_primitives::U256::ONE);
        let mut request = crate::lysis::tests::fixture();
        request.lysis_limit_minor = alloy_primitives::U256::ONE;
        let EnclaveResponse::LysisRejected {
            inputs_canonical_hash,
            attestation_tag,
        } = execute_lysis(&keys, &offer, chain, &request)
        else {
            panic!("expected signed calculation rejection");
        };
        assert_eq!(
            inputs_canonical_hash,
            outbe_tee::lysis::inputs_hash(&request).unwrap()
        );
        outbe_tee::tribute_v2::verify_attestation(
            &keys.attestation_pub(),
            &outbe_tee::lysis::rejection_preimage(inputs_canonical_hash),
            &attestation_tag,
        )
        .unwrap();
        assert!(outbe_tee::tribute_v2::verify_attestation(
            &keys.attestation_pub(),
            &outbe_tee::lysis::rejection_preimage(B256::ZERO),
            &attestation_tag,
        )
        .is_err());
        request.tributes.last_mut().unwrap().encrypted_amounts[20] ^= 1;
        assert!(matches!(
            execute_lysis(&keys, &offer, chain, &request),
            EnclaveResponse::Error { .. }
        ));
    }

    #[test]
    fn capacity_result_is_signed_over_exact_inputs_and_reason() {
        let keys = EnclaveKeys::new([0x43; 32], None).unwrap();
        let inputs = B256::repeat_byte(7);
        let response =
            materialization_capacity(&keys, inputs, "carrier: 2097153 > 2097152".into()).unwrap();
        let EnclaveResponse::EncryptedNodsCapacityExceededV2 {
            reason,
            inputs_canonical_hash,
            attestation_tag,
        } = response
        else {
            panic!("expected capacity result")
        };
        assert_eq!(inputs_canonical_hash, inputs);
        let preimage = nod_materialization::attestation(
            b"outbe/nod/materialization-capacity/v2",
            inputs,
            &reason,
        )
        .unwrap();
        outbe_tee::tribute_v2::verify_attestation(
            &keys.attestation_pub(),
            &preimage,
            &attestation_tag,
        )
        .unwrap();
        let substituted = nod_materialization::attestation(
            b"outbe/nod/materialization-capacity/v2",
            B256::ZERO,
            &reason,
        )
        .unwrap();
        assert!(outbe_tee::tribute_v2::verify_attestation(
            &keys.attestation_pub(),
            &substituted,
            &attestation_tag
        )
        .is_err());
        assert!(materialization_failure(&keys, inputs, TeeError::EncryptFailed).is_err());
    }

    #[test]
    fn malformed_proof_is_signed_rejection_but_missing_key_is_local_fault() {
        let keys = EnclaveKeys::new([0x43; 32], None).unwrap();
        let offer: SharedTributeOfferKey = Arc::new(OnceLock::new());
        let authority = StateNodAuthority {
            source: outbe_tee::tribute_source::TributeSourceSubject {
                checkpoint: outbe_tee::tribute_source::TributeSourceCheckpoint {
                    block_number: 1,
                    block_hash: B256::repeat_byte(2),
                    state_root: B256::repeat_byte(3),
                },
                chain_id: 1,
                worldwide_day: outbe_primitives::time::WorldwideDay::new(20261016),
                tribute_count: 1,
            },
            head: vec![],
            subtree_height: 3,
            source_bindings: vec![],
            nod_gratis_load_max: alloy_primitives::U256::ONE,
        };
        let chain = B256::from(alloy_primitives::U256::ONE);
        assert!(matches!(
            open_state(&keys, &offer, chain, &authority, b"invalid"),
            EnclaveResponse::Error { .. }
        ));
        assert!(offer
            .set(DerivedTributeOfferKey::for_test([7; 32], vec![9; 48], 0, 0))
            .is_ok());
        let expected = nod_materialization::hash(
            b"outbe/nod/state-open-inputs",
            &(&authority, b"invalid".as_slice()),
        )
        .unwrap();
        match open_state(&keys, &offer, chain, &authority, b"invalid") {
            EnclaveResponse::EncryptedNodsRejectedV2 {
                reason,
                inputs_canonical_hash,
                attestation_tag,
            } => {
                assert_eq!(inputs_canonical_hash, expected);
                let preimage = nod_materialization::attestation(
                    b"outbe/nod/materialization-rejected/v2",
                    expected,
                    &reason,
                )
                .unwrap();
                outbe_tee::tribute_v2::verify_attestation(
                    &keys.attestation_pub(),
                    &preimage,
                    &attestation_tag,
                )
                .unwrap();
                let wrong = nod_materialization::attestation(
                    b"outbe/nod/materialization-rejected/v2",
                    B256::ZERO,
                    &reason,
                )
                .unwrap();
                assert!(outbe_tee::tribute_v2::verify_attestation(
                    &keys.attestation_pub(),
                    &wrong,
                    &attestation_tag
                )
                .is_err());
            }
            response => panic!("expected signed proof rejection, received {response:?}"),
        }
    }
}
pub(super) fn execute_lysis(
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
    request: &outbe_tee::lysis::LysisRequest,
) -> EnclaveResponse {
    response((|| -> EnclaveResult<EnclaveResponse> {
        let key = offer
            .get()
            .ok_or(TransportDispatchError::NoResidentNetworkKey)?;
        if request.chain_id != resident_chain_id(chain)? {
            return Err(TransportDispatchError::NodMaterializationChainMismatch.into());
        }
        let fidelity = crate::fidelity::derive_fidelity_state_key(key.group_sig(), chain, 0)
            .map_err(|_| TransportDispatchError::FidelityKeyUnavailable)?;
        let inputs = outbe_tee::lysis::inputs_hash(request)
            .map_err(|_| TransportDispatchError::InvalidNodPrepareInputs)?;
        let result = match crate::lysis::execute(key.secret(), &fidelity, request) {
            Ok(result) => result,
            Err(TeeError::LysisCalculationRejected) => {
                return Ok(EnclaveResponse::LysisRejected {
                    inputs_canonical_hash: inputs,
                    attestation_tag: keys
                        .sign_attestation(&outbe_tee::lysis::rejection_preimage(inputs))
                        .to_vec(),
                });
            }
            Err(error) => return Err(error.into()),
        };
        let preimage = outbe_tee::lysis::result_preimage(inputs, &result)
            .map_err(|_| TransportDispatchError::InvalidNodPrepareResult)?;
        Ok(EnclaveResponse::LysisExecuted {
            result: Box::new(result),
            inputs_canonical_hash: inputs,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}
