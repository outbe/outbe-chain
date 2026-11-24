//! Pure sequential Lysis V1 semantic program.

use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::HashMap;

use alloy_primitives::U256;
use outbe_core_common::wwd_entity_id::derive_poseidon_entity_id;
use outbe_system_common::math::scaled_math::checked_mul_div_floor;
use outbe_system_common::time::WorldwideDay;

use crate::algorithm::{compute_fraction_distribution_fp, SCALE};

use super::types::{
    FidelityPhaseV1, LeagueFractionV1, NodActionV1, OfferInputV1, ProgramErrorV1, ProgramInputV1,
    ProgramResultV1, SemanticObservationV1,
};

/// One load checked before the conditional Oracle/Fidelity reads at its ordinal.
pub(crate) struct PendingNodV1 {
    ordinal: usize,
    gratis_load_minor: U256,
    remaining_after: U256,
}

/// Stateful sequential reducer. It owns no storage and emits no chain effects.
pub(crate) struct ProgramExecutionV1 {
    offers: Vec<OfferInputV1>,
    first_leagues: Vec<u16>,
    fractions: BTreeMap<u16, U256>,
    total_nominal: U256,
    lysis_limit_minor: U256,
    next_ordinal: usize,
    remaining: U256,
    nod_actions: Vec<NodActionV1>,
    observations: Vec<SemanticObservationV1>,
}

/// Execute a complete pre-observed logical input.
pub fn execute(input: ProgramInputV1) -> Result<ProgramResultV1, ProgramErrorV1> {
    execute_with_amounts(input).map(|output| output.0)
}

/// Returns private loads for encryption inside the enclave.
/// The caller must not expose these loads across the enclave boundary.
pub fn execute_with_amounts(
    mut input: ProgramInputV1,
) -> Result<(ProgramResultV1, Vec<U256>), ProgramErrorV1> {
    input.offers.sort_by_key(|observed| observed.offer.offer_id);
    let offers = input
        .offers
        .iter()
        .map(|observed| observed.offer.clone())
        .collect::<Vec<_>>();
    if offers.is_empty() {
        return Err(ProgramErrorV1::EmptyInput);
    }
    validate_canonical_offers(input.worldwide_day, &offers)?;
    let mut first_leagues = Vec::with_capacity(input.offers.len());
    let mut observed_total = U256::ZERO;
    for (ordinal, observed) in input.offers.iter().enumerate() {
        let league = observed
            .first_league
            .copied()
            .ok_or(ProgramErrorV1::FidelityUnavailable {
                ordinal,
                phase: FidelityPhaseV1::First,
            })?;
        first_leagues.push(league);
        observed_total =
            checked_nominal_step(observed_total, observed.offer.nominal_amount_minor, ordinal)?;
    }
    if observed_total.is_zero() {
        return Err(ProgramErrorV1::ZeroTotalNominal);
    }
    let mut execution = prepare(
        input.worldwide_day,
        offers,
        first_leagues,
        input.lysis_limit_minor,
    )?;

    let mut loads = Vec::with_capacity(input.offers.len());
    for observed in &input.offers {
        let pending = execution.quote_next()?;
        loads.push(pending.gratis_load_minor);
        let ordinal = pending.ordinal;
        let entry_price =
            observed
                .entry_price_minor
                .copied()
                .ok_or(ProgramErrorV1::EntryPriceUnavailable {
                    ordinal,
                    currency: observed.offer.reference_currency,
                })?;
        let second_league =
            observed
                .second_league
                .copied()
                .ok_or(ProgramErrorV1::FidelityUnavailable {
                    ordinal,
                    phase: FidelityPhaseV1::Second,
                })?;
        execution.commit_next(
            pending,
            entry_price,
            second_league,
            observed.nod_target_available,
        )?;
    }
    Ok((execution.finish()?, loads))
}

