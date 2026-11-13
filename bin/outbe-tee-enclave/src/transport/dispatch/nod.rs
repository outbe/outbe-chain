//! Network-key-only encrypted NOD commands.
use super::tribute::{resident_chain_id, response};
use crate::{
    errors::{EnclaveResult, TeeError, TransportDispatchError},
    keys::EnclaveKeys,
    transport::SharedTributeOfferKey,
};
use alloy_primitives::B256;
use outbe_primitives::{
    nod_encryption::{EncryptedNodGroupLoadV1, EncryptedNodV1, NodQuoteContextV1, NodTermsV1},
    tribute_encryption::EncryptedTributeV2,
};
use outbe_tee::{
    nod_mine::{
        self, IssueNodFromTributeRequestV1, MineEncryptedNodRequestV1, NodGroupLoadOpRequestV1,
    },
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
        let nod_key = crate::nod_encryption::derive_nod_state_key(key.group_sig(), chain, 0)
            .map_err(|_| TransportDispatchError::NodKeyUnavailable)?;
        let gratis = crate::gratis::derive_gratis_state_key(key.group_sig(), chain, 0)
            .map_err(|_| TransportDispatchError::GratisKeyUnavailable)?;
        let fidelity = crate::fidelity::derive_fidelity_state_key(key.group_sig(), chain, 0)
            .map_err(|_| TransportDispatchError::FidelityKeyUnavailable)?;
        let mut result = match crate::nod_mine::apply(&nod_key, &gratis, &fidelity, request) {
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
        let nod_key = crate::nod_encryption::derive_nod_state_key(key.group_sig(), chain, 0)
            .map_err(|_| TransportDispatchError::NodKeyUnavailable)?;
        let amount = crate::nod_encryption::decrypt_nod(&nod_key, nod)
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
        let tribute_key =
            crate::tribute_encryption::derive_tribute_state_key(key.group_sig(), chain, 0)
                .map_err(|_| TransportDispatchError::TributeKeyUnavailable)?;
        let nod_key = crate::nod_encryption::derive_nod_state_key(key.group_sig(), chain, 0)
            .map_err(|_| TransportDispatchError::NodKeyUnavailable)?;
        let inputs = response_auth::hash(
            b"outbe/nod/tribute-create-inputs/v1",
            &(input.tribute, &input.terms, input.amount),
        )
        .map_err(|_| TransportDispatchError::InvalidNodTributeEncryptInputs)?;
        let nod = crate::nod_encryption::encrypt_nod_for_tribute(
            &tribute_key,
            &nod_key,
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

pub(super) fn issue_from_tribute(
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
    request: &IssueNodFromTributeRequestV1,
) -> EnclaveResponse {
    response((|| -> EnclaveResult<EnclaveResponse> {
        let key = offer
            .get()
            .ok_or(TransportDispatchError::NoResidentNetworkKey)?;
        if request.terms.chain_id != resident_chain_id(chain)? {
            return Err(TransportDispatchError::NodTributeEncryptChainMismatch.into());
        }
        let tribute_key =
            crate::tribute_encryption::derive_tribute_state_key(key.group_sig(), chain, 0)
                .map_err(|_| TransportDispatchError::TributeKeyUnavailable)?;
        let nod_key = crate::nod_encryption::derive_nod_state_key(key.group_sig(), chain, 0)
            .map_err(|_| TransportDispatchError::NodKeyUnavailable)?;
        let inputs = response_auth::hash(b"outbe/nod/issue-from-tribute-inputs/v1", request)
            .map_err(|_| TransportDispatchError::InvalidNodTributeEncryptInputs)?;
        let outcome = crate::nod_encryption::issue_nod_from_tribute(
            key.secret(),
            &tribute_key,
            &nod_key,
            request,
        )
        .map_err(|_| TransportDispatchError::InvalidNodTributeEncrypt)?;
        let preimage =
            response_auth::attestation(b"outbe/nod/issue-from-tribute-result/v1", inputs, &outcome)
                .map_err(|_| TransportDispatchError::InvalidNodTributeEncryptResult)?;
        Ok(EnclaveResponse::NodIssuedFromTributeV1 {
            outcome,
            inputs_canonical_hash: inputs,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}

pub(super) fn apply_group_load_op(
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
    request: &NodGroupLoadOpRequestV1,
) -> EnclaveResponse {
    response((|| -> EnclaveResult<EnclaveResponse> {
        let key = offer
            .get()
            .ok_or(TransportDispatchError::NoResidentNetworkKey)?;
        if request.chain_id != resident_chain_id(chain)? {
            return Err(TransportDispatchError::NodReadChainMismatch.into());
        }
        let nod_key = crate::nod_encryption::derive_nod_state_key(key.group_sig(), chain, 0)
            .map_err(|_| TransportDispatchError::NodKeyUnavailable)?;
        let inputs = nod_mine::nod_group_load_inputs_hash(request)
            .map_err(|_| TransportDispatchError::InvalidNodReadInputs)?;
        let record = crate::nod_encryption::apply_nod_group_load_operation(&nod_key, request)
            .map_err(|_| TransportDispatchError::InvalidEncryptedNod)?;
        let preimage = nod_mine::nod_group_load_attestation_preimage(inputs, &record)
            .map_err(|_| TransportDispatchError::InvalidNodMineResult)?;
        Ok(EnclaveResponse::NodGroupLoadOpAppliedV1 {
            record,
            inputs_canonical_hash: inputs,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}

pub(super) fn read_group_load(
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
    record: &EncryptedNodGroupLoadV1,
) -> EnclaveResponse {
    let Some(key) = offer.get() else {
        return EnclaveResponse::NotReady {
            message: "no resident network key".into(),
        };
    };
    response((|| -> EnclaveResult<EnclaveResponse> {
        if record.chain_id != resident_chain_id(chain)? {
            return Err(TransportDispatchError::NodReadChainMismatch.into());
        }
        let nod_key = crate::nod_encryption::derive_nod_state_key(key.group_sig(), chain, 0)
            .map_err(|_| TransportDispatchError::NodKeyUnavailable)?;
        let amount = crate::nod_encryption::read_nod_group_load(&nod_key, record)
            .map_err(|_| TransportDispatchError::InvalidEncryptedNod)?;
        let inputs = nod_mine::nod_group_load_read_hash(record)
            .map_err(|_| TransportDispatchError::InvalidNodReadInputs)?;
        let preimage = nod_mine::nod_group_load_read_preimage(inputs, amount);
        Ok(EnclaveResponse::NodGroupLoadReadV1 {
            amount,
            inputs_canonical_hash: inputs,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}

pub(super) fn encrypt_quote(
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
    (nod, context, payment_minor): (&EncryptedNodV1, &NodQuoteContextV1, alloy_primitives::U256),
) -> EnclaveResponse {
    response((|| -> EnclaveResult<EnclaveResponse> {
        let key = offer
            .get()
            .ok_or(TransportDispatchError::NoResidentNetworkKey)?;
        if context.chain_id != resident_chain_id(chain)? {
            return Err(TransportDispatchError::NodReadChainMismatch.into());
        }
        let nod_key = crate::nod_encryption::derive_nod_state_key(key.group_sig(), chain, 0)
            .map_err(|_| TransportDispatchError::NodKeyUnavailable)?;
        let inputs = response_auth::hash(
            b"outbe/nod/quote-encrypt-inputs/v1",
            &(nod, context, payment_minor),
        )
        .map_err(|_| TransportDispatchError::InvalidNodReadInputs)?;
        let encrypted_payment =
            crate::nod_encryption::encrypt_nod_quote(&nod_key, nod, context, payment_minor)
                .map_err(|_| TransportDispatchError::InvalidEncryptedNod)?;
        let preimage = response_auth::attestation(
            b"outbe/nod/quote-encrypt-result/v1",
            inputs,
            &encrypted_payment,
        )
        .map_err(|_| TransportDispatchError::InvalidNodMineResult)?;
        Ok(EnclaveResponse::NodQuoteEncryptedV1 {
            encrypted_payment,
            inputs_canonical_hash: inputs,
            attestation_tag: keys.sign_attestation(&preimage).to_vec(),
        })
    })())
}
