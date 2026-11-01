//! Owner-local NOD decryption used by hardware acceptance scenarios.
use crate::internal::{eth, tribute_keys};
use crate::world::World;
use alloy_primitives::{Address, U256};
use outbe_primitives::nod_encryption::{EncryptedNodV1, NodQuoteContextV1, NodTermsV1};

pub(crate) fn encrypted(data: &eth::INod::NodData) -> EncryptedNodV1 {
    EncryptedNodV1 {
        terms: NodTermsV1 {
            chain_id: data.chainId,
            nod_id: data.nodId,
            owner: data.owner,
            emission_day: outbe_primitives::series_id::SeriesId::from(data.seriesId).day(),
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

pub(crate) fn settlement_cost_minor(world: &World, data: &eth::INod::NodData) -> U256 {
    let gratis_load = decrypt(world, data);
    outbe_nod::api::settlement_cost_minor(data.entryPriceMinor, gratis_load)
        .expect("owner-local NOD settlement cost")
}

pub(crate) fn decrypt_quote(
    world: &World,
    owner: Address,
    nod_id: U256,
    asset: Address,
    quote: &eth::INodFactory::quoteSettlementReturn,
) -> U256 {
    let view_key = tribute_keys::nod_view_key(world, owner);
    let chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .expect("chain ID for Nod quote decryption");
    outbe_tee::nod_decrypt::decrypt_nod_quote_for_owner(
        &view_key,
        &NodQuoteContextV1 {
            chain_id,
            nod_id,
            owner,
            asset,
            settlement_currency: quote.settlementCurrency,
            snapshot_id: quote.snapshotId,
        },
        &quote.encryptedPaymentMinor,
    )
    .expect("owner-local NOD quote decryption")
}
