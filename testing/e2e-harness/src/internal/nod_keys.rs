//! Owner-local NOD decryption used by hardware acceptance scenarios.
use crate::internal::{eth, tribute_keys};
use crate::world::World;
use alloy_primitives::{Address, U256};
use outbe_primitives::{
    nod_encryption::{EncryptedNodV2, NodTermsV2},
    time::WorldwideDay,
    wwd_entity_id::WwdEntityId,
};

pub(crate) fn encrypted(data: &eth::INod::NodData) -> EncryptedNodV2 {
    EncryptedNodV2 {
        terms: NodTermsV2 {
            chain_id: data.chainId,
            nod_id: WwdEntityId::from(data.nodId),
            owner: data.owner,
            worldwide_day: WorldwideDay::new(data.worldwideDay),
            league_id: data.leagueId,
            entry_price_minor: data.entryPriceMinor,
            issuance_currency: data.issuanceCurrency,
            reference_currency: data.referenceCurrency,
        },
        encryption_binding: data.encryptionBinding,
        encrypted_creator_public_key: data.encryptedCreatorPublicKey.to_vec(),
        encrypted_gratis_amount: data.encryptedGratisAmount.to_vec(),
    }
}
pub(crate) fn decrypt(world: &World, data: &eth::INod::NodData) -> U256 {
    let public = world
        .rpc
        .tribute_network_public_key(world.validators.primary_port())
        .expect("network encryption public key");
    decrypt_with_public(data, &public)
}
pub(crate) fn decrypt_with_public(data: &eth::INod::NodData, public: &[u8; 32]) -> U256 {
    outbe_tee::nod_decrypt::decrypt_nod_for_owner(
        &tribute_keys::secret(data.owner),
        public,
        &encrypted(data),
    )
    .expect("owner-local NOD decryption")
}
pub(crate) fn public(owner: Address) -> alloy_primitives::B256 {
    tribute_keys::public_hex(owner)
        .parse()
        .expect("creator encryption public key")
}
