//! Ledger transitions and owner-authorized account-key export.
use super::*;
use crate::protocol::*;

pub(super) fn gratis(
    context: &InProcessTeeContext,
    request: &GratisOpRequest,
) -> Result<EnclaveResponse> {
    let key = state(context, crate::gratis::derive_gratis_state_key)?;
    let mut result = crate::gratis::apply_op(&key, request);
    if let (GratisOpStatus::Applied, Some(section)) = (&result.status, &request.fidelity) {
        let fidelity = state(context, crate::fidelity::derive_fidelity_state_key)?;
        match crate::fidelity::apply_cohort_section(
            &fidelity,
            request.account,
            request.amount,
            section,
        ) {
            Ok(outcome) => result.fidelity = Some(outcome),
            Err(error) => {
                result = crate::gratis::rejected_result(
                    format!("fidelity section failed: {error}"),
                    result.inputs_canonical_hash,
                )
            }
        }
    }
    result.attestation_tag = sign(
        context,
        &gratis_op_attestation_preimage(result.inputs_canonical_hash, &result)?,
    );
    Ok(EnclaveResponse::GratisOpApplied {
        result: Box::new(result),
    })
}
pub(super) fn promis(
    context: &InProcessTeeContext,
    request: &PromisOpRequest,
) -> Result<EnclaveResponse> {
    let key = state(context, crate::promis::derive_promis_state_key)?;
    let mut result = crate::promis::apply_op(&key, request);
    result.attestation_tag = sign(
        context,
        &promis_op_attestation_preimage(result.inputs_canonical_hash, &result)?,
    );
    Ok(EnclaveResponse::PromisOpApplied {
        result: Box::new(result),
    })
}
pub(super) fn mine_gratis(
    context: &InProcessTeeContext,
    request: &MineGratisFromPromisRequestV1,
) -> Result<EnclaveResponse> {
    let promis = state(context, crate::promis::derive_promis_state_key)?;
    let gratis = state(context, crate::gratis::derive_gratis_state_key)?;
    let fidelity = state(context, crate::fidelity::derive_fidelity_state_key)?;
    let mut result = context.with_offer_key_handle(|offer_secret| {
        crate::promis::apply_mine_gratis(
            crate::promis::MineGratisKeys {
                offer_secret,
                promis_state_key: &promis,
                gratis_state_key: &gratis,
                fidelity_state_key: &fidelity,
            },
            request,
        )
    })?;
    result.attestation_tag = sign(
        context,
        &mine_gratis_from_promis_attestation_preimage(result.inputs_canonical_hash, &result)?,
    );
    Ok(EnclaveResponse::GratisMinedFromPromisV1 {
        result: Box::new(result),
    })
}
pub(super) fn fidelity(
    context: &InProcessTeeContext,
    request: &FidelityCohortRequest,
) -> Result<EnclaveResponse> {
    let key = state(context, crate::fidelity::derive_fidelity_state_key)?;
    let mut result = crate::fidelity::apply_cohort_op(&key, request)?;
    result.attestation_tag = sign(
        context,
        &fidelity_cohort_attestation_preimage(result.inputs_canonical_hash, &result)?,
    );
    Ok(EnclaveResponse::FidelityCohortApplied {
        result: Box::new(result),
    })
}
pub(super) fn snapshot(
    context: &InProcessTeeContext,
    request: &FidelitySnapshotRequest,
) -> Result<EnclaveResponse> {
    let key = state(context, crate::fidelity::derive_fidelity_state_key)?;
    let leagues = crate::fidelity::snapshot_leagues(&key, request)?;
    let inputs_canonical_hash = fidelity_snapshot_canonical_hash(request);
    let attestation_tag = sign(
        context,
        &fidelity_snapshot_attestation_preimage(inputs_canonical_hash, &leagues)?,
    );
    Ok(EnclaveResponse::FidelityLeaguesSnapshotted {
        leagues,
        inputs_canonical_hash,
        attestation_tag,
    })
}
pub(super) fn query(
    context: &InProcessTeeContext,
    request: &FidelityQueryRequest,
) -> Result<EnclaveResponse> {
    let key = state(context, crate::fidelity::derive_fidelity_state_key)?;
    match crate::fidelity::query_index(&key, chain(context), request) {
        Ok(mut result) => {
            result.attestation_tag = sign(
                context,
                &fidelity_query_attestation_preimage(result.inputs_canonical_hash, &result)?,
            );
            Ok(EnclaveResponse::FidelityIndexQueried {
                result: Box::new(result),
            })
        }
        Err(error) => {
            // This reason is signed consensus output, not an erased returned error cause.
            let reason = format!("{error}");
            let inputs_canonical_hash = fidelity_query_canonical_hash(request);
            let attestation_tag = sign(
                context,
                &crate::response_auth::attestation(
                    b"outbe/fidelity/query-rejected/v1",
                    inputs_canonical_hash,
                    &reason,
                )?,
            );
            Ok(EnclaveResponse::FidelityQueryRejected {
                inputs_canonical_hash,
                reason,
                attestation_tag,
            })
        }
    }
}
pub(super) fn account_keys(
    context: &InProcessTeeContext,
    ledger: Ledger,
    account: alloy_primitives::Address,
    requester: [u8; 32],
    signature: &[u8],
) -> Result<EnclaveResponse> {
    let signature = signature
        .try_into()
        .map_err(|_| DomainDispatchError::OwnerSignatureLength)?;
    let domain = crate::confidential::domain_for(ledger);
    let key = context
        .with_offer_material(|material| domain.derive_state_key(material, chain(context), 0))??;
    let keys = domain.account_keys.derive_account_keys(&key, account)?;
    let blob = domain.export_sealed_account_keys(&keys, signature, &requester)?;
    Ok(EnclaveResponse::AccountKeysSealed {
        account,
        sealed: blob.sealed,
        nonce: blob.nonce,
        enclave_ephemeral_pubkey: blob.enclave_ephemeral_pubkey,
    })
}
