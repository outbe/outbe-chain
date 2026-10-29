//! Owner-local NOD decryption used by hardware acceptance scenarios.
use crate::internal::{eth, tribute_keys};
use crate::world::World;
use alloy_primitives::U256;
use outbe_primitives::{
    nod_encryption::{EncryptedNodV1, NodTermsV1},
    time::WorldwideDay,
};

pub(crate) fn encrypted(data: &eth::INod::NodData) -> EncryptedNodV1 {
    EncryptedNodV1 {
        terms: NodTermsV1 {
            chain_id: data.chainId,
            nod_id: data.nodId,
            owner: data.owner,
            worldwide_day: WorldwideDay::new(data.worldwideDay),
            league_id: data.leagueId,
            entry_price_minor: data.entryPriceMinor,
            issuance_currency: data.issuanceCurrency,
            reference_currency: data.referenceCurrency,
        },
        encryption_binding: data.encryptionBinding,
        encrypted_gratis_amount: data.encryptedGratisAmount.to_vec(),
    }
}
pub(crate) fn decrypt(world: &World, data: &eth::INod::NodData) -> U256 {
    let view_key = tribute_keys::nod_view_key(world, data.owner);
    outbe_tee::nod_decrypt::decrypt_nod_for_owner(&view_key, &encrypted(data))
        .expect("owner-local NOD decryption")
}
