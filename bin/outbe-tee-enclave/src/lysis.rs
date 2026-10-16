//! Lysis decrypts and calculates only inside the enclave.

use std::collections::BTreeMap;

use alloy_primitives::U256;
use outbe_lysis::program_v1::{
    execute_with_amounts, LysisArithmeticErrorV1, ObservationValueV1, ObservedTributeV1,
    ProgramErrorV1, ProgramInputV1, TributeInputV1,
};
use outbe_primitives::{nod_encryption::NodTermsV2, wwd_entity_id::derive_poseidon_entity_id};
use outbe_tee::lysis::{LysisNod, LysisRequest, LysisResult};

use crate::errors::{Result, TeeError};

fn rejected() -> TeeError {
    TeeError::TributeOfferReject("Lysis input or computation rejected".into())
}

fn calculation_error(error: ProgramErrorV1) -> TeeError {
    match error {
        ProgramErrorV1::ZeroGratisLoad { .. }
        | ProgramErrorV1::ZeroCost { .. }
        | ProgramErrorV1::EntryPriceUnavailable { .. }
        | ProgramErrorV1::Arithmetic {
            reason: LysisArithmeticErrorV1::NodEntryPriceOutOfBounds { .. },
        } => TeeError::LysisCalculationRejected,
        _ => rejected(),
    }
}

