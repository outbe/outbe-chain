//! Tribute-to-NOD issuance. `encrypt_nod` itself stays in `outbe_nod`.

use alloy_primitives::U256;
use outbe_core_common::nod_encryption::{EncryptedNodV1, NodTermsV1};
use outbe_core_common::tribute_day_encryption::EncryptedTributeDayFractionsV1;
use outbe_core_common::tribute_encryption::EncryptedTributeV2;
use outbe_tee::resident::{OfferKeyHandle, StateKeyHandle};
use serde::{Deserialize, Serialize};

use super::errors::{display_text, EngineError, EngineResult};
use super::tribute_day::read_day_fractions;
use super::tribute_encryption::decrypt_tribute;

/// Request to issue an encrypted NOD from a Tribute and settled day fractions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IssueNodFromTributeRequestV1 {
    /// Source encrypted Tribute record.
    pub tribute: EncryptedTributeV2,
    /// Settled encrypted day-fractions record for `tribute.context.worldwide_day`.
    pub day_fractions: EncryptedTributeDayFractionsV1,
    /// Public NOD terms for the issued asset.
    pub terms: NodTermsV1,
}

/// Outcome of [`issue_nod_from_tribute`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum IssueNodFromTributeOutcomeV1 {
    /// Encrypted NOD issued with positive settlement cost.
    Issued(EncryptedNodV1),
    /// Deterministic Lysis arithmetic error message.
    LysisArithmetic(String),
    /// Computed settlement cost rounded down to zero.
    ZeroSettlementCost,
}

const NOD_SOURCE_MISMATCH: &str = "NOD source terms mismatch";

/// Bind a newly issued NOD to an encrypted Tribute after the source check.
pub fn encrypt_nod_for_tribute(
    tribute_state_key: &StateKeyHandle,
    nod_state_key: &StateKeyHandle,
    tribute: &EncryptedTributeV2,
    terms: NodTermsV1,
    amount: U256,
) -> EngineResult<EncryptedNodV1> {
    if !terms_match(tribute, &terms) {
        return Err(EngineError::OfferRejected {
            reason: NOD_SOURCE_MISMATCH,
        });
    }
    let _ = decrypt_tribute(tribute_state_key, tribute)?;
    Ok(outbe_nod::nod_encryption::encrypt_nod(
        nod_state_key,
        terms,
        amount,
    )?)
}

/// Issue an encrypted NOD from a Tribute and the settled day fractions.
pub fn issue_nod_from_tribute(
    network_secret: &OfferKeyHandle,
    tribute_state_key: &StateKeyHandle,
    nod_state_key: &StateKeyHandle,
    request: &IssueNodFromTributeRequestV1,
) -> EngineResult<IssueNodFromTributeOutcomeV1> {
    let source = &request.tribute.context;
    let terms = &request.terms;
    let fractions = &request.day_fractions;
    let fractions_of_this_day = fractions.chain_id == terms.chain_id
        && fractions.worldwide_day == source.worldwide_day
        && fractions.emission_day.value() == terms.emission_day;
    if !terms_match(&request.tribute, terms) || !fractions_of_this_day {
        return Err(EngineError::OfferRejected {
            reason: NOD_SOURCE_MISMATCH,
        });
    }
    let amounts = decrypt_tribute(tribute_state_key, &request.tribute)?;
    let fractions = read_day_fractions(network_secret, &request.day_fractions)?;
    let fraction = U256::from(fractions.get(&terms.league_id.max(1)).copied().unwrap_or(0));
    let gratis_load_minor = match outbe_lysis::program_v1::calculate_gratis_load(
        amounts.nominal_amount_minor,
        fraction,
        0,
    ) {
        Ok(load) => load,
        Err(err) => {
            return Ok(IssueNodFromTributeOutcomeV1::LysisArithmetic(display_text(
                &err,
            )))
        }
    };
    let cost = settlement_cost(terms.entry_price_minor, gratis_load_minor)?;
    if cost.is_zero() {
        return Ok(IssueNodFromTributeOutcomeV1::ZeroSettlementCost);
    }
    let nod =
        outbe_nod::nod_encryption::encrypt_nod(nod_state_key, terms.clone(), gratis_load_minor)?;
    Ok(IssueNodFromTributeOutcomeV1::Issued(nod))
}

fn settlement_cost(entry: U256, load: U256) -> EngineResult<U256> {
    outbe_core_common::settlement::settlement_cost_units(
        &outbe_core_common::settlement::SettlementCost {
            entry_price_minor: entry,
            load_minor: load,
            units: U256::ONE,
        },
        None,
        outbe_system_common::units::PROTOCOL_AMOUNT_DECIMALS,
    )
    .map_err(EngineError::SettlementOverflow)
}

fn terms_match(tribute: &EncryptedTributeV2, terms: &NodTermsV1) -> bool {
    let source = &tribute.context;
    let source_terms = (
        source.chain_id,
        source.owner,
        source.issuance_currency,
        source.reference_currency,
    );
    let issued_terms = (
        terms.chain_id,
        terms.owner,
        terms.issuance_currency,
        terms.reference_currency,
    );
    let canonical_id = terms
        .series_id()
        .map(|series_id| outbe_core_common::nod_rules::derive_nod_id(series_id, terms.owner));
    issued_terms == source_terms && canonical_id == Ok(terms.nod_id)
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn settlement_overflow_keeps_the_historical_display_and_the_typed_source() {
        let error = settlement_cost(U256::MAX, U256::from(2)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "offer rejected: settlement cost overflow"
        );
        assert!(error.source().is_some());
        assert_eq!(
            error.source().unwrap().to_string(),
            "settlement conversion overflow"
        );
    }
}
