//! Verify the original certified action and source proof before encrypting NODs.
use crate::{
    crypto::{chacha20poly1305_decrypt, chacha20poly1305_encrypt, hkdf_sha256},
    errors::{NodMaterializationRejectReason, Result, TeeError},
};
use std::collections::BTreeMap;

use alloy_primitives::{B256, U256};
use outbe_compressed_entities::{
    encode_tribute_v2, verify_body_in_collection, CeDomain, CollectionBodyProofV1, StoredBody,
    TRIBUTE_BODY_SCHEMA_V2,
};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    nod_materialization::{
        verify_nod_materialization_batch, NodMaterializationBatchV1, NodMaterializationHeadV1,
        ProtectedNodMaterializationV2,
    },
    profile::poc_schema_limits,
};
use outbe_primitives::{
    math::scaled_math::checked_mul_div_floor,
    nod_encryption::{EncryptedNodV2, NodTermsV2},
    time::WorldwideDay,
    units::SCALE_1E6_U256,
};
use outbe_tee::nod_materialization::{
    NodMaterializationAuthorityV2, PrepareEncryptedNodsRequestV2,
};
use ring::hmac;
use zeroize::Zeroizing;
pub fn prepare(
    secret: &[u8; 32],
    request: &PrepareEncryptedNodsRequestV2,
) -> Result<ProtectedNodMaterializationV2> {
    let nods = verify_and_encrypt(secret, request)?;
    let plaintext =
        Zeroizing::new(postcard::to_allocvec(request).map_err(|_| TeeError::EncryptFailed)?);
    let mut input = b"outbe/nod/materialization-binding/v2".to_vec();
    input.extend_from_slice(&plaintext);
    let input = Zeroizing::new(input);
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, secret), &input);
    let binding = B256::from_slice(tag.as_ref());
    let key = Zeroizing::new(hkdf_sha256(
        secret,
        binding.as_slice(),
        b"outbe/nod/materialization-witness/v2",
    )?);
    let encrypted_witness = chacha20poly1305_encrypt(&key, &[0u8; 12], &plaintext)?;
    let head =
        NodMaterializationHeadV1::decode_canonical(&request.authority.head, &poc_schema_limits())
            .map_err(TeeError::tribute_offer_reject)?;
    Ok(ProtectedNodMaterializationV2 {
        queue_sequence: head.queue_sequence,
        first_nod_ordinal: head.next_nod_ordinal,
        encryption_binding: binding,
        encrypted_witness: BoundedBytes(encrypted_witness),
        encrypted_nods: nods
            .iter()
            .map(|nod| {
                serde_json::to_vec(nod)
                    .map(BoundedBytes)
                    .map_err(|_| TeeError::EncryptFailed)
            })
            .collect::<Result<_>>()?,
    })
}
pub fn open(
    secret: &[u8; 32],
    authority: &NodMaterializationAuthorityV2,
    carrier: &ProtectedNodMaterializationV2,
) -> Result<Vec<EncryptedNodV2>> {
    let key = Zeroizing::new(hkdf_sha256(
        secret,
        carrier.encryption_binding.as_slice(),
        b"outbe/nod/materialization-witness/v2",
    )?);
    let plaintext = Zeroizing::new(chacha20poly1305_decrypt(
        &key,
        &[0u8; 12],
        &carrier.encrypted_witness.0,
    )?);
    let request: PrepareEncryptedNodsRequestV2 =
        postcard::from_bytes(&plaintext).map_err(|_| TeeError::DecryptFailed)?;
    if &request.authority != authority {
        return Err(NodMaterializationRejectReason::StaleAuthority.into());
    }
    let expected = prepare(secret, &request)?;
    if &expected != carrier {
        return Err(NodMaterializationRejectReason::CiphertextMismatch.into());
    }
    verify_and_encrypt(secret, &request)
}
fn verify_and_encrypt(
    secret: &[u8; 32],
    request: &PrepareEncryptedNodsRequestV2,
) -> Result<Vec<EncryptedNodV2>> {
    let limits = poc_schema_limits();
    let head = NodMaterializationHeadV1::decode_canonical(&request.authority.head, &limits)
        .map_err(TeeError::tribute_offer_reject)?;
    let batch = NodMaterializationBatchV1::decode_canonical(&request.batch, &limits)
        .map_err(TeeError::tribute_offer_reject)?;
    let verified =
        verify_nod_materialization_batch(&batch, &head, request.authority.subtree_height, &limits)
            .map_err(TeeError::tribute_offer_reject)?;
    if verified.actions().len() != request.sources.len() {
        return Err(NodMaterializationRejectReason::SourceCountMismatch.into());
    }
    let mut fractions = BTreeMap::new();
    for fraction in &request.league_fractions {
        if fraction.fraction_fp.is_zero()
            || fractions
                .insert(fraction.league_id, fraction.fraction_fp)
                .is_some()
        {
            return Err(NodMaterializationRejectReason::LeagueFraction.into());
        }
    }
    verified
        .actions()
        .iter()
        .zip(&request.sources)
        .map(|(action, source)| {
            let tribute = &source.tribute;
            if action.tribute_id.as_slice() != tribute.context.tribute_id.as_slice() {
                return Err(NodMaterializationRejectReason::TributeIdentityMismatch.into());
            }
            let proof: CollectionBodyProofV1 = postcard::from_bytes(&source.proof)
                .map_err(|_| NodMaterializationRejectReason::InvalidTributeProofEncoding)?;
            let body = StoredBody::new(
                TRIBUTE_BODY_SCHEMA_V2,
                encode_tribute_v2(tribute).map_err(TeeError::tribute_offer_reject)?,
            )
            .map_err(TeeError::tribute_offer_reject)?
            .encode();
            verify_body_in_collection(
                request.authority.sealed_tribute_root,
                CeDomain::Tribute,
                tribute.context.tribute_id,
                &body,
                &proof,
            )
            .map_err(TeeError::tribute_offer_reject)?;
            let nod_id =
                outbe_primitives::wwd_entity_id::WwdEntityId::try_from(action.nod_id.as_slice())
                    .map_err(TeeError::tribute_offer_reject)?;
            let terms = NodTermsV2 {
                chain_id: request.authority.chain_id,
                nod_id,
                owner: action.owner,
                worldwide_day: WorldwideDay::new(action.wwd),
                league_id: action.league_id,
                entry_price_minor: action.entry_price_minor,
                issuance_currency: action.issuance_currency,
                reference_currency: action.reference_currency,
            };
            let amounts = crate::tribute_encryption::decrypt_tribute(secret, tribute)?;
            let fraction = fractions
                .get(&action.league_id)
                .copied()
                .ok_or(NodMaterializationRejectReason::LeagueFraction)?;
            let load = gratis_load(
                amounts.nominal_amount_minor,
                fraction,
                request.authority.nod_gratis_load_max,
            )?;
            crate::nod_encryption::encrypt_nod_for_tribute(secret, tribute, terms, load)
        })
        .collect()
}
fn gratis_load(
    nominal_amount_minor: U256,
    fraction_fp: U256,
    nod_gratis_load_max: U256,
) -> Result<U256> {
    let load = checked_mul_div_floor(nominal_amount_minor, fraction_fp, SCALE_1E6_U256)
        .map_err(|_| NodMaterializationRejectReason::GratisLoad)?
        .min(nod_gratis_load_max);
    if load.is_zero() {
        return Err(NodMaterializationRejectReason::GratisLoad.into());
    }
    Ok(load)
}
