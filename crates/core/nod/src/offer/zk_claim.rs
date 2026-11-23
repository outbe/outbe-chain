//! Canonical TributeDraft public-input derivation.

use alloy_primitives::B256;
use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use outbe_zk::claim::TributeDraftClaim;
use outbe_zk::protocol::entity::Entity as EntityTrait;

use super::compute::CanonicalAmount;
use super::errors::{OfferResult, ProcessError};
use super::payload::TributeInputPayload;
use super::protocol::{EncryptedTributeOffer, TributeZkExpectedHashes};

pub fn derive_expected_hashes(
    offer: &EncryptedTributeOffer,
    payload: &TributeInputPayload,
    amount: &CanonicalAmount,
) -> OfferResult<Option<TributeZkExpectedHashes>> {
    let Some(context) = &offer.zk_context else {
        return Ok(None);
    };

    let id = parse_b256(&payload.tribute_draft_id, "tribute_draft_id")?;
    let su_ids = payload
        .su_hashes
        .iter()
        .map(|value| parse_b256(value, "su_hash"))
        .collect::<OfferResult<Vec<_>>>()?;
    let su_ids = outbe_zk::codec::sort_set(&su_ids).map_err(ProcessError::InvalidCanonicalSuIds)?;

    let draft = TributeDraftClaim {
        id,
        derived_owner: context.derived_owner,
        worldwide_day: offer.worldwide_day.into(),
        currency: offer.tribute_currency,
        base: amount.base,
        atto: amount.micro,
        su_ids,
    };
    let nft_hash = draft
        .entity_hash()
        .map_err(ProcessError::InvalidCanonicalTributeDraft)?;
    let sender = offer.owner.into_array();
    let commitment_id: [u8; 32] = id.into();
    let binding_hash = outbe_zk::primitive::hash::binding(
        &sender,
        &commitment_id,
        context.chain_id,
        context.l2_chain_id,
    )
    .map_err(ProcessError::DeriveBindingHashFailed)?;

    Ok(Some(TributeZkExpectedHashes {
        nft_hash: field_to_b256(nft_hash),
        binding_hash: field_to_b256(binding_hash),
    }))
}

fn parse_b256(value: &str, what: &'static str) -> OfferResult<B256> {
    value
        .parse::<B256>()
        .map_err(|_| ProcessError::InvalidB256Hex { what }.into())
}

fn field_to_b256(field: Fr) -> B256 {
    let bytes = field.into_bigint().to_bytes_be();
    let mut out = [0u8; 32];
    let start = out.len() - bytes.len();
    out[start..].copy_from_slice(&bytes);
    B256::from(out)
}
