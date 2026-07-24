//! Pure bounded phase functions used by the fixed Lysis V1 work graph.

use std::collections::BTreeMap;

use alloy_primitives::U256;
use outbe_compressed_entities::EntityId36;

use super::{
    execute::compute_fraction_map_from_groups, FidelityPhaseV1, LeagueFractionV1,
    ObservedTributeV1, ProgramErrorV1,
};

use super::planner::PRIMARY_WORK_SHARD_SIZE;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FidelityObservationV1 {
    pub raw_ordinal: u32,
    pub tribute_id: EntityId36,
    pub pre_distribution_league: u16,
    pub issuance_league: u16,
    pub nominal_amount_minor: U256,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FidelityLeaguePartialV1 {
    pub league_id: u16,
    pub count: u32,
    pub nominal_amount_minor: U256,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FidelityAggregateV1 {
    pub start_ordinal: u32,
    pub end_ordinal: u32,
    pub tribute_count: u32,
    pub checked_total_nominal: U256,
    pub ordered_league_partials: Vec<FidelityLeaguePartialV1>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FidelityMapOutputV1 {
    pub observations: Vec<FidelityObservationV1>,
    pub aggregate: FidelityAggregateV1,
}

pub fn fidelity_map(
    start_ordinal: u32,
    observed: &[ObservedTributeV1],
) -> Result<FidelityMapOutputV1, ProgramErrorV1> {
    if observed.is_empty()
        || observed.len()
            > usize::try_from(PRIMARY_WORK_SHARD_SIZE)
                .map_err(|_| ProgramErrorV1::OutputCountMismatch)?
    {
        return Err(ProgramErrorV1::OutputCountMismatch);
    }
    let count =
        u32::try_from(observed.len()).map_err(|_| ProgramErrorV1::OutputCountMismatch)?;
    let end_ordinal = start_ordinal
        .checked_add(count)
        .ok_or(ProgramErrorV1::OutputCountMismatch)?;
    let mut observations = Vec::with_capacity(observed.len());
    let mut groups = BTreeMap::<u16, (u32, U256)>::new();
    let mut checked_total_nominal = U256::ZERO;

    for (local_ordinal, item) in observed.iter().enumerate() {
        let raw_ordinal = start_ordinal
            .checked_add(
                u32::try_from(local_ordinal).map_err(|_| ProgramErrorV1::OutputCountMismatch)?,
            )
            .ok_or(ProgramErrorV1::OutputCountMismatch)?;
        let first = item
            .first_league
            .copied()
            .ok_or(ProgramErrorV1::FidelityUnavailable {
                ordinal: raw_ordinal as usize,
                phase: FidelityPhaseV1::First,
            })?;
        let second = item
            .second_league
            .copied()
            .ok_or(ProgramErrorV1::FidelityUnavailable {
                ordinal: raw_ordinal as usize,
                phase: FidelityPhaseV1::Second,
            })?;
        if first != second {
            return Err(ProgramErrorV1::FidelityMismatch {
                ordinal: raw_ordinal as usize,
                first,
                second,
            });
        }
        checked_total_nominal = checked_total_nominal
            .checked_add(item.tribute.nominal_amount_minor)
            .ok_or(ProgramErrorV1::TotalNominalOverflow {
                ordinal: raw_ordinal as usize,
            })?;
        let group = groups.entry(first).or_insert((0, U256::ZERO));
        group.0 = group
            .0
            .checked_add(1)
            .ok_or_else(|| ProgramErrorV1::Arithmetic {
                message: "Fidelity league population overflow".to_owned(),
            })?;
        group.1 = group
            .1
            .checked_add(item.tribute.nominal_amount_minor)
            .ok_or(ProgramErrorV1::TotalNominalOverflow {
                ordinal: raw_ordinal as usize,
            })?;
        observations.push(FidelityObservationV1 {
            raw_ordinal,
            tribute_id: item.tribute.tribute_id,
            pre_distribution_league: first,
            issuance_league: second,
            nominal_amount_minor: item.tribute.nominal_amount_minor,
        });
    }

    Ok(FidelityMapOutputV1 {
        observations,
        aggregate: FidelityAggregateV1 {
            start_ordinal,
            end_ordinal,
            tribute_count: count,
            checked_total_nominal,
            ordered_league_partials: groups
                .into_iter()
                .map(
                    |(league_id, (count, nominal_amount_minor))| FidelityLeaguePartialV1 {
                        league_id,
                        count,
                        nominal_amount_minor,
                    },
                )
                .collect(),
        },
    })
}

pub fn fidelity_reduce(
    left: &FidelityAggregateV1,
    right: &FidelityAggregateV1,
) -> Result<FidelityAggregateV1, ProgramErrorV1> {
    if left.start_ordinal >= left.end_ordinal
        || right.start_ordinal >= right.end_ordinal
        || left.end_ordinal != right.start_ordinal
    {
        return Err(ProgramErrorV1::OutputCountMismatch);
    }
    let tribute_count = left
        .tribute_count
        .checked_add(right.tribute_count)
        .ok_or(ProgramErrorV1::OutputCountMismatch)?;
    let checked_total_nominal = left
        .checked_total_nominal
        .checked_add(right.checked_total_nominal)
        .ok_or(ProgramErrorV1::TotalNominalOverflow {
            ordinal: right.start_ordinal as usize,
        })?;
    let mut groups = BTreeMap::<u16, (u32, U256)>::new();
    for partial in left
        .ordered_league_partials
        .iter()
        .chain(&right.ordered_league_partials)
    {
        let group = groups
            .entry(partial.league_id)
            .or_insert((0, U256::ZERO));
        group.0 = group
            .0
            .checked_add(partial.count)
            .ok_or_else(|| ProgramErrorV1::Arithmetic {
                message: "Fidelity reducer population overflow".to_owned(),
            })?;
        group.1 = group
            .1
            .checked_add(partial.nominal_amount_minor)
            .ok_or(ProgramErrorV1::TotalNominalOverflow {
                ordinal: right.start_ordinal as usize,
            })?;
    }
    Ok(FidelityAggregateV1 {
        start_ordinal: left.start_ordinal,
        end_ordinal: right.end_ordinal,
        tribute_count,
        checked_total_nominal,
        ordered_league_partials: groups
            .into_iter()
            .map(
                |(league_id, (count, nominal_amount_minor))| FidelityLeaguePartialV1 {
                    league_id,
                    count,
                    nominal_amount_minor,
                },
            )
            .collect(),
    })
}

pub fn finalize_fi_fraction_table(
    aggregate: &FidelityAggregateV1,
    gratis_allocation: U256,
) -> Result<Vec<LeagueFractionV1>, ProgramErrorV1> {
    if aggregate.tribute_count == 0 || aggregate.checked_total_nominal.is_zero() {
        return Err(ProgramErrorV1::ZeroTotalNominal);
    }
    let groups = aggregate
        .ordered_league_partials
        .iter()
        .map(|partial| {
            (
                partial.league_id,
                (partial.count, partial.nominal_amount_minor),
            )
        })
        .collect::<BTreeMap<_, _>>();
    compute_fraction_map_from_groups(
        &groups,
        aggregate.tribute_count,
        aggregate.checked_total_nominal,
        gratis_allocation,
    )
    .map(|fractions| {
        fractions
            .into_iter()
            .map(|(league, fraction)| LeagueFractionV1 { league, fraction })
            .collect()
    })
}
