//! State-backed Nod preparation. All inputs arrive as bounded arguments.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    nod_materialization::{
        verify_nod_materialization_batch, NodMaterializationBatchV1, NodMaterializationHeadV1,
        ProtectedNodMaterializationV2, VerifiedNodMaterializationBatchV1,
    },
    profile::poc_schema_limits,
};
use outbe_primitives::{
    math::scaled_math::checked_mul_div_floor,
    nod_encryption::{EncryptedNodV2, NodTermsV2},
    time::WorldwideDay,
    units::SCALE_1E6_U256,
    wwd_entity_id::{derive_poseidon_entity_id, WwdEntityId},
};
use outbe_tee::nod_state_materialization::{
    PrepareStateNodsRequest, StateNodAuthority, TributeSourceLookup,
};
use ring::hmac;
use zeroize::Zeroizing;

use crate::{
    crypto::{chacha20poly1305_decrypt, chacha20poly1305_encrypt, hkdf_sha256},
    errors::{Result, TeeError},
};

pub fn request_sources(request: &PrepareStateNodsRequest) -> Result<TributeSourceLookup> {
    if !request.sources.is_empty() || !request.authority.source_bindings.is_empty() {
        return Err(rejected("source lookup already contains source data"));
    }
    let (_, verified) = verified_batch(request)?;
    let mut seen = BTreeSet::new();
    let mut tribute_ids = Vec::with_capacity(verified.actions().len());
    for action in verified.actions() {
        let day = WorldwideDay::new(action.wwd);
        let id = derive_poseidon_entity_id(action.owner, day).map_err(rejected)?;
        if id.as_slice() != action.tribute_id.as_slice() || !seen.insert(id) {
            return Err(rejected("certified Tribute identity mismatch"));
        }
        tribute_ids.push(id);
    }
    Ok(TributeSourceLookup {
        subject: request.authority.source,
        tribute_ids,
    })
}

pub fn prepare(
    secret: &[u8; 32],
    request: &PrepareStateNodsRequest,
) -> Result<ProtectedNodMaterializationV2> {
    let (head, verified) = verified_batch(request)?;
    let nods = verify_and_encrypt(secret, request, &verified)?;
    let plaintext =
        Zeroizing::new(postcard::to_allocvec(request).map_err(|_| TeeError::EncryptFailed)?);
    let mut preimage = b"outbe/nod/state-materialization-binding".to_vec();
    preimage.extend_from_slice(&plaintext);
    let preimage = Zeroizing::new(preimage);
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, secret), &preimage);
    let binding = B256::from_slice(tag.as_ref());
    let key = witness_key(secret, binding)?;
    let encrypted_witness = chacha20poly1305_encrypt(&key, &[0u8; 12], &plaintext)?;
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
    authority: &StateNodAuthority,
    carrier: &ProtectedNodMaterializationV2,
) -> Result<Vec<EncryptedNodV2>> {
    let key = witness_key(secret, carrier.encryption_binding)?;
    let plaintext = Zeroizing::new(chacha20poly1305_decrypt(
        &key,
        &[0u8; 12],
        &carrier.encrypted_witness.0,
    )?);
    let request: PrepareStateNodsRequest =
        postcard::from_bytes(&plaintext).map_err(|_| TeeError::DecryptFailed)?;
    if &request.authority != authority {
        return Err(rejected("stale state NOD authority"));
    }
    let expected = prepare(secret, &request)?;
    if &expected != carrier {
        return Err(rejected("state NOD carrier mismatch"));
    }
    let (_, verified) = verified_batch(&request)?;
    verify_and_encrypt(secret, &request, &verified)
}

fn witness_key(secret: &[u8; 32], binding: B256) -> Result<Zeroizing<[u8; 32]>> {
    hkdf_sha256(
        secret,
        binding.as_slice(),
        b"outbe/nod/state-materialization-witness",
    )
    .map(Zeroizing::new)
}

fn verified_batch(
    request: &PrepareStateNodsRequest,
) -> Result<(NodMaterializationHeadV1, VerifiedNodMaterializationBatchV1)> {
    let limits = poc_schema_limits();
    let source = &request.authority.source;
    let head = NodMaterializationHeadV1::decode_canonical(&request.authority.head, &limits)
        .map_err(rejected)?;
    if source.chain_id == 0
        || source.checkpoint.block_number == 0
        || source.checkpoint.block_hash.is_zero()
        || source.checkpoint.state_root.is_zero()
        || source.worldwide_day.value() != head.worldwide_day
        || source.tribute_count != head.nod_count
    {
        return Err(rejected("state NOD source checkpoint mismatch"));
    }
    let batch =
        NodMaterializationBatchV1::decode_canonical(&request.batch, &limits).map_err(rejected)?;
    let verified =
        verify_nod_materialization_batch(&batch, &head, request.authority.subtree_height, &limits)
            .map_err(rejected)?;
    Ok((head, verified))
}

fn verify_and_encrypt(
    secret: &[u8; 32],
    request: &PrepareStateNodsRequest,
    verified: &VerifiedNodMaterializationBatchV1,
) -> Result<Vec<EncryptedNodV2>> {
    let count = verified.actions().len();
    if request.sources.len() != count || request.authority.source_bindings.len() != count {
        return Err(rejected("state NOD source count mismatch"));
    }
    let mut fractions = BTreeMap::new();
    for fraction in &request.league_fractions {
        if fraction.fraction_fp.is_zero()
            || fractions
                .insert(fraction.league_id, fraction.fraction_fp)
                .is_some()
        {
            return Err(rejected("state NOD league fraction mismatch"));
        }
    }
    let limits = outbe_tee::tribute_source::proof_limits();
    let mut seen = BTreeSet::new();
    verified
        .actions()
        .iter()
        .zip(&request.sources)
        .zip(&request.authority.source_bindings)
        .map(|((action, source), binding)| {
            let tribute = &source.tribute;
            if action.tribute_id.as_slice() != tribute.context.tribute_id.as_slice()
                || !seen.insert(tribute.context.tribute_id)
            {
                return Err(rejected("state NOD Tribute identity mismatch"));
            }
            source
                .verify(&request.authority.source, *binding, limits)
                .map_err(rejected)?;
            let nod_id = WwdEntityId::try_from(action.nod_id.as_slice()).map_err(rejected)?;
            let terms = NodTermsV2 {
                chain_id: request.authority.source.chain_id,
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
                .ok_or_else(|| rejected("state NOD league fraction missing"))?;
            let load =
                checked_mul_div_floor(amounts.nominal_amount_minor, fraction, SCALE_1E6_U256)
                    .map_err(rejected)?;
            if load.is_zero() {
                return Err(rejected("state NOD gratis load is zero"));
            }
            crate::nod_encryption::encrypt_nod_for_tribute(secret, tribute, terms, load)
        })
        .collect()
}

fn rejected(error: impl std::fmt::Display) -> TeeError {
    TeeError::tribute_offer_reject(error)
}
