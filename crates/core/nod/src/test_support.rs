//! Encrypted fixtures using the production cryptographic implementation.
pub use crate::enclave_client::test_enclave::{install, scope, Guard, NETWORK_SECRET};
use crate::{NodIssueParams, NodItemState};
use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::WwdEntityId;
use outbe_primitives::{
    nod_encryption::{EncryptedNodV2, NodTermsV2},
    time::WorldwideDay,
};

pub fn encrypted_fixture(params: &NodIssueParams, chain_id: u64) -> EncryptedNodV2 {
    let nod_id = crate::NodContract::generate_nod_id(params.owner, params.worldwide_day).unwrap();
    encrypt(
        NodTermsV2 {
            chain_id,
            nod_id,
            owner: params.owner,
            worldwide_day: params.worldwide_day,
            league_id: params.league_id,
            entry_price_minor: params.entry_price_minor,
            issuance_currency: params.issuance_currency,
            reference_currency: params.reference_currency,
        },
        params.gratis_load_minor,
    )
}
fn encrypt(terms: NodTermsV2, amount: U256) -> EncryptedNodV2 {
    install();
    let public = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([0x6b; 32]));
    outbe_tee_enclave::nod_encryption::encrypt_nod(
        &NETWORK_SECRET,
        public.as_bytes(),
        terms,
        amount,
    )
    .unwrap()
}
/// Test-only plaintext inputs; never accepted by a production writer.
pub struct NodItemFixture {
    pub nod_id: WwdEntityId,
    pub owner: Address,
    pub gratis_load_minor: U256,
    pub worldwide_day: WorldwideDay,
    pub league_id: u16,
    pub bucket_key: B256,
    pub issuance_currency: u16,
    pub reference_currency: u16,
    pub issued_at: u64,
    pub is_settled: bool,
}
pub fn item(f: NodItemFixture, entry_price_minor: U256) -> NodItemState {
    let encrypted = encrypt(
        NodTermsV2 {
            chain_id: 1,
            nod_id: f.nod_id,
            owner: f.owner,
            worldwide_day: f.worldwide_day,
            league_id: f.league_id,
            entry_price_minor,
            issuance_currency: f.issuance_currency,
            reference_currency: f.reference_currency,
        },
        f.gratis_load_minor,
    );
    NodItemState {
        nod_id: f.nod_id,
        owner: f.owner,
        encrypted,
        worldwide_day: f.worldwide_day,
        league_id: f.league_id,
        bucket_key: f.bucket_key,
        issuance_currency: f.issuance_currency,
        reference_currency: f.reference_currency,
        issued_at: f.issued_at,
        is_settled: f.is_settled,
    }
}
pub fn set_amount(item: &mut NodItemState, amount: U256) {
    item.encrypted = encrypt(item.encrypted.terms.clone(), amount);
}
pub fn set_terms(item: &mut NodItemState) {
    let amount = crate::api::calculation_amount(item).unwrap();
    let t = &item.encrypted.terms;
    item.encrypted = encrypt(
        NodTermsV2 {
            chain_id: t.chain_id,
            nod_id: item.nod_id,
            owner: item.owner,
            worldwide_day: item.worldwide_day,
            league_id: item.league_id,
            entry_price_minor: t.entry_price_minor,
            issuance_currency: item.issuance_currency,
            reference_currency: item.reference_currency,
        },
        amount,
    );
}

pub fn set_entry_price(item: &mut NodItemState, price: U256) {
    let amount = crate::api::calculation_amount(item).unwrap();
    let mut terms = item.encrypted.terms.clone();
    terms.entry_price_minor = price;
    item.encrypted = encrypt(terms, amount);
}
