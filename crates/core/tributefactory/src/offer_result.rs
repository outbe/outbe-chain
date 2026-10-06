//! Factory input independent of the confidential body's encoding.

use alloy_primitives::B256;
use outbe_tee::{
    protocol::{TributeOfferStatus, TributeZkExpectedHashes},
    tribute_v2::EncryptedTributeOfferResultV2,
};
use outbe_tribute::TributeRecord;

pub(crate) struct ProcessedOffer {
    pub token_id: B256,
    pub tribute: Option<TributeRecord>,
    pub su_hashes: Vec<String>,
    pub wallet_addresses: Vec<String>,
    pub sra_addresses: Vec<String>,
    pub zk_expected_hashes: Option<TributeZkExpectedHashes>,
    pub status: TributeOfferStatus,
}

impl From<EncryptedTributeOfferResultV2> for ProcessedOffer {
    fn from(result: EncryptedTributeOfferResultV2) -> Self {
        Self {
            token_id: result.token_id,
            tribute: result.tribute.map(TributeRecord::from_encrypted),
            su_hashes: result.su_hashes,
            wallet_addresses: result.wallet_addresses,
            sra_addresses: result.sra_addresses,
            zk_expected_hashes: result.zk_expected_hashes,
            status: result.status,
        }
    }
}

#[cfg(any(test, feature = "bench-utils"))]
pub(crate) fn legacy_fixture(
    offer: &outbe_tee::protocol::EncryptedTributeOffer,
    result: outbe_tee::protocol::TributeOfferResult,
) -> outbe_primitives::error::Result<ProcessedOffer> {
    if result.owner != offer.owner && matches!(result.status, TributeOfferStatus::Created) {
        return Err(crate::errors::TributeFactoryError::InvalidCanonicalIdentity.into());
    }
    let body = outbe_tribute::TributeData {
        tribute_id: outbe_compressed_entities::WwdEntityId::from_day_and_digest(
            offer.worldwide_day,
            result.token_id,
        ),
        owner: result.owner,
        worldwide_day: offer.worldwide_day,
        issuance_amount_minor: result.issuance_amount_minor,
        nominal_amount_minor: result.nominal_amount_minor,
        issuance_currency: offer.tribute_currency,
        reference_currency: offer.reference_currency,
        tribute_price_minor: result.effective_reference_price_minor,
        exclude_from_intex_issuance: offer.exclude_from_intex_issuance,
    };
    Ok(ProcessedOffer {
        token_id: result.token_id,
        tribute: Some(TributeRecord::from_legacy(body)),
        su_hashes: result.su_hashes,
        wallet_addresses: result.wallet_addresses,
        sra_addresses: result.sra_addresses,
        zk_expected_hashes: result.zk_expected_hashes,
        status: result.status,
    })
}
