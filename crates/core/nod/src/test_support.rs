//! Encrypted fixtures using the production cryptographic implementation.
pub use crate::enclave_client::test_enclave::{install, scope, uninstall, Guard, NETWORK_SECRET};
use crate::{NodIssueParams, NodItemState};
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    nod_encryption::{EncryptedNodV1, NodTermsV1},
    time::WorldwideDay,
};

pub fn encrypted_fixture(params: &NodIssueParams, chain_id: u64) -> EncryptedNodV1 {
    let nod_id = crate::identity::generate_nod_id(params.owner, params.worldwide_day);
    encrypt(
        NodTermsV1 {
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
fn encrypt(terms: NodTermsV1, amount: U256) -> EncryptedNodV1 {
    install();
    outbe_tee_enclave::nod_encryption::encrypt_nod(&NETWORK_SECRET, terms, amount).unwrap()
}
/// Test-only plaintext inputs; never accepted by a production writer.
pub struct NodItemFixture {
    pub nod_id: U256,
    pub owner: Address,
    pub gratis_load_minor: U256,
    pub worldwide_day: WorldwideDay,
    pub league_id: u16,
    pub group_key: B256,
    pub issuance_currency: u16,
    pub reference_currency: u16,
    pub issued_at: u64,
    pub is_settled: bool,
}
pub fn item(f: NodItemFixture, entry_price_minor: U256) -> NodItemState {
    let encrypted = encrypt(
        NodTermsV1 {
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
        group_key: f.group_key,
        issuance_currency: f.issuance_currency,
        reference_currency: f.reference_currency,
        issued_at: f.issued_at,
        is_settled: f.is_settled,
    }
}
/// Creates an unsettled fixture with a group derived from its encrypted terms.
pub fn unsettled_item(terms: NodTermsV1, amount: U256, issued_at: u64) -> NodItemState {
    assert_eq!(terms.chain_id, 1, "NOD item fixture requires chain 1");
    let group_key = group_key(issued_at, terms.entry_price_minor, terms.reference_currency);
    item(
        NodItemFixture {
            nod_id: terms.nod_id,
            owner: terms.owner,
            gratis_load_minor: amount,
            worldwide_day: terms.worldwide_day,
            league_id: terms.league_id,
            group_key,
            issuance_currency: terms.issuance_currency,
            reference_currency: terms.reference_currency,
            issued_at,
            is_settled: false,
        },
        terms.entry_price_minor,
    )
}

pub fn set_amount(item: &mut NodItemState, amount: U256) {
    item.encrypted = encrypt(item.encrypted.terms.clone(), amount);
}
pub fn set_terms(item: &mut NodItemState) {
    let amount = crate::api::calculation_amount(item).unwrap();
    let t = &item.encrypted.terms;
    item.encrypted = encrypt(
        NodTermsV1 {
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

/// Unsettled Nod used by execution and idle-scan fixtures.
pub fn sample_item(owner: Address, day: WorldwideDay) -> NodItemState {
    let nod_id = crate::identity::generate_nod_id(owner, day);
    item(
        NodItemFixture {
            is_settled: false,
            nod_id,
            owner,
            gratis_load_minor: U256::from(11),
            worldwide_day: day,
            league_id: 4,
            group_key: group_key(1_752_534_000, U256::from(5), 978),
            issuance_currency: 840,
            reference_currency: 978,
            issued_at: 1_752_534_000,
        },
        U256::from(5),
    )
}

/// The group NodFactory puts a Nod in under the production call rate the fixtures seal.
pub fn group_key(issued_at: u64, entry_price_minor: U256, reference_currency: u16) -> B256 {
    group_key_at(
        &crate::config::NodParams::PROD,
        issued_at,
        entry_price_minor,
        reference_currency,
    )
}

/// The group NodFactory puts a Nod in under the call rate of `params`.
pub fn group_key_at(
    params: &crate::config::NodParams,
    issued_at: u64,
    entry_price_minor: U256,
    reference_currency: u16,
) -> B256 {
    crate::identity::group_key(
        issued_at,
        outbe_primitives::markup::marked_up(entry_price_minor, params.call_rate).unwrap(),
        reference_currency,
    )
    .expect("date step")
}

/// Records `item` as NodFactory issues it: the terms its entry seals and its load.
pub fn add(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    item: &NodItemState,
) -> outbe_primitives::error::Result<()> {
    crate::api::add_nod(
        storage,
        item,
        sealed_terms(storage, item, item.entry_price_minor()),
        crate::api::calculation_amount(item)?,
    )
}

/// The call terms NodFactory seals on a group opened by `item` at `entry_price_minor`,
/// from the live profile. Fixtures record Nods without the factory.
pub fn sealed_terms(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    item: &NodItemState,
    entry_price_minor: U256,
) -> Option<crate::schema::CallTerms> {
    if entry_price_minor.is_zero() {
        return None;
    }
    let params = crate::config::read(storage).unwrap();
    Some(crate::schema::CallTerms {
        call_price_minor: outbe_primitives::markup::marked_up(entry_price_minor, params.call_rate)
            .unwrap(),
        reference_currency: item.reference_currency,
        call_rate: params.call_rate,
        call_window_seconds: params.call_window_seconds,
        call_threshold_seconds: params.call_threshold_seconds,
        call_notice_period_seconds: params.call_notice_period_seconds,
    })
}
