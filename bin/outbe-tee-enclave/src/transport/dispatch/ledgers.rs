//! Handle ledgers requests after session admission.

use super::requests::RequestContext;
use crate::transport::*;

pub(super) fn dispatch(req: EnclaveRequest, context: RequestContext<'_>) -> EnclaveResponse {
    match req {
        EnclaveRequest::ApplyGratisOp { request } => apply_gratis(context, request),
        EnclaveRequest::ApplyFidelityCohortOp { request } => apply_fidelity(context, request),
        EnclaveRequest::SnapshotFidelityLeagues { request } => snapshot_fidelity(context, request),
        EnclaveRequest::QueryFidelityIndex { request } => query_fidelity(context, request),
        EnclaveRequest::ApplyPromisOp { request } => apply_promis(context, request),
        EnclaveRequest::DeriveAccountKeys {
            ledger,
            account,
            requester_ephemeral_pubkey,
            owner_sig,
        } => derive_account_keys(
            context,
            AccountKeyRequest {
                ledger,
                account,
                requester_ephemeral_pubkey,
                owner_sig,
            },
        ),
        _ => unreachable!("request family is checked by the exhaustive dispatcher"),
    }
}

struct AccountKeyRequest {
    ledger: outbe_tee::protocol::Ledger,
    account: alloy_primitives::Address,
    requester_ephemeral_pubkey: [u8; 32],
    owner_sig: Vec<u8>,
}

fn apply_gratis(
    context: RequestContext<'_>,
    request: Box<outbe_tee::protocol::GratisOpRequest>,
) -> EnclaveResponse {
    let RequestContext {
        keys,
        offer_key,
        chain_id,
        ..
    } = context;
    // Derive the resident Gratis state key from the same DKG group
    // signature as the offer key. The key is identical on every enclave, so
    // the re-encrypted state is byte-identical (consensus determinism).
    let Some(derived) = offer_key.get() else {
        return EnclaveResponse::Error {
            message: "ApplyGratisOp: no resident group key (DKG not complete)".to_string(),
        };
    };
    let state_key = match crate::gratis::derive_gratis_state_key(derived.group_sig(), chain_id, 0) {
        Ok(k) => k,
        Err(e) => {
            return EnclaveResponse::Error {
                message: e.to_string(),
            };
        }
    };
    let mut result = crate::gratis::apply_op(&state_key, &request);
    // Co-located Fidelity cohort section. This arm applies it atomically with
    // the Gratis op under the section's own independent key domain. A failing section
    // rejects the WHOLE op, and the host writes neither ledger.
    if let (outbe_tee::protocol::GratisOpStatus::Applied, Some(section)) =
        (&result.status, &request.fidelity)
    {
        let fidelity_outcome =
            crate::fidelity::derive_fidelity_state_key(derived.group_sig(), chain_id, 0).and_then(
                |fidelity_key| {
                    crate::fidelity::apply_cohort_section(
                        &fidelity_key,
                        request.account,
                        request.amount,
                        section,
                    )
                },
            );
        match fidelity_outcome {
            Ok(outcome) => result.fidelity = Some(outcome),
            Err(e) => {
                result = crate::gratis::rejected_result(
                    format!("fidelity section failed: {e}"),
                    result.inputs_canonical_hash,
                );
            }
        }
    }
    // Sign (inputs_canonical_hash || result) with the attestation key so the
    // host can prove the result came from this attested enclave.
    let preimage =
        outbe_tee::protocol::gratis_op_attestation_preimage(result.inputs_canonical_hash, &result);
    result.attestation_tag = keys.sign_attestation(&preimage).to_vec();
    EnclaveResponse::GratisOpApplied {
        result: Box::new(result),
    }
}

fn apply_fidelity(
    context: RequestContext<'_>,
    request: Box<outbe_tee::protocol::FidelityCohortRequest>,
) -> EnclaveResponse {
    fidelity_request(context, "ApplyFidelityCohortOp", |key| {
        let mut result = crate::fidelity::apply_cohort_op(key, &request)?;
        let preimage = outbe_tee::protocol::fidelity_cohort_attestation_preimage(
            result.inputs_canonical_hash,
            &result,
        );
        result.attestation_tag = context.keys.sign_attestation(&preimage).to_vec();
        Ok(EnclaveResponse::FidelityCohortApplied {
            result: Box::new(result),
        })
    })
}

fn snapshot_fidelity(
    context: RequestContext<'_>,
    request: Box<outbe_tee::protocol::FidelitySnapshotRequest>,
) -> EnclaveResponse {
    fidelity_request(context, "SnapshotFidelityLeagues", |key| {
        let leagues = crate::fidelity::snapshot_leagues(key, &request)?;
        let inputs_canonical_hash = outbe_tee::protocol::fidelity_snapshot_canonical_hash(&request);
        let preimage = outbe_tee::protocol::fidelity_snapshot_attestation_preimage(
            inputs_canonical_hash,
            &leagues,
        );
        let attestation_tag = context.keys.sign_attestation(&preimage).to_vec();
        Ok(EnclaveResponse::FidelityLeaguesSnapshotted {
            leagues,
            inputs_canonical_hash,
            attestation_tag,
        })
    })
}