/// Validate canonical input, derive the FI fraction map, and initialize the reducer.
pub(crate) fn prepare(
    worldwide_day: WorldwideDay,
    offers: Vec<OfferInputV1>,
    first_leagues: Vec<u16>,
    lysis_limit_minor: U256,
) -> Result<ProgramExecutionV1, ProgramErrorV1> {
    if offers.is_empty() {
        return Err(ProgramErrorV1::EmptyInput);
    }
    if offers.len() != first_leagues.len() {
        return Err(ProgramErrorV1::OutputCountMismatch);
    }

    validate_canonical_offers(worldwide_day, &offers)?;
    let mut total_nominal = U256::ZERO;
    let mut observations = Vec::with_capacity(offers.len());
    for (ordinal, (offer, league)) in offers.iter().zip(&first_leagues).enumerate() {
        total_nominal = checked_nominal_step(total_nominal, offer.nominal_amount_minor, ordinal)?;
        observations.push(SemanticObservationV1::Fidelity {
            ordinal,
            phase: FidelityPhaseV1::First,
            owner: offer.owner,
            league: *league,
        });
    }
    if total_nominal.is_zero() {
        return Err(ProgramErrorV1::ZeroTotalNominal);
    }

    let nominal_amounts = offers
        .iter()
        .map(|offer| offer.nominal_amount_minor)
        .collect::<Vec<_>>();
    let fractions = compute_fraction_map(
        &nominal_amounts,
        &first_leagues,
        total_nominal,
        lysis_limit_minor,
    )?;
    Ok(ProgramExecutionV1 {
        offers,
        first_leagues,
        fractions,
        total_nominal,
        lysis_limit_minor,
        next_ordinal: 0,
        remaining: lysis_limit_minor,
        nod_actions: Vec::new(),
        observations,
    })
}

impl ProgramExecutionV1 {
    /// Compute and limit-check the next Gratis load before external observations.
    pub(crate) fn quote_next(&self) -> Result<PendingNodV1, ProgramErrorV1> {
        let ordinal = self.next_ordinal;
        let offer = self
            .offers
            .get(ordinal)
            .ok_or(ProgramErrorV1::OutputCountMismatch)?;
        let fraction = self
            .fractions
            .get(&self.first_leagues[ordinal])
            .copied()
            .unwrap_or(U256::ZERO);
        let gratis_load_minor = compute_gratis_load(offer.nominal_amount_minor, fraction, ordinal)?;
        let remaining_after = validate_required_gratis(self.remaining, gratis_load_minor, ordinal)?;
        Ok(PendingNodV1 {
            ordinal,
            gratis_load_minor,
            remaining_after,
        })
    }

    /// Complete the next semantic action after its conditional observations.
    pub(crate) fn commit_next(
        &mut self,
        pending: PendingNodV1,
        entry_price_minor: U256,
        second_league: u16,
        nod_target_available: bool,
    ) -> Result<&NodActionV1, ProgramErrorV1> {
        let ordinal = self.next_ordinal;
        if pending.ordinal != ordinal {
            return Err(ProgramErrorV1::OutputCountMismatch);
        }
        let offer = &self.offers[ordinal];
        self.observations.push(SemanticObservationV1::Oracle {
            ordinal,
            currency: offer.reference_currency,
            entry_price_minor,
        });

        if !outbe_core_common::nod_rules::is_issuable_entry(entry_price_minor) {
            return Err(
                crate::errors::LysisArithmeticErrorV1::NodEntryPriceOutOfBounds { ordinal }.into(),
            );
        }
        let first_league = self.first_leagues[ordinal];
        self.observations.push(SemanticObservationV1::Fidelity {
            ordinal,
            phase: FidelityPhaseV1::Second,
            owner: offer.owner,
            league: second_league,
        });
        if first_league != second_league {
            return Err(ProgramErrorV1::FidelityMismatch {
                ordinal,
                first: first_league,
                second: second_league,
            });
        }
        compute_cost(entry_price_minor, pending.gratis_load_minor, ordinal)?;
        if !nod_target_available || offer.owner.is_zero() {
            return Err(ProgramErrorV1::InvalidNodTarget { ordinal });
        }
        let action = NodActionV1 {
            source_offer_id: offer.offer_id,
            owner: offer.owner,
            league_id: second_league,
            entry_price_minor,
            issuance_currency: offer.issuance_currency,
            reference_currency: offer.reference_currency,
        };

        self.remaining = pending.remaining_after;
        let action_index = self.nod_actions.len();
        self.nod_actions.push(action);
        self.next_ordinal += 1;
        Ok(&self.nod_actions[action_index])
    }

