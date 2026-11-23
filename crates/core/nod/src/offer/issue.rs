//! One offer issues one encrypted Nod: its nominal, its load and its terms.

#![deny(clippy::float_arithmetic)]

use alloy_primitives::{Address, U256};
use outbe_core_common::nod_encryption::{EncryptedNodV1, NodTermsV1};
use outbe_core_common::series_id::SeriesId;
use outbe_tee::resident::StateKeyHandle;
use serde::{Deserialize, Serialize};

use super::compute::{compute_nominal, parse_canonical_amount};
use super::errors::{display_text, OfferError, OfferResult, ProcessError};
use super::protocol::{EncryptedTributeOffer, TributeZkExpectedHashes};
use super::zk_claim::derive_expected_hashes;
use super::{open_offer, OfferKeyMaterial};
use crate::constants::LOAD_RATE_PCT;
use outbe_core_common::confidential_tee::ComputeError;

/// An offer and the public terms its Nod gets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueNodAtOfferRequestV1 {
    pub chain_id: u64,
    pub offer: EncryptedTributeOffer,
    /// Emission day of the series, `YYYYMMDD`.
    pub emission_day: u32,
    /// 24-hour VWAP of the reference currency at the offer hour.
    pub entry_price_minor: U256,
}

/// Outcome of one offer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodAtOfferStatusV1 {
    Issued,
    /// The load rounds down to zero minor units.
    LoadBelowOneMinorUnit,
    Rejected {
        reason: String,
    },
}

/// Public result of one offer. Only an issued offer carries the Nod and the claims.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodAtOfferResultV1 {
    pub status: NodAtOfferStatusV1,
    pub nod: Option<EncryptedNodV1>,
    pub su_hashes: Vec<String>,
    pub wallet_addresses: Vec<String>,
    pub sra_addresses: Vec<String>,
    pub zk_expected_hashes: Option<TributeZkExpectedHashes>,
}

impl NodAtOfferResultV1 {
    fn refused(status: NodAtOfferStatusV1) -> Self {
        Self {
            status,
            nod: None,
            su_hashes: Vec::new(),
            wallet_addresses: Vec::new(),
            sra_addresses: Vec::new(),
            zk_expected_hashes: None,
        }
    }
}

/// `floor(nominal × 32 / 100)`, without overflow.
#[must_use]
pub fn nod_load_of(nominal: U256) -> U256 {
    let rate = U256::from(LOAD_RATE_PCT);
    let hundred = U256::from(100u64);
    (nominal / hundred) * rate + (nominal % hundred) * rate / hundred
}

/// Decrypt `request.offer`, price it and encrypt its Nod. A failed offer is not an error.
pub fn issue_nod_at_offer(
    key: &OfferKeyMaterial<'_>,
    nod_state_key: &StateKeyHandle,
    resident_chain_id: u64,
    request: &IssueNodAtOfferRequestV1,
) -> NodAtOfferResultV1 {
    issue(key, nod_state_key, resident_chain_id, request).unwrap_or_else(|err| {
        NodAtOfferResultV1::refused(NodAtOfferStatusV1::Rejected {
            reason: display_text(&err),
        })
    })
}

fn issue(
    key: &OfferKeyMaterial<'_>,
    nod_state_key: &StateKeyHandle,
    resident_chain_id: u64,
    request: &IssueNodAtOfferRequestV1,
) -> OfferResult<NodAtOfferResultV1> {
    let offer = &request.offer;
    if request.chain_id != resident_chain_id
        || offer
            .zk_context
            .as_ref()
            .is_some_and(|context| context.chain_id != resident_chain_id)
    {
        return Err(ProcessError::OfferProofChainMismatch.into());
    }
    let payload = open_offer(key, offer)?;
    let owner = payload
        .creator
        .parse::<Address>()
        .map_err(|_| OfferError::from(ProcessError::InvalidCreatorAddress))?;
    let amount = parse_canonical_amount(&payload.amount_base, &payload.amount_micro)?;
    if amount.amount_minor.is_zero() {
        return Err(ProcessError::AmountNotPositive.into());
    }
    let zk_expected_hashes = derive_expected_hashes(offer, &payload, &amount)?;
    let nominal = match compute_nominal(
        amount.amount_minor,
        offer.issuance_wwd_vwap_minor,
        offer.reference_wwd_vwap_minor,
        offer.reference_scurve_minor,
    ) {
        Ok((nominal, _)) => nominal,
        Err(ComputeError::NominalRoundsToZero) => U256::ZERO,
        Err(err) => return Err(err.into()),
    };
    let load = nod_load_of(nominal);
    if load.is_zero() {
        return Ok(NodAtOfferResultV1::refused(
            NodAtOfferStatusV1::LoadBelowOneMinorUnit,
        ));
    }
    let series_id = SeriesId::for_pair(
        request.emission_day,
        offer.tribute_currency,
        offer.reference_currency,
    )
    .map_err(|_| ProcessError::InvalidSeries)?;
    let terms = NodTermsV1 {
        chain_id: request.chain_id,
        nod_id: outbe_core_common::nod_rules::derive_nod_id(series_id, owner),
        owner,
        emission_day: request.emission_day,
        entry_price_minor: request.entry_price_minor,
        issuance_currency: offer.tribute_currency,
        reference_currency: offer.reference_currency,
    };
    let nod = crate::nod_encryption::encrypt_nod(nod_state_key, terms, load, nominal)
        .map_err(ProcessError::NodEncryptionFailed)?;
    Ok(NodAtOfferResultV1 {
        status: NodAtOfferStatusV1::Issued,
        nod: Some(nod),
        su_hashes: payload.su_hashes,
        wallet_addresses: payload.wallet_addresses,
        sra_addresses: payload.sra_addresses,
        zk_expected_hashes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_is_32_percent_of_the_nominal_rounded_down() {
        assert_eq!(nod_load_of(U256::from(100u64)), U256::from(32u64));
        assert_eq!(nod_load_of(U256::from(3u64)), U256::ZERO);
        assert_eq!(nod_load_of(U256::from(4u64)), U256::ONE);
        assert_eq!(
            nod_load_of(U256::from(1_000_003u64)),
            U256::from(320_000u64)
        );
        assert_eq!(
            nod_load_of(U256::MAX),
            U256::MAX / U256::from(100u64) * U256::from(32u64) + U256::from(11u64)
        );
    }
}