fn query_fidelity(
    context: RequestContext<'_>,
    request: Box<outbe_tee::protocol::FidelityQueryRequest>,
) -> EnclaveResponse {
    fidelity_request(context, "QueryFidelityIndex", |key| {
        let mut result = crate::fidelity::query_index(key, context.chain_id, &request)?;
        let preimage = outbe_tee::protocol::fidelity_query_attestation_preimage(
            result.inputs_canonical_hash,
            &result,
        );
        result.attestation_tag = context.keys.sign_attestation(&preimage).to_vec();
        Ok(EnclaveResponse::FidelityIndexQueried {
            result: Box::new(result),
        })
    })
}

fn apply_promis(
    context: RequestContext<'_>,
    request: Box<outbe_tee::protocol::PromisOpRequest>,
) -> EnclaveResponse {
    let RequestContext {
        keys,
        offer_key,
        chain_id,
        ..
    } = context;
    // Same resident-key derivation + attestation as ApplyGratisOp, over the
    // independent Promis key domain (mint/burn on an encrypted balance).
    let Some(derived) = offer_key.get() else {
        return EnclaveResponse::Error {
            message: "ApplyPromisOp: no resident group key (DKG not complete)".to_string(),
        };
    };
    let state_key = match crate::promis::derive_promis_state_key(derived.group_sig(), chain_id, 0) {
        Ok(k) => k,
        Err(e) => {
            return EnclaveResponse::Error {
                message: e.to_string(),
            };
        }
    };
    let mut result = crate::promis::apply_op(&state_key, &request);
    let preimage =
        outbe_tee::protocol::promis_op_attestation_preimage(result.inputs_canonical_hash, &result);
    result.attestation_tag = keys.sign_attestation(&preimage).to_vec();
    EnclaveResponse::PromisOpApplied {
        result: Box::new(result),
    }
}

fn derive_account_keys(context: RequestContext<'_>, request: AccountKeyRequest) -> EnclaveResponse {
    let RequestContext {
        offer_key,
        chain_id,
        ..
    } = context;
    let AccountKeyRequest {
        ledger,
        account,
        requester_ephemeral_pubkey,
        owner_sig,
    } = request;
    // OFF-CHAIN key delivery only (served over RPC, never during block
    // execution): derive the account's view + modify keys and seal them to
    // the requester's ephemeral X25519 key.
    let Some(derived) = offer_key.get() else {
        return EnclaveResponse::Error {
            message: "DeriveAccountKeys: no resident group key (DKG not complete)".to_string(),
        };
    };
    // Prove the caller controls `account` INSIDE the enclave before releasing
    // its (secret) view/modify keys. The host RPC recovers the same signature
    // as a fast reject. But a compromised host reaches this transport directly,
    // so the release decision must run in the enclave's trust domain, not the
    // host's. Same shared preimage + recover_signer the host uses (one impl, no
    // divergence).
    let Ok(sig65) = <[u8; 65]>::try_from(owner_sig.as_slice()) else {
        return EnclaveResponse::Error {
            message: "DeriveAccountKeys: owner signature must be 65 bytes".to_string(),
        };
    };
    let prehash =
        outbe_tee::protocol::eip191_hash(&outbe_tee::protocol::derive_account_keys_message(
            ledger,
            account,
            alloy_primitives::B256::from(requester_ephemeral_pubkey),
        ));
    match outbe_primitives::tee_signatures::recover_signer(&prehash, &sig65) {
        Ok(signer) if signer == account => {}
        _ => {
            return EnclaveResponse::Error {
                message: "DeriveAccountKeys: owner signature does not control account".to_string(),
            };
        }
    }
    let sealed = (|| -> crate::errors::Result<crate::crypto::EncryptedShare> {
        let domain = crate::confidential::domain_for(ledger);
        let state_key = domain.derive_state_key(derived.group_sig(), chain_id, 0)?;
        let view_key = domain.derive_view_key(&state_key, account)?;
        let modify_key = domain.derive_modify_key(&state_key, account)?;
        let mut plaintext = view_key.to_vec();
        plaintext.extend_from_slice(&modify_key);
        crate::crypto::encrypt_share(&requester_ephemeral_pubkey, &plaintext)
    })();
    match sealed {
        Ok(blob) => EnclaveResponse::AccountKeysSealed {
            account,
            sealed: blob.ciphertext,
            nonce: blob.nonce,
            enclave_ephemeral_pubkey: blob.ephemeral_pub,
        },
        Err(e) => EnclaveResponse::Error {
            message: e.to_string(),
        },
    }
}

fn fidelity_request(
    context: RequestContext<'_>,
    operation: &str,
    apply: impl FnOnce(&[u8; 32]) -> crate::errors::Result<EnclaveResponse>,
) -> EnclaveResponse {
    let Some(derived) = context.offer_key.get() else {
        return EnclaveResponse::Error {
            message: format!("{operation}: no resident group key (DKG not complete)"),
        };
    };
    into_response(
        crate::fidelity::derive_fidelity_state_key(derived.group_sig(), context.chain_id, 0)
            .and_then(|key| apply(&key)),
    )
}