    /// Finish only after exactly one action per canonical Offer.
    pub(crate) fn finish(self) -> Result<ProgramResultV1, ProgramErrorV1> {
        if self.next_ordinal != self.offers.len() || self.nod_actions.len() != self.offers.len() {
            return Err(ProgramErrorV1::OutputCountMismatch);
        }
        Ok(ProgramResultV1 {
            offer_ids: self.offers.iter().map(|offer| offer.offer_id).collect(),
            total_nominal: self.total_nominal,
            lysis_limit_minor: self.lysis_limit_minor,
            remaining_lysis_limit_minor: self.remaining,
            league_fractions: self
                .fractions
                .iter()
                .map(|(league, fraction)| LeagueFractionV1 {
                    league: *league,
                    fraction: *fraction,
                })
                .collect(),
            nod_actions: self.nod_actions,
            observations: self.observations,
        })
    }
}

pub(crate) fn compute_fraction_map(
    nominal_amounts: &[U256],
    offer_fis: &[u16],
    total_interest: U256,
    lysis_limit_minor: U256,
) -> Result<BTreeMap<u16, U256>, ProgramErrorV1> {
    let mut fi_groups = BTreeMap::<u16, (u32, U256)>::new();
    for (ordinal, &league) in offer_fis.iter().enumerate() {
        let group = fi_groups.entry(league).or_insert((0, U256::ZERO));
        group.0 = group
            .0
            .checked_add(1)
            .ok_or(crate::errors::LysisArithmeticErrorV1::FidelityLeaguePopulationOverflow)?;
        group.1 = group
            .1
            .checked_add(nominal_amounts[ordinal])
            .ok_or(ProgramErrorV1::TotalNominalOverflow { ordinal })?;
    }
    compute_fraction_map_from_groups(
        &fi_groups,
        u32::try_from(nominal_amounts.len()).map_err(|_| ProgramErrorV1::OutputCountMismatch)?,
        total_interest,
        lysis_limit_minor,
    )
}

pub fn compute_fraction_map_from_groups(
    fi_groups: &BTreeMap<u16, (u32, U256)>,
    offer_count: u32,
    total_interest: U256,
    lysis_limit_minor: U256,
) -> Result<BTreeMap<u16, U256>, ProgramErrorV1> {
    let sorted_fis = fi_groups.keys().copied().collect::<Vec<_>>();
    let mut shares = Vec::with_capacity(sorted_fis.len());
    let mut populations = Vec::with_capacity(sorted_fis.len());
    let mut group_nominals = Vec::with_capacity(sorted_fis.len());
    for league in &sorted_fis {
        let (population, group_nominal) = fi_groups[league];
        shares.push(scaled_floor(group_nominal, SCALE, total_interest)?);
        populations.push(u64::from(population));
        group_nominals.push(group_nominal);
    }
    let share_sum = shares.iter().copied().try_fold(U256::ZERO, |sum, share| {
        sum.checked_add(share).ok_or_else(|| {
            ProgramErrorV1::from(crate::errors::LysisArithmeticErrorV1::FidelityShareSumOverflow)
        })
    })?;
    if let Some(last) = shares.last_mut() {
        if share_sum < SCALE {
            *last = last
                .checked_add(SCALE - share_sum)
                .ok_or(crate::errors::LysisArithmeticErrorV1::FidelityShareRemainderOverflow)?;
        }
    }
    let f = scaled_floor(lysis_limit_minor, SCALE, total_interest)?;
    let fmax = f
        .checked_mul(U256::from(2_u8))
        .ok_or(crate::errors::LysisArithmeticErrorV1::MaximumGratisFractionOverflow)?;
    // The distribution prioritizes its first group: highest league first.
    shares.reverse();
    populations.reverse();
    let mut fractions = compute_fraction_distribution_fp(
        &shares,
        &populations,
        usize::try_from(offer_count).map_err(|_| ProgramErrorV1::OutputCountMismatch)?,
        f,
        fmax,
    )
    .map_err(ProgramErrorV1::math)?;
    // Match the ascending league IDs and nominals for projection and output.
    fractions.reverse();

    // Six-decimal share rounding can make the real group projection exceed the
    // allocation even when the normalized share-space projection does not. Own
    // the one real-amount normalization here so every Lysis execution consumes
    // exactly the same fraction map.
    let projected = group_nominals.iter().zip(&fractions).try_fold(
        U256::ZERO,
        |sum, (nominal, fraction)| {
            let load = scaled_floor(*nominal, *fraction, SCALE)?;
            sum.checked_add(load).ok_or_else(|| {
                ProgramErrorV1::from(
                    crate::errors::LysisArithmeticErrorV1::ProjectedGratisAllocationOverflow,
                )
            })
        },
    )?;
    if projected > lysis_limit_minor {
        for fraction in &mut fractions {
            *fraction = scaled_floor(*fraction, lysis_limit_minor, projected)?;
        }
    }
    Ok(sorted_fis.into_iter().zip(fractions).collect())
}

