//! Canonical TributeDraft public-input derivation inside the enclave.
//!
//! The claim is folded from:
//!
//! - the encrypted payload (draft id, amount, su ids),
//! - the cleartext offer (`worldwide_day`, `tribute_currency`),
//! - `derived_owner`, which is public input zero of the submitted Tribute proof.
//!
//! Keeping the fold here binds the proof claim to the plaintext the enclave
//! actually decrypted without exposing the draft id or amount fields to the
//! host.
//!
//! The day and currency are folded in. Thus a caller who declares cleartext
//! values that disagree with their L2-attested draft produces an `nft_hash` that
//! does not match the proof's public input. Such an offer is rejected. That is
//! what keeps those two fields bound now that they no longer travel encrypted.

use alloy_primitives::B256;
use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use outbe_tee::protocol::{EncryptedTributeOffer, TributeZkExpectedHashes};
use outbe_zk::claim::TributeDraftClaim;
use outbe_zk::protocol::entity::Entity as EntityTrait;

use crate::compute::CanonicalAmount;
use crate::errors::{EnclaveResult, ProcessError};
use crate::payload::TributeInputPayload;

pub(crate) fn derive_expected_hashes(
    offer: &EncryptedTributeOffer,
    payload: &TributeInputPayload,
    amount: &CanonicalAmount,
) -> EnclaveResult<Option<TributeZkExpectedHashes>> {
    let Some(context) = &offer.zk_context else {
        return Ok(None);
    };

    let id = parse_b256(&payload.tribute_draft_id, "tribute_draft_id")?;
    let su_ids = payload
        .su_hashes
        .iter()
        .map(|value| parse_b256(value, "su_hash"))
        .collect::<EnclaveResult<Vec<_>>>()?;
    let su_ids =
        outbe_zk::codec::sort_set(&su_ids).map_err(ProcessError::invalid_canonical_su_ids)?;

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
        .map_err(ProcessError::invalid_canonical_draft)?;
    let sender = offer.owner.into_array();
    let commitment_id: [u8; 32] = id.into();
    let binding_hash = outbe_zk::primitive::hash::binding(
        &sender,
        &commitment_id,
        context.chain_id,
        context.l2_chain_id,
    )
    .map_err(ProcessError::derive_binding_hash)?;

    Ok(Some(TributeZkExpectedHashes {
        nft_hash: field_to_b256(nft_hash),
        binding_hash: field_to_b256(binding_hash),
    }))
}

fn parse_b256(value: &str, what: &'static str) -> EnclaveResult<B256> {
    value
        .parse::<B256>()
        .map_err(|_| ProcessError::InvalidB256Hex { what }.into())
}

fn field_to_b256(field: Fr) -> B256 {
    let bytes = field.into_bigint().to_bytes_be();
    let mut out = [0u8; 32];
    out[32 - bytes.len()..].copy_from_slice(&bytes);
    B256::from(out)
}
