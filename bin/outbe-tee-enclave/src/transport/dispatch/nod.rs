//! Network-key-only encrypted NOD commands.
use super::tribute::{resident_chain_id, response};
use crate::{
    errors::{EnclaveResult, TeeError, TransportDispatchError},
    keys::EnclaveKeys,
    transport::SharedTributeOfferKey,
};
use alloy_primitives::B256;
use outbe_primitives::{
    nod_encryption::{EncryptedNodV1, NodTermsV1},
    tribute_encryption::EncryptedTributeV2,
};
use outbe_tee::{
    nod_mine::{self, MineEncryptedNodRequestV1},
    protocol::EnclaveResponse,
    response_auth,
};

pub(super) fn mine(
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
    request: &MineEncryptedNodRequestV1,
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
                let preimage =
                    response_auth::attestation(b"outbe/nod/mint-rejected/v1", inputs, &reason)
                        .map_err(|_| TransportDispatchError::InvalidNodRejectionResult)?;
                return Ok(EnclaveResponse::EncryptedNodMintRejectedV1 {
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
        Ok(EnclaveResponse::EncryptedNodMinedV1 {
            result: Box::new(result),
        })
    })())
}
pub(super) fn read_amount(
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
    nod: &EncryptedNodV1,
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
        Ok(EnclaveResponse::NodAmountReadV1 {
            amount,
            inputs_canonical_hash: inputs,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}

pub(super) struct NodTributeInput<'a> {
    pub tribute: &'a EncryptedTributeV2,
    pub terms: NodTermsV1,
    pub amount: alloy_primitives::U256,
}

pub(super) fn encrypt_for_tribute(
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
    input: NodTributeInput<'_>,
) -> EnclaveResponse {
    response((|| -> EnclaveResult<EnclaveResponse> {
        let key = offer
            .get()
            .ok_or(TransportDispatchError::NoResidentNetworkKey)?;
        if input.terms.chain_id != resident_chain_id(chain)? {
            return Err(TransportDispatchError::NodTributeEncryptChainMismatch.into());
        }
        let inputs = response_auth::hash(
            b"outbe/nod/tribute-create-inputs/v1",
            &(input.tribute, &input.terms, input.amount),
        )
        .map_err(|_| TransportDispatchError::InvalidNodTributeEncryptInputs)?;
        let nod = crate::nod_encryption::encrypt_nod_for_tribute(
            key.secret(),
            input.tribute,
            input.terms,
            input.amount,
        )
        .map_err(|_| TransportDispatchError::InvalidNodTributeEncrypt)?;
        let preimage =
            response_auth::attestation(b"outbe/nod/tribute-create-result/v1", inputs, &nod)
                .map_err(|_| TransportDispatchError::InvalidNodTributeEncryptResult)?;
        Ok(EnclaveResponse::NodEncryptedForTributeV1 {
            nod,
            inputs_canonical_hash: inputs,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}

#[cfg(feature = "e2e-test")]
pub(super) struct NodFixtureInput {
    pub terms: outbe_primitives::nod_encryption::NodTermsV1,
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
        let inputs = response_auth::hash(
            b"outbe/nod/test-create-inputs/v1",
            &(&terms, &creator_public, amount),
        )
        .map_err(|_| TransportDispatchError::InvalidNodFixtureInputs)?;
        let nod = crate::nod_encryption::encrypt_nod(key.secret(), &creator_public, terms, amount)
            .map_err(|_| TransportDispatchError::InvalidNodFixture)?;
        let preimage = response_auth::attestation(b"outbe/nod/test-create-result/v1", inputs, &nod)
            .map_err(|_| TransportDispatchError::InvalidNodFixtureResult)?;
        Ok(EnclaveResponse::NodCreatedForTestV1 {
            nod,
            inputs_canonical_hash: inputs,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
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
            return Err(TransportDispatchError::LysisChainMismatch.into());
        }
        let fidelity = crate::fidelity::derive_fidelity_state_key(key.group_sig(), chain, 0)
            .map_err(|_| TransportDispatchError::FidelityKeyUnavailable)?;
        let inputs = outbe_tee::lysis::inputs_hash(request)
            .map_err(|_| TransportDispatchError::InvalidLysisInputs)?;
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
            .map_err(|_| TransportDispatchError::InvalidLysisResult)?;
        Ok(EnclaveResponse::LysisExecuted {
            result: Box::new(result),
            inputs_canonical_hash: inputs,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
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
}