fn scaled_floor(
    numerator: U256,
    multiplier: U256,
    denominator: U256,
) -> Result<U256, ProgramErrorV1> {
    checked_mul_div_floor(numerator, multiplier, denominator).map_err(ProgramErrorV1::math)
}

pub fn compute_gratis_load(
    nominal_amount_minor: U256,
    fraction: U256,
    ordinal: usize,
) -> Result<U256, ProgramErrorV1> {
    let load = scaled_floor(nominal_amount_minor, fraction, SCALE)?;
    if load.is_zero() {
        return Err(ProgramErrorV1::ZeroGratisLoad { ordinal });
    }
    Ok(load)
}

pub(crate) fn compute_cost(
    entry_price_minor: U256,
    gratis_load_minor: U256,
    ordinal: usize,
) -> Result<U256, ProgramErrorV1> {
    // Use the same minor-unit minimum as Nod reads and settlement.
    let cost =
        outbe_core_common::nod_rules::settlement_cost_minor(entry_price_minor, gratis_load_minor)
            .map_err(ProgramErrorV1::math)?;
    if cost.is_zero() {
        return Err(ProgramErrorV1::ZeroCost {
            ordinal,
            entry_price_minor,
            gratis_load_minor,
        });
    }
    Ok(cost)
}

pub(crate) fn validate_canonical_offers(
    worldwide_day: WorldwideDay,
    offers: &[OfferInputV1],
) -> Result<(), ProgramErrorV1> {
    let mut previous = None;
    for (ordinal, offer) in offers.iter().enumerate() {
        if offer.worldwide_day != worldwide_day || offer.offer_id.worldwide_day() != worldwide_day {
            return Err(ProgramErrorV1::WorldwideDayMismatch { ordinal });
        }
        if let Some(previous) = previous {
            if offer.offer_id == previous {
                return Err(ProgramErrorV1::DuplicateInput { ordinal });
            }
            if offer.offer_id < previous {
                return Err(ProgramErrorV1::NonCanonicalInput { ordinal });
            }
        }
        previous = Some(offer.offer_id);
        let expected = derive_poseidon_entity_id(offer.owner, offer.worldwide_day);
        if offer.offer_id != expected {
            return Err(ProgramErrorV1::InvalidOfferIdentity { ordinal });
        }
    }
    Ok(())
}

pub(crate) fn checked_nominal_step(
    total: U256,
    nominal_amount_minor: U256,
    ordinal: usize,
) -> Result<U256, ProgramErrorV1> {
    total
        .checked_add(nominal_amount_minor)
        .ok_or(ProgramErrorV1::TotalNominalOverflow { ordinal })
}

pub(crate) fn validate_required_gratis(
    remaining: U256,
    gratis_load_minor: U256,
    ordinal: usize,
) -> Result<U256, ProgramErrorV1> {
    if gratis_load_minor.is_zero() {
        return Err(ProgramErrorV1::ZeroGratisLoad { ordinal });
    }
    if gratis_load_minor > remaining {
        return Err(ProgramErrorV1::GratisLoadExceedsRemaining { ordinal });
    }
    Ok(remaining - gratis_load_minor)
}

/// Compatibility helper for existing unit tests while the implementation
/// authority lives in `program_v1`.
#[cfg(test)]
pub(crate) fn compute_fraction_hash_map(
    nominal_amounts: &[U256],
    offer_fis: &[u16],
    total_interest: U256,
    lysis_limit_minor: U256,
) -> Result<HashMap<u16, U256>, ProgramErrorV1> {
    compute_fraction_map(
        nominal_amounts,
        offer_fis,
        total_interest,
        lysis_limit_minor,
    )
    .map(|fractions| fractions.into_iter().collect())
}

#[cfg(test)]
mod tests;
