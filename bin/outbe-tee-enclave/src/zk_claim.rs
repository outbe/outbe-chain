//! Canonical TributeDraft public-input derivation inside the enclave.
//!
//! The claim is folded from the encrypted payload (draft id, amount, su ids),
//! the cleartext offer (`worldwide_day`, `tribute_currency`) and
//! `owner`, which is public input zero of the submitted full proof.
//! Keeping the fold here binds the proof claim to the plaintext the enclave
//! actually decrypted without exposing the draft id or amount fields to the
//! host.
//!
//! Because the day and currency are folded in, a caller who declares cleartext
//! values that disagree with their L2-attested draft produces an `nft_hash` that
//! does not match the proof's public input, and the offer is rejected. That is
//! what keeps those two fields bound now that they no longer travel encrypted.
//!
//! `binding_hash` folds the caller's L2 chain id as a sixth preimage element, so
//! a proof minted for one L2 does not verify as another's even under a
//! byte-identical circuit. The claim and both formulas live in
//! `outbe-l2-claims`; the enclave holds no copy of either.

use alloy_primitives::B256;
use outbe_l2_claims::claims::tribute::{binding, TributeDraftClaim};
use outbe_l2_claims::outbe_zk_core::codec::{field_to_b256, sort_set};
use outbe_l2_claims::outbe_zk_core::entity::Entity;
use outbe_tee::protocol::{EncryptedTributeOffer, TributeZkExpectedHashes};

use crate::compute::CanonicalAmount;
use crate::payload::TributeInputPayload;

pub(crate) fn derive_expected_hashes(
    offer: &EncryptedTributeOffer,
    payload: &TributeInputPayload,
    amount: &CanonicalAmount,
) -> Result<Option<TributeZkExpectedHashes>, String> {
    let Some(context) = &offer.zk_context else {
        return Ok(None);
    };

    let id = parse_b256(&payload.tribute_draft_id, "tribute_draft_id")?;
    let su_ids = payload
        .su_hashes
        .iter()
        .map(|value| parse_b256(value, "su_hash"))
        .collect::<Result<Vec<_>, _>>()?;
    let su_ids = sort_set(&su_ids).map_err(|error| format!("invalid canonical su_ids: {error}"))?;

    let draft = TributeDraftClaim {
        id,
        owner: context.owner,
        worldwide_day: offer.worldwide_day.into(),
        currency: offer.tribute_currency,
        base: amount.base,
        micro: amount.micro,
        su_ids,
    };
    let nft_hash = draft
        .entity_hash()
        .map_err(|error| format!("invalid canonical TributeDraft: {error}"))?;
    let binding_hash = binding(
        &offer.owner.into_array(),
        &id.0,
        context.chain_id,
        context.l2_chain_id,
    )
    .map_err(|error| format!("failed to derive binding_hash: {error}"))?;

    Ok(Some(TributeZkExpectedHashes {
        nft_hash: field_to_b256(&nft_hash).map_err(|error| format!("nft_hash: {error}"))?,
        binding_hash: field_to_b256(&binding_hash)
            .map_err(|error| format!("binding_hash: {error}"))?,
    }))
}

fn parse_b256(value: &str, what: &'static str) -> Result<B256, String> {
    value
        .parse::<B256>()
        .map_err(|_| format!("{what} must be 0x-prefixed 32-byte hex"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{hex, Address, U256};
    use outbe_primitives::time::WorldwideDay;
    use outbe_tee::protocol::TributeZkContext;

    /// A `B256` holding the small field value `n` - same helper the circuits
    /// crate's frozen-vector test uses.
    fn b256(n: u64) -> B256 {
        B256::from(U256::from(n))
    }

    fn offer(owner: Address, l2_chain_id: u64) -> EncryptedTributeOffer {
        EncryptedTributeOffer {
            owner,
            cipher_text: Vec::new(),
            nonce: Vec::new(),
            ephemeral_pubkey: U256::ZERO,
            worldwide_day: WorldwideDay::new(20_260_802),
            tribute_currency: 840,
            reference_currency: 840,
            exclude_from_intex_issuance: false,
            issuance_wwd_vwap_minor: U256::ZERO,
            reference_wwd_vwap_minor: U256::ZERO,
            reference_scurve_minor: U256::ZERO,
            zk_context: Some(TributeZkContext {
                owner: b256(2),
                chain_id: 19_280_501,
                l2_chain_id,
            }),
        }
    }

    fn payload(id: B256, su_ids: &[B256]) -> TributeInputPayload {
        TributeInputPayload {
            creator: "alice".to_owned(),
            tribute_draft_id: format!("0x{}", hex::encode(id)),
            amount_base: "100".to_owned(),
            amount_micro: "0".to_owned(),
            su_hashes: su_ids
                .iter()
                .map(|id| format!("0x{}", hex::encode(id)))
                .collect(),
            wallet_addresses: Vec::new(),
            sra_addresses: Vec::new(),
        }
    }

    const AMOUNT: CanonicalAmount = CanonicalAmount {
        base: 100,
        micro: 0,
        amount_minor: U256::ZERO,
    };

    fn hashes(
        offer: &EncryptedTributeOffer,
        payload: &TributeInputPayload,
    ) -> TributeZkExpectedHashes {
        derive_expected_hashes(offer, payload, &AMOUNT)
            .expect("derivation")
            .expect("zk context present")
    }

    /// Consensus parity: the enclave's fold of the canonical claim must equal
    /// the value frozen in `outbe-l2-claims/tests/tribute.rs`
    /// (`entity_hash_keeps_the_frozen_fold_order`). Same claim, same preimage,
    /// same bytes - if this drifts, proofs stop verifying.
    #[test]
    fn nft_hash_matches_the_circuits_frozen_vector() {
        let bound = hashes(
            &offer(Address::repeat_byte(0x01), 57_005),
            &payload(b256(1), &[b256(3), b256(4)]),
        );
        assert_eq!(
            bound.nft_hash,
            "0x20fa811a8b272be2f39466daa3dbe1215832c7b6f3ac98d6672e6f137c1f831c"
                .parse::<B256>()
                .unwrap()
        );
    }

    /// Same parity for `binding`, whose sixth preimage element is the L2 chain
    /// id the context now carries (`binding_keeps_the_frozen_vector`), plus the
    /// proof that the enclave actually threads it (`binding_separates_l2_chains`).
    #[test]
    fn binding_hash_matches_the_circuits_frozen_vector_and_folds_the_l2_id() {
        let payload = payload(B256::repeat_byte(0x02), &[b256(3)]);
        let bound = hashes(&offer(Address::repeat_byte(0x01), 57_005), &payload);
        assert_eq!(
            bound.binding_hash,
            "0x1fa0a96020985973a74705b5e7b6f54f3c3f9b9ca1d0de200c93566e2eb73402"
                .parse::<B256>()
                .unwrap()
        );

        let other_l2 = hashes(&offer(Address::repeat_byte(0x01), 57_006), &payload);
        assert_ne!(bound.binding_hash, other_l2.binding_hash);
        assert_eq!(bound.nft_hash, other_l2.nft_hash);
    }
}
