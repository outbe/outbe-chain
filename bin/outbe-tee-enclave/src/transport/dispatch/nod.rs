//! Network-key-only encrypted NOD commands.
use super::tribute::{resident_chain_id, response};
use crate::{errors::TeeError, keys::EnclaveKeys, transport::SharedTributeOfferKey};
use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    nod_materialization::ProtectedNodMaterializationV2, profile::poc_schema_limits,
};
use outbe_primitives::nod_encryption::EncryptedNodV2;
use outbe_tee::{
    nod_materialization::{self, NodMaterializationAuthorityV2, PrepareEncryptedNodsRequestV2},
    nod_mine::{self, MineEncryptedNodRequestV2},
    protocol::EnclaveResponse,
};
pub(super) fn prepare(
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
    request: &PrepareEncryptedNodsRequestV2,
) -> EnclaveResponse {
    response((|| {
        let key = offer.get().ok_or("no resident network key")?;
        if request.authority.chain_id != resident_chain_id(chain)? {
            return Err("NOD materialization chain mismatch");
        }
        let inputs = nod_materialization::hash(b"outbe/nod/prepare-inputs/v2", request)
            .map_err(|_| "invalid NOD prepare inputs")?;
        let carrier = match crate::nod_materialization::prepare(key.secret(), request) {
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
            Err(_) => return Err("invalid protected NOD carrier"),
        };

        let preimage =
            nod_materialization::attestation(b"outbe/nod/prepare-result/v2", inputs, &carrier)
                .map_err(|_| "invalid NOD prepare result")?;
        Ok(EnclaveResponse::EncryptedNodsPreparedV2 {
            carrier,
            inputs_canonical_hash: inputs,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}
pub(super) fn open(
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
    authority: &NodMaterializationAuthorityV2,
    carrier: &[u8],
) -> EnclaveResponse {
    response((|| {
        let key = offer.get().ok_or("no resident network key")?;
        if authority.chain_id != resident_chain_id(chain)? {
            return Err("NOD materialization chain mismatch");
        }
        let inputs = nod_materialization::hash(b"outbe/nod/open-inputs/v2", &(authority, carrier))
            .map_err(|_| "invalid NOD open inputs")?;
        let decoded =
            match ProtectedNodMaterializationV2::decode_canonical(carrier, &poc_schema_limits()) {
                Ok(decoded) => decoded,
                Err(error) => {
                    return materialization_failure(
                        keys,
                        inputs,
                        TeeError::TributeOfferReject(error.to_string()),
                    );
                }
            };
        let nods = match crate::nod_materialization::open(key.secret(), authority, &decoded) {
            Ok(nods) => nods,
            Err(error) => return materialization_failure(keys, inputs, error),
        };

        let preimage = nod_materialization::attestation(b"outbe/nod/open-result/v2", inputs, &nods)
            .map_err(|_| "invalid NOD open result")?;
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
    response((|| {
        let key = offer.get().ok_or("no resident network key")?;
        if request.nod.terms.chain_id != resident_chain_id(chain)? {
            return Err("NOD mine chain mismatch");
        }
        let gratis = crate::gratis::derive_gratis_state_key(key.group_sig(), chain, 0)
            .map_err(|_| "Gratis key unavailable")?;
        let fidelity = crate::fidelity::derive_fidelity_state_key(key.group_sig(), chain, 0)
            .map_err(|_| "Fidelity key unavailable")?;
        let mut result = match crate::nod_mine::apply(key.secret(), &gratis, &fidelity, request) {
            Ok(result) => result,
            Err(TeeError::TributeOfferReject(reason)) => {
                let inputs =
                    nod_mine::inputs_hash(request).map_err(|_| "invalid NOD mint inputs")?;
                let preimage = nod_materialization::attestation(
                    b"outbe/nod/mint-rejected/v2",
                    inputs,
                    &reason,
                )
                .map_err(|_| "invalid NOD rejection result")?;
                return Ok(EnclaveResponse::EncryptedNodMintRejectedV2 {
                    reason,
                    inputs_canonical_hash: inputs,
                    attestation_tag: keys.sign_attestation(&preimage).to_vec(),
                });
            }
            Err(_) => return Err("encrypted NOD mint failed"),
        };
        let preimage = nod_mine::result_preimage(&result).map_err(|_| "invalid NOD mine result")?;
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
    response((|| {
        if nod.terms.chain_id != resident_chain_id(chain)? {
            return Err("NOD read chain mismatch");
        }
        let amount = crate::nod_encryption::decrypt_nod(key.secret(), nod)
            .map_err(|_| "invalid encrypted NOD")?;
        let inputs = nod_mine::nod_read_hash(nod).map_err(|_| "invalid NOD read inputs")?;
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
    response((|| {
        let key = offer.get().ok_or("no resident network key")?;
        if terms.chain_id != resident_chain_id(chain)? {
            return Err("NOD fixture chain mismatch");
        }
        let inputs = nod_materialization::hash(
            b"outbe/nod/test-create-inputs/v2",
            &(&terms, &creator_public, amount),
        )
        .map_err(|_| "invalid NOD fixture inputs")?;
        let nod = crate::nod_encryption::encrypt_nod(key.secret(), &creator_public, terms, amount)
            .map_err(|_| "invalid NOD fixture")?;
        let preimage =
            nod_materialization::attestation(b"outbe/nod/test-create-result/v2", inputs, &nod)
                .map_err(|_| "invalid NOD fixture result")?;
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
) -> Result<EnclaveResponse, &'static str> {
    let preimage =
        nod_materialization::attestation(b"outbe/nod/materialization-capacity/v2", inputs, &reason)
            .map_err(|_| "invalid NOD capacity result")?;
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
) -> Result<EnclaveResponse, &'static str> {
    let reason = match error {
        TeeError::TributeOfferReject(reason) => reason,
        TeeError::DecryptFailed => "invalid protected NOD witness or source ciphertext".to_owned(),
        _ => return Err("protected NOD materialization failed"),
    };
    let preimage =
        nod_materialization::attestation(b"outbe/nod/materialization-rejected/v2", inputs, &reason)
            .map_err(|_| "invalid NOD rejection result")?;
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
        let authority = NodMaterializationAuthorityV2 {
            chain_id: 1,
            head: vec![],
            subtree_height: 3,
            sealed_tribute_root: B256::repeat_byte(4),
        };
        let chain = B256::from(alloy_primitives::U256::ONE);
        assert!(matches!(
            open(&keys, &offer, chain, &authority, b"invalid"),
            EnclaveResponse::Error { .. }
        ));
        assert!(offer
            .set(DerivedTributeOfferKey::for_test([7; 32], vec![9; 48], 0, 0))
            .is_ok());
        let expected = nod_materialization::hash(
            b"outbe/nod/open-inputs/v2",
            &(&authority, b"invalid".as_slice()),
        )
        .unwrap();
        match open(&keys, &offer, chain, &authority, b"invalid") {
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