pub fn execute(
    secret: &[u8; 32],
    fidelity_key: &[u8; 32],
    request: &LysisRequest,
) -> Result<LysisResult> {
    if request.chain_id == 0
        || request.tributes.is_empty()
        || request.fidelity.timestamp != request.timestamp
        || request.fidelity.entries.len() != request.tributes.len()
        || request
            .tributes
            .windows(2)
            .any(|pair| pair[0].context.tribute_id >= pair[1].context.tribute_id)
    {
        return Err(rejected());
    }
    let prices = request
        .entry_prices
        .iter()
        .copied()
        .collect::<BTreeMap<_, _>>();
    if prices.len() != request.entry_prices.len() {
        return Err(rejected());
    }
    let leagues = crate::fidelity::snapshot_leagues(fidelity_key, &request.fidelity)
        .map_err(|_| rejected())?;
    let mut observed = Vec::with_capacity(request.tributes.len());
    for ((tribute, fidelity), entry) in request
        .tributes
        .iter()
        .zip(leagues)
        .zip(&request.fidelity.entries)
    {
        let context = &tribute.context;
        if context.chain_id != request.chain_id
            || context.worldwide_day != request.worldwide_day
            || context.owner.is_zero()
            || fidelity.owner != context.owner
            || entry.owner != context.owner
            || derive_poseidon_entity_id(context.owner, context.worldwide_day)
                .map_err(|_| rejected())?
                != context.tribute_id
        {
            return Err(rejected());
        }
        let amounts =
            crate::tribute_encryption::decrypt_tribute(secret, tribute).map_err(|_| rejected())?;
        let price = prices
            .get(&context.reference_currency)
            .ok_or_else(rejected)?;
        observed.push(ObservedTributeV1 {
            tribute: TributeInputV1 {
                tribute_id: context.tribute_id,
                owner: context.owner,
                worldwide_day: context.worldwide_day,
                issuance_currency: context.issuance_currency,
                nominal_amount_minor: amounts.nominal_amount_minor,
                reference_currency: context.reference_currency,
                tribute_price_minor: context.tribute_price_minor,
            },
            first_league: ObservationValueV1::Value(fidelity.league),
            second_league: ObservationValueV1::Value(fidelity.league),
            entry_price_minor: price
                .map_or(ObservationValueV1::Unavailable, ObservationValueV1::Value),
            nod_target_available: true,
        });
    }
    let (result, loads) = execute_with_amounts(ProgramInputV1 {
        worldwide_day: request.worldwide_day,
        logical_evaluation_time: request.timestamp,
        lysis_limit_minor: request.lysis_limit_minor,
        tributes: observed,
    })
    .map_err(calculation_error)?;
    let mut nods = Vec::with_capacity(result.nod_actions.len());
    for ((action, load), tribute) in result.nod_actions.iter().zip(loads).zip(&request.tributes) {
        let settlement_cost_minor =
            outbe_primitives::nod_rules::settlement_cost_minor(action.entry_price_minor, load)
                .map_err(|_| rejected())?;
        let nod = crate::nod_encryption::encrypt_nod_for_tribute(
            secret,
            tribute,
            NodTermsV2 {
                chain_id: request.chain_id,
                nod_id: action.nod_id,
                owner: action.owner,
                worldwide_day: action.worldwide_day,
                league_id: action.league_id,
                entry_price_minor: action.entry_price_minor,
                issuance_currency: action.issuance_currency,
                reference_currency: action.reference_currency,
            },
            load,
        )
        .map_err(|_| rejected())?;
        nods.push(LysisNod {
            nod,
            settlement_cost_minor,
        });
    }
    if result.total_nominal.is_zero() || request.lysis_limit_minor == U256::ZERO {
        return Err(rejected());
    }
    Ok(LysisResult {
        nods,
        total_nominal_minor: result.total_nominal,
        unused_lysis_limit_minor: result.remaining_lysis_limit_minor,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloy_primitives::{Address, B256};
    use outbe_primitives::{
        time::WorldwideDay,
        tribute_encryption::{TributeAmountsV2, TributeContextV2},
    };
    use outbe_tee::protocol::{FidelitySnapshotEntry, FidelitySnapshotRequest};

    const SECRET: [u8; 32] = [0x5a; 32];
    const OWNER_SECRET: [u8; 32] = [0x37; 32];
    const FIDELITY_KEY: [u8; 32] = [0x63; 32];

    pub(crate) fn fixture() -> LysisRequest {
        let day = WorldwideDay::new(20261015);
        let timestamp = day.start_timestamp() + 3600;
        let creator = crate::crypto::x25519_public(&OWNER_SECRET);
        let mut tributes = Vec::new();
        for (index, nominal, currency) in [
            (1, 1_234_u64, 840),
            (2, 7_654_321, 978),
            (3, 987_654_321, 840),
        ] {
            let owner = Address::repeat_byte(index);
            tributes.push(
                crate::tribute_encryption::encrypt_tribute(
                    &SECRET,
                    &creator,
                    TributeContextV2 {
                        chain_id: 1,
                        tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
                        owner,
                        worldwide_day: day,
                        issuance_currency: 840,
                        reference_currency: currency,
                        tribute_price_minor: U256::from(2_000_000),
                        offer_input_hash: B256::repeat_byte(index),
                    },
                    &TributeAmountsV2 {
                        nominal_amount_minor: U256::from(nominal),
                        issuance_amount_minor: U256::from(nominal * 2),
                    },
                )
                .unwrap(),
            );
        }
        tributes.sort_by_key(|tribute| tribute.context.tribute_id);
        let fidelity = FidelitySnapshotRequest {
            timestamp,
            first_qualified_start: 0,
            entries: tributes
                .iter()
                .map(|tribute| FidelitySnapshotEntry {
                    owner: tribute.context.owner,
                    cohort_blob: vec![],
                })
                .collect(),
        };
        LysisRequest {
            chain_id: 1,
            worldwide_day: day,
            timestamp,
            lysis_limit_minor: U256::from(1_000_000),
            tributes,
            fidelity,
            entry_prices: vec![
                (840, Some(U256::from(250_000))),
                (978, Some(U256::from(125_001))),
            ],
        }
    }

    #[test]
    fn ready_nods_preserve_ecdh_access_and_exact_sequential_math() {
        let request = fixture();
        let output = execute(&SECRET, &FIDELITY_KEY, &request).unwrap();
        let leagues = crate::fidelity::snapshot_leagues(&FIDELITY_KEY, &request.fidelity).unwrap();
        let observed = request
            .tributes
            .iter()
            .zip(leagues)
            .map(|(tribute, league)| {
                let context = &tribute.context;
                ObservedTributeV1 {
                    tribute: TributeInputV1 {
                        tribute_id: context.tribute_id,
                        owner: context.owner,
                        worldwide_day: context.worldwide_day,
                        issuance_currency: context.issuance_currency,
                        reference_currency: context.reference_currency,
                        tribute_price_minor: context.tribute_price_minor,
                        nominal_amount_minor: crate::tribute_encryption::decrypt_tribute(
                            &SECRET, tribute,
                        )
                        .unwrap()
                        .nominal_amount_minor,
                    },
                    first_league: ObservationValueV1::Value(league.league),
                    second_league: ObservationValueV1::Value(league.league),
                    entry_price_minor: ObservationValueV1::Value(
                        request
                            .entry_prices
                            .iter()
                            .find(|(currency, _)| *currency == context.reference_currency)
                            .unwrap()
                            .1
                            .unwrap(),
                    ),
                    nod_target_available: true,
                }
            })
            .collect();
        let (expected, loads) = execute_with_amounts(ProgramInputV1 {
            worldwide_day: request.worldwide_day,
            logical_evaluation_time: request.timestamp,
            lysis_limit_minor: request.lysis_limit_minor,
            tributes: observed,
        })
        .unwrap();
        assert_eq!(output.total_nominal_minor, expected.total_nominal);
        assert_eq!(
            output.unused_lysis_limit_minor,
            expected.remaining_lysis_limit_minor
        );
        let enclave_public = crate::crypto::x25519_public(&SECRET);
        for (output, expected_load) in output.nods.iter().zip(loads) {
            let enclave_load = crate::nod_encryption::decrypt_nod(&SECRET, &output.nod).unwrap();
            let owner_load = outbe_tee::nod_decrypt::decrypt_nod_for_owner(
                &OWNER_SECRET,
                &enclave_public,
                &output.nod,
            )
            .unwrap();
            assert_eq!(enclave_load, expected_load);
            assert_eq!(owner_load, expected_load);
            assert_eq!(
                output.settlement_cost_minor,
                outbe_primitives::nod_rules::settlement_cost_minor(
                    output.nod.terms.entry_price_minor,
                    expected_load
                )
                .unwrap()
            );
        }
        let restarted_secret = SECRET;
        assert_eq!(
            output,
            execute(&restarted_secret, &FIDELITY_KEY, &request).unwrap()
        );
    }

    #[test]
    fn invalid_sources_and_observations_are_rejected_without_private_error_values() {
        for fault in 0..8 {
            let mut request = fixture();
            match fault {
                0 => request.tributes[0].encrypted_amounts[20] ^= 1,
                1 => request.tributes[0].context.chain_id += 1,
                2 => request.tributes.swap(0, 1),
                3 => request.fidelity.entries[0].owner = Address::ZERO,
                4 => request.fidelity.timestamp += 1,
                5 => request.entry_prices.clear(),
                6 => request.entry_prices[0].1 = None,
                7 => request.lysis_limit_minor = U256::ONE,
                _ => unreachable!(),
            }
            let error = execute(&SECRET, &FIDELITY_KEY, &request)
                .unwrap_err()
                .to_string();
            assert_eq!(
                error,
                if fault >= 6 {
                    TeeError::LysisCalculationRejected.to_string()
                } else {
                    rejected().to_string()
                }
            );
            assert!(!error.contains("7654321"));
            assert!(!error.contains("987654321"));
        }
    }

    #[test]
    fn calculation_rejection_requires_authenticating_every_source() {
        for corrupt_fidelity in [false, true] {
            let mut request = fixture();
            request.lysis_limit_minor = U256::ONE;
            if corrupt_fidelity {
                request.fidelity.entries.last_mut().unwrap().cohort_blob = vec![0xff; 32];
            } else {
                request.tributes.last_mut().unwrap().encrypted_amounts[20] ^= 1;
            }
            assert_eq!(
                execute(&SECRET, &FIDELITY_KEY, &request)
                    .unwrap_err()
                    .to_string(),
                rejected().to_string()
            );
        }
    }
}
