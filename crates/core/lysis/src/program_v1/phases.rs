//! Pure bounded phase functions used by the fixed Lysis V1 work graph.

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{derive_poseidon_entity_id, WwdEntityId};
use outbe_ocomp_protocol::result::NodActionV1 as CertifiedNodActionV1;
use outbe_primitives::time::WorldwideDay;

use super::{
    execute::{calculate_cost, calculate_gratis_load, compute_fraction_map_from_groups},
    FidelityPhaseV1, LeagueFractionV1, NodActionV1, ObservedTributeV1, ProgramErrorV1,
};

use super::planner::PRIMARY_WORK_SHARD_SIZE;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FidelityObservationV1 {
    pub raw_ordinal: u32,
    pub tribute_id: WwdEntityId,
    pub pre_distribution_league: u16,
    pub issuance_league: u16,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AmountRecordV1 {
    pub raw_ordinal: u32,
    pub tribute_id: WwdEntityId,
    pub owner: Address,
    pub worldwide_day: WorldwideDay,
    pub league_id: u16,
    pub gratis_fraction_fp: U256,
    pub entry_price_minor: U256,
    pub issuance_currency: u16,
    pub reference_currency: u16,
}

struct AmountMathV1 {
    record: AmountRecordV1,
    nominal_amount_minor: U256,
    gratis_load_minor: U256,
    settlement_cost_minor: U256,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AmountRunV1 {
    pub start_ordinal: u32,
    pub end_ordinal: u32,
    pub ordered_records: Vec<AmountRecordV1>,
    pub checked_segment_gratis_total: U256,
    pub checked_segment_nominal_total: U256,
    pub checked_segment_settlement_cost_total: U256,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedOutputRecordV1 {
    pub raw_ordinal: u32,
    pub nod_action: NodActionV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedOutputRunV1 {
    pub start_ordinal: u32,
    pub end_ordinal: u32,
    pub ordered_records: Vec<FinalizedOutputRecordV1>,
    pub checked_tribute_nominal_total: U256,
    pub checked_segment_gratis_total: U256,
    pub checked_settlement_cost_total: U256,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BucketRecordV1 {
    pub bucket_key: B256,
    pub raw_ordinal: u32,
    pub tribute_id: WwdEntityId,
    pub nod_id: WwdEntityId,
}

/// Key of the shuffle bucket a Nod action sorts into: its day, entry price and currency.
pub trait NodBucketKeyV1 {
    fn bucket_key(&self) -> B256;
}

impl NodBucketKeyV1 for NodActionV1 {
    fn bucket_key(&self) -> B256 {
        outbe_nod::identity::bucket_key(
            self.worldwide_day,
            self.entry_price_minor,
            self.reference_currency,
        )
    }
}

impl NodBucketKeyV1 for CertifiedNodActionV1 {
    fn bucket_key(&self) -> B256 {
        outbe_nod::identity::bucket_key(
            WorldwideDay::new(self.wwd),
            self.entry_price_minor,
            self.reference_currency,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BucketOrderedRunV1 {
    pub run_ordinal: u32,
    pub ordered_records: Vec<BucketRecordV1>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FidelityReduceValueV1 {
    Empty,
    Aggregate(FidelityAggregateV1),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GratisSegmentSummaryV1 {
    pub start_ordinal: u32,
    pub end_ordinal: u32,
    pub checked_segment_gratis_total: U256,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GratisSummaryValueV1 {
    Empty,
    Summary(GratisSegmentSummaryV1),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GratisIncomingV1 {
    pub start_ordinal: u32,
    pub end_ordinal: u32,
    pub incoming_remaining: Option<U256>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GratisLeafPrefixV1 {
    pub segment_ordinal: u32,
    pub incoming_remaining: U256,
    pub outgoing_remaining: U256,
    pub first_error_ordinal: Option<u32>,
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
    let count = u32::try_from(observed.len()).map_err(|_| ProgramErrorV1::OutputCountMismatch)?;
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
        let group = groups.entry(partial.league_id).or_insert((0, U256::ZERO));
        group.0 = group
            .0
            .checked_add(partial.count)
            .ok_or_else(|| ProgramErrorV1::Arithmetic {
                message: "Fidelity reducer population overflow".to_owned(),
            })?;
        group.1 = group.1.checked_add(partial.nominal_amount_minor).ok_or(
            ProgramErrorV1::TotalNominalOverflow {
                ordinal: right.start_ordinal as usize,
            },
        )?;
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

pub fn fidelity_reduce_pair(
    left: FidelityReduceValueV1,
    right: FidelityReduceValueV1,
) -> Result<FidelityReduceValueV1, ProgramErrorV1> {
    match (left, right) {
        (FidelityReduceValueV1::Empty, FidelityReduceValueV1::Empty) => {
            Ok(FidelityReduceValueV1::Empty)
        }
        (FidelityReduceValueV1::Aggregate(left), FidelityReduceValueV1::Empty) => {
            Ok(FidelityReduceValueV1::Aggregate(left))
        }
        (FidelityReduceValueV1::Aggregate(left), FidelityReduceValueV1::Aggregate(right)) => {
            fidelity_reduce(&left, &right).map(FidelityReduceValueV1::Aggregate)
        }
        (FidelityReduceValueV1::Empty, FidelityReduceValueV1::Aggregate(_)) => {
            Err(ProgramErrorV1::OutputCountMismatch)
        }
    }
}

pub fn finalize_fi_fraction_table(
    aggregate: &FidelityAggregateV1,
    lysis_limit_minor: U256,
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
        lysis_limit_minor,
    )
    .map(|fractions| {
        fractions
            .into_iter()
            .map(|(league, fraction)| LeagueFractionV1 { league, fraction })
            .collect()
    })
}

pub fn amount_map(
    start_ordinal: u32,
    observed: &[ObservedTributeV1],
    fidelity_observations: &[FidelityObservationV1],
    fractions: &[LeagueFractionV1],
) -> Result<AmountRunV1, ProgramErrorV1> {
    if observed.is_empty()
        || observed.len() != fidelity_observations.len()
        || observed.len()
            > usize::try_from(PRIMARY_WORK_SHARD_SIZE)
                .map_err(|_| ProgramErrorV1::OutputCountMismatch)?
    {
        return Err(ProgramErrorV1::OutputCountMismatch);
    }
    let fraction_by_league = fractions_by_league(fractions)?;

    let mut ordered_records = Vec::with_capacity(observed.len());
    let mut checked_segment_gratis_total = U256::ZERO;
    let mut checked_segment_nominal_total = U256::ZERO;
    let mut checked_segment_settlement_cost_total = U256::ZERO;
    for (local_ordinal, (item, fidelity)) in observed.iter().zip(fidelity_observations).enumerate()
    {
        let raw_ordinal = start_ordinal
            .checked_add(
                u32::try_from(local_ordinal).map_err(|_| ProgramErrorV1::OutputCountMismatch)?,
            )
            .ok_or(ProgramErrorV1::OutputCountMismatch)?;
        let math = amount_record(raw_ordinal, item, fidelity, &fraction_by_league)?;
        checked_segment_gratis_total = checked_add_amount(
            checked_segment_gratis_total,
            math.gratis_load_minor,
            raw_ordinal,
            "Gratis total overflow",
        )?;
        checked_segment_nominal_total = checked_add_amount(
            checked_segment_nominal_total,
            math.nominal_amount_minor,
            raw_ordinal,
            "nominal total overflow",
        )?;
        checked_segment_settlement_cost_total = checked_add_amount(
            checked_segment_settlement_cost_total,
            math.settlement_cost_minor,
            raw_ordinal,
            "settlement cost total overflow",
        )?;
        ordered_records.push(math.record);
    }
    let count =
        u32::try_from(ordered_records.len()).map_err(|_| ProgramErrorV1::OutputCountMismatch)?;
    Ok(AmountRunV1 {
        start_ordinal,
        end_ordinal: start_ordinal
            .checked_add(count)
            .ok_or(ProgramErrorV1::OutputCountMismatch)?,
        ordered_records,
        checked_segment_gratis_total,
        checked_segment_nominal_total,
        checked_segment_settlement_cost_total,
    })
}

fn checked_add_amount(
    total: U256,
    amount: U256,
    raw_ordinal: u32,
    message: &str,
) -> Result<U256, ProgramErrorV1> {
    total
        .checked_add(amount)
        .ok_or_else(|| ProgramErrorV1::Arithmetic {
            message: format!("{message} at {raw_ordinal}"),
        })
}

fn fractions_by_league(
    fractions: &[LeagueFractionV1],
) -> Result<BTreeMap<u16, U256>, ProgramErrorV1> {
    let mut fraction_by_league = BTreeMap::new();
    let mut previous_league = None;
    for fraction in fractions {
        if previous_league.is_some_and(|previous| previous >= fraction.league) {
            return Err(ProgramErrorV1::OutputCountMismatch);
        }
        previous_league = Some(fraction.league);
        fraction_by_league.insert(fraction.league, fraction.fraction);
    }
    Ok(fraction_by_league)
}

fn amount_record(
    raw_ordinal: u32,
    item: &ObservedTributeV1,
    fidelity: &FidelityObservationV1,
    fraction_by_league: &BTreeMap<u16, U256>,
) -> Result<AmountMathV1, ProgramErrorV1> {
    if fidelity.raw_ordinal != raw_ordinal
        || fidelity.tribute_id != item.tribute.tribute_id
        || fidelity.pre_distribution_league != fidelity.issuance_league
    {
        return Err(ProgramErrorV1::OutputCountMismatch);
    }
    let first_league = item
        .first_league
        .copied()
        .ok_or(ProgramErrorV1::FidelityUnavailable {
            ordinal: raw_ordinal as usize,
            phase: FidelityPhaseV1::First,
        })?;
    let second_league = item
        .second_league
        .copied()
        .ok_or(ProgramErrorV1::FidelityUnavailable {
            ordinal: raw_ordinal as usize,
            phase: FidelityPhaseV1::Second,
        })?;
    if first_league != second_league {
        return Err(ProgramErrorV1::FidelityMismatch {
            ordinal: raw_ordinal as usize,
            first: first_league,
            second: second_league,
        });
    }
    if first_league != fidelity.pre_distribution_league || second_league != fidelity.issuance_league
    {
        return Err(ProgramErrorV1::OutputCountMismatch);
    }
    let fraction = fraction_by_league
        .get(&first_league)
        .copied()
        .unwrap_or(U256::ZERO);
    let gratis_load_minor = calculate_gratis_load(
        item.tribute.nominal_amount_minor,
        fraction,
        raw_ordinal as usize,
    )?;
    let entry_price_minor =
        item.entry_price_minor
            .copied()
            .ok_or(ProgramErrorV1::EntryPriceUnavailable {
                ordinal: raw_ordinal as usize,
                currency: item.tribute.reference_currency,
            })?;
    if !item.nod_target_available || item.tribute.owner.is_zero() {
        return Err(ProgramErrorV1::InvalidNodTarget {
            ordinal: raw_ordinal as usize,
        });
    }
    if !outbe_nod::pricing::is_issuable_entry(entry_price_minor) {
        return Err(ProgramErrorV1::Arithmetic {
            message: format!("Nod entry price out of bounds at {raw_ordinal}"),
        });
    }
    let settlement_cost_minor =
        calculate_cost(entry_price_minor, gratis_load_minor, raw_ordinal as usize)?;
    Ok(AmountMathV1 {
        record: AmountRecordV1 {
            raw_ordinal,
            tribute_id: item.tribute.tribute_id,
            owner: item.tribute.owner,
            worldwide_day: item.tribute.worldwide_day,
            league_id: second_league,
            gratis_fraction_fp: fraction,
            entry_price_minor,
            issuance_currency: item.tribute.issuance_currency,
            reference_currency: item.tribute.reference_currency,
        },
        nominal_amount_minor: item.tribute.nominal_amount_minor,
        gratis_load_minor,
        settlement_cost_minor,
    })
}

pub fn gratis_summary(
    start_ordinal: u32,
    record_count: usize,
    checked_segment_gratis_total: U256,
) -> Result<GratisSegmentSummaryV1, ProgramErrorV1> {
    if record_count == 0
        || record_count
            > usize::try_from(PRIMARY_WORK_SHARD_SIZE)
                .map_err(|_| ProgramErrorV1::OutputCountMismatch)?
        || checked_segment_gratis_total.is_zero()
    {
        return Err(ProgramErrorV1::OutputCountMismatch);
    }
    let count = u32::try_from(record_count).map_err(|_| ProgramErrorV1::OutputCountMismatch)?;
    let end_ordinal = start_ordinal
        .checked_add(count)
        .ok_or(ProgramErrorV1::OutputCountMismatch)?;
    Ok(GratisSegmentSummaryV1 {
        start_ordinal,
        end_ordinal,
        checked_segment_gratis_total,
    })
}

pub fn gratis_summary_reduce_pair(
    left: GratisSummaryValueV1,
    right: GratisSummaryValueV1,
) -> Result<GratisSummaryValueV1, ProgramErrorV1> {
    match (left, right) {
        (GratisSummaryValueV1::Empty, GratisSummaryValueV1::Empty) => {
            Ok(GratisSummaryValueV1::Empty)
        }
        (GratisSummaryValueV1::Summary(left), GratisSummaryValueV1::Empty) => {
            Ok(GratisSummaryValueV1::Summary(left))
        }
        (GratisSummaryValueV1::Empty, GratisSummaryValueV1::Summary(_)) => {
            Err(ProgramErrorV1::OutputCountMismatch)
        }
        (GratisSummaryValueV1::Summary(left), GratisSummaryValueV1::Summary(right)) => {
            if left.start_ordinal >= left.end_ordinal
                || right.start_ordinal >= right.end_ordinal
                || left.end_ordinal != right.start_ordinal
            {
                return Err(ProgramErrorV1::OutputCountMismatch);
            }
            let total = left
                .checked_segment_gratis_total
                .checked_add(right.checked_segment_gratis_total)
                .ok_or_else(|| ProgramErrorV1::Arithmetic {
                    message: format!("Gratis reducer total overflow at {}", right.start_ordinal),
                })?;
            Ok(GratisSummaryValueV1::Summary(GratisSegmentSummaryV1 {
                start_ordinal: left.start_ordinal,
                end_ordinal: right.end_ordinal,
                checked_segment_gratis_total: total,
            }))
        }
    }
}

pub fn gratis_prefix_down(
    parent_incoming: Option<U256>,
    left: GratisSummaryValueV1,
    right: GratisSummaryValueV1,
) -> Result<[Option<GratisIncomingV1>; 2], ProgramErrorV1> {
    match (left, right) {
        (GratisSummaryValueV1::Empty, GratisSummaryValueV1::Empty) => Ok([None, None]),
        (GratisSummaryValueV1::Empty, GratisSummaryValueV1::Summary(_)) => {
            Err(ProgramErrorV1::OutputCountMismatch)
        }
        (GratisSummaryValueV1::Summary(left), right) => {
            let left_outgoing = parent_incoming
                .and_then(|remaining| remaining.checked_sub(left.checked_segment_gratis_total));
            let left_prefix = Some(GratisIncomingV1 {
                start_ordinal: left.start_ordinal,
                end_ordinal: left.end_ordinal,
                incoming_remaining: parent_incoming,
            });
            let right_prefix = match right {
                GratisSummaryValueV1::Empty => None,
                GratisSummaryValueV1::Summary(right) => {
                    if left.end_ordinal != right.start_ordinal {
                        return Err(ProgramErrorV1::OutputCountMismatch);
                    }
                    Some(GratisIncomingV1 {
                        start_ordinal: right.start_ordinal,
                        end_ordinal: right.end_ordinal,
                        incoming_remaining: left_outgoing,
                    })
                }
            };
            Ok([left_prefix, right_prefix])
        }
    }
}

pub fn finalize_gratis_leaf(
    incoming_remaining: Option<U256>,
    start_ordinal: u32,
    record_count: usize,
    checked_segment_gratis_total: U256,
) -> Result<GratisLeafPrefixV1, ProgramErrorV1> {
    if record_count == 0
        || record_count
            > usize::try_from(PRIMARY_WORK_SHARD_SIZE)
                .map_err(|_| ProgramErrorV1::OutputCountMismatch)?
        || checked_segment_gratis_total.is_zero()
    {
        return Err(ProgramErrorV1::OutputCountMismatch);
    }
    let incoming_remaining = incoming_remaining.ok_or(ProgramErrorV1::OutputCountMismatch)?;
    let outgoing_remaining = incoming_remaining
        .checked_sub(checked_segment_gratis_total)
        .ok_or(ProgramErrorV1::GratisLoadExceedsRemaining {
            ordinal: start_ordinal as usize,
        })?;
    Ok(GratisLeafPrefixV1 {
        segment_ordinal: start_ordinal / PRIMARY_WORK_SHARD_SIZE,
        incoming_remaining,
        outgoing_remaining,
        first_error_ordinal: None,
    })
}

pub fn output_finalize(
    amounts: &AmountRunV1,
    prefix: &GratisLeafPrefixV1,
) -> Result<FinalizedOutputRunV1, ProgramErrorV1> {
    if amounts.ordered_records.is_empty()
        || amounts.start_ordinal / PRIMARY_WORK_SHARD_SIZE != prefix.segment_ordinal
        || prefix.first_error_ordinal.is_some()
    {
        return Err(ProgramErrorV1::OutputCountMismatch);
    }
    let outgoing = prefix
        .incoming_remaining
        .checked_sub(amounts.checked_segment_gratis_total)
        .ok_or(ProgramErrorV1::GratisLoadExceedsRemaining {
            ordinal: amounts.start_ordinal as usize,
        })?;
    if outgoing != prefix.outgoing_remaining {
        return Err(ProgramErrorV1::OutputCountMismatch);
    }
    let mut ordered_records = Vec::with_capacity(amounts.ordered_records.len());
    for amount in &amounts.ordered_records {
        if amount.raw_ordinal
            != amounts
                .start_ordinal
                .checked_add(
                    u32::try_from(ordered_records.len())
                        .map_err(|_| ProgramErrorV1::OutputCountMismatch)?,
                )
                .ok_or(ProgramErrorV1::OutputCountMismatch)?
        {
            return Err(ProgramErrorV1::OutputCountMismatch);
        }
        let nod_id =
            derive_poseidon_entity_id(amount.owner, amount.worldwide_day).map_err(|error| {
                ProgramErrorV1::Arithmetic {
                    message: error.to_string(),
                }
            })?;
        ordered_records.push(FinalizedOutputRecordV1 {
            raw_ordinal: amount.raw_ordinal,
            nod_action: NodActionV1 {
                source_tribute_id: amount.tribute_id,
                nod_id,
                owner: amount.owner,
                worldwide_day: amount.worldwide_day,
                league_id: amount.league_id,
                entry_price_minor: amount.entry_price_minor,
                issuance_currency: amount.issuance_currency,
                reference_currency: amount.reference_currency,
            },
        });
    }
    if amounts.end_ordinal
        != amounts
            .start_ordinal
            .checked_add(
                u32::try_from(ordered_records.len())
                    .map_err(|_| ProgramErrorV1::OutputCountMismatch)?,
            )
            .ok_or(ProgramErrorV1::OutputCountMismatch)?
    {
        return Err(ProgramErrorV1::OutputCountMismatch);
    }
    Ok(FinalizedOutputRunV1 {
        start_ordinal: amounts.start_ordinal,
        end_ordinal: amounts.end_ordinal,
        ordered_records,
        checked_tribute_nominal_total: amounts.checked_segment_nominal_total,
        checked_segment_gratis_total: amounts.checked_segment_gratis_total,
        checked_settlement_cost_total: amounts.checked_segment_settlement_cost_total,
    })
}

fn validate_finalized_leaf(run: &FinalizedOutputRunV1) -> Result<u32, ProgramErrorV1> {
    let count = u32::try_from(run.ordered_records.len())
        .map_err(|_| ProgramErrorV1::OutputCountMismatch)?;
    if count == 0
        || count > PRIMARY_WORK_SHARD_SIZE
        || !run.start_ordinal.is_multiple_of(PRIMARY_WORK_SHARD_SIZE)
        || run.end_ordinal
            != run
                .start_ordinal
                .checked_add(count)
                .ok_or(ProgramErrorV1::OutputCountMismatch)?
    {
        return Err(ProgramErrorV1::OutputCountMismatch);
    }
    for (local_ordinal, record) in run.ordered_records.iter().enumerate() {
        let expected = run
            .start_ordinal
            .checked_add(
                u32::try_from(local_ordinal).map_err(|_| ProgramErrorV1::OutputCountMismatch)?,
            )
            .ok_or(ProgramErrorV1::OutputCountMismatch)?;
        if record.raw_ordinal != expected {
            return Err(ProgramErrorV1::OutputCountMismatch);
        }
    }
    Ok(run.start_ordinal / PRIMARY_WORK_SHARD_SIZE)
}

pub fn shuffle_buckets(run: &FinalizedOutputRunV1) -> Result<BucketOrderedRunV1, ProgramErrorV1> {
    let run_ordinal = validate_finalized_leaf(run)?;
    let mut ordered_records = run
        .ordered_records
        .iter()
        .map(|record| BucketRecordV1 {
            bucket_key: record.nod_action.bucket_key(),
            raw_ordinal: record.raw_ordinal,
            tribute_id: record.nod_action.source_tribute_id,
            nod_id: record.nod_action.nod_id,
        })
        .collect::<Vec<_>>();
    ordered_records.sort_by_key(|record| (record.bucket_key, record.raw_ordinal));
    Ok(BucketOrderedRunV1 {
        run_ordinal,
        ordered_records,
    })
}
