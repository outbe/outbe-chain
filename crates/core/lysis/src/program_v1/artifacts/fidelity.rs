use std::collections::BTreeMap;

use alloy_primitives::{B256, U256};
use outbe_compressed_entities::WwdEntityId;
use outbe_ocomp_protocol::{CanonicalReader, CanonicalWriter, SchemaLimits};

use super::coverage::{
    decode_raw_coverage_carrier, encode_raw_coverage_carrier, raw_coverage_root,
    validate_raw_coverage_carrier,
};
use super::{
    validate_shard_size, FixedReduceOutputV1, LysisArtifactErrorV1, FIDELITY_MAP_MAGIC,
    FIXED_REDUCE_MAGIC,
};
use crate::program_v1::phases::{
    FidelityAggregateV1, FidelityLeaguePartialV1, FidelityMapOutputV1, FidelityObservationV1,
};
use crate::program_v1::LeagueFractionV1;

impl FidelityMapOutputV1 {
    pub fn coverage_root(&self) -> Result<B256, LysisArtifactErrorV1> {
        validate_fidelity_map_output(self)?;
        raw_coverage_root(
            self.observations
                .iter()
                .map(|record| (record.raw_ordinal, record.tribute_id)),
        )
    }
}

pub fn encode_fixed_reduce_output(
    output: &FixedReduceOutputV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, LysisArtifactErrorV1> {
    validate_fixed_reduce_output(output)?;
    let encoded_carrier = encode_raw_coverage_carrier(&output.coverage, limits)?;
    let mut encoded = CanonicalWriter::new(limits.codec);
    encoded.write_fixed(&FIXED_REDUCE_MAGIC)?;
    encoded.write_bounded_bytes(&encoded_carrier, limits.max_bounded_bytes)?;
    encoded.write_option(output.aggregate.as_ref(), |writer, aggregate| {
        writer.write_u32(aggregate.start_ordinal)?;
        writer.write_u32(aggregate.end_ordinal)?;
        writer.write_u32(aggregate.tribute_count)?;
        writer.write_u256(aggregate.checked_total_nominal)?;
        writer.write_vec(
            &aggregate.ordered_league_partials,
            limits.max_collection_items,
            |writer, partial| {
                writer.write_u16(partial.league_id)?;
                writer.write_u32(partial.count)?;
                writer.write_u256(partial.nominal_amount_minor)
            },
        )
    })?;
    encoded.write_vec(
        &output.ordered_fractions,
        limits.max_collection_items,
        |writer, fraction| {
            writer.write_u16(fraction.league)?;
            writer.write_u256(fraction.fraction)
        },
    )?;
    Ok(encoded.into_bytes())
}

pub fn decode_fixed_reduce_output(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<FixedReduceOutputV1, LysisArtifactErrorV1> {
    let mut input = CanonicalReader::new(encoded, limits.codec)?;
    if input.read_fixed::<4>()? != FIXED_REDUCE_MAGIC {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "fixed reducer output header",
        ));
    }
    let carrier =
        decode_raw_coverage_carrier(input.read_bounded_bytes(limits.max_bounded_bytes)?, limits)?;
    let aggregate = input.read_option(|reader| {
        let start_ordinal = reader.read_u32()?;
        let end_ordinal = reader.read_u32()?;
        let tribute_count = reader.read_u32()?;
        let checked_total_nominal = reader.read_u256()?;
        let ordered_league_partials =
            reader.read_vec(limits.max_collection_items, 38, |reader| {
                Ok(FidelityLeaguePartialV1 {
                    league_id: reader.read_u16()?,
                    count: reader.read_u32()?,
                    nominal_amount_minor: reader.read_u256()?,
                })
            })?;
        Ok(FidelityAggregateV1 {
            start_ordinal,
            end_ordinal,
            tribute_count,
            checked_total_nominal,
            ordered_league_partials,
        })
    })?;
    let ordered_fractions = input.read_vec(limits.max_collection_items, 34, |reader| {
        Ok(LeagueFractionV1 {
            league: reader.read_u16()?,
            fraction: reader.read_u256()?,
        })
    })?;
    input.finish()?;
    let output = FixedReduceOutputV1 {
        aggregate,
        coverage: carrier,
        ordered_fractions,
    };
    validate_fixed_reduce_output(&output)?;
    if encode_fixed_reduce_output(&output, limits)? != encoded {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "fixed reducer output canonical re-encoding",
        ));
    }
    Ok(output)
}

pub fn encode_fidelity_map_output(
    output: &FidelityMapOutputV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, LysisArtifactErrorV1> {
    validate_fidelity_map_output(output)?;
    let mut encoded = CanonicalWriter::new(limits.codec);
    encoded.write_fixed(&FIDELITY_MAP_MAGIC)?;
    encoded.write_u32(output.aggregate.start_ordinal)?;
    encoded.write_u32(output.aggregate.end_ordinal)?;
    encoded.write_u32(output.aggregate.tribute_count)?;
    encoded.write_u256(output.aggregate.checked_total_nominal)?;
    encoded.write_u32(
        u32::try_from(output.observations.len())
            .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?,
    )?;
    for observation in &output.observations {
        encoded.write_u32(observation.raw_ordinal)?;
        encoded.write_b256(*observation.tribute_id)?;
        encoded.write_u16(observation.pre_distribution_league)?;
        encoded.write_u16(observation.issuance_league)?;
        encoded.write_u256(observation.nominal_amount_minor)?;
    }
    encoded.write_u32(
        u32::try_from(output.aggregate.ordered_league_partials.len())
            .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?,
    )?;
    for partial in &output.aggregate.ordered_league_partials {
        encoded.write_u16(partial.league_id)?;
        encoded.write_u32(partial.count)?;
        encoded.write_u256(partial.nominal_amount_minor)?;
    }
    Ok(encoded.into_bytes())
}

pub fn decode_fidelity_map_output(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<FidelityMapOutputV1, LysisArtifactErrorV1> {
    let mut input = CanonicalReader::new(encoded, limits.codec)?;
    if input.read_fixed::<4>()? != FIDELITY_MAP_MAGIC {
        return Err(LysisArtifactErrorV1::InvalidEncoding("Fidelity map header"));
    }
    let start_ordinal = input.read_u32()?;
    let end_ordinal = input.read_u32()?;
    let tribute_count = input.read_u32()?;
    let checked_total_nominal = input.read_u256()?;
    let observation_count = input.read_u32()?;
    validate_shard_size(
        usize::try_from(observation_count).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?,
    )?;
    let mut observations = Vec::new();
    observations
        .try_reserve_exact(
            usize::try_from(observation_count).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?,
        )
        .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
    for _ in 0..observation_count {
        observations.push(FidelityObservationV1 {
            raw_ordinal: input.read_u32()?,
            tribute_id: WwdEntityId::from(input.read_b256()?),
            pre_distribution_league: input.read_u16()?,
            issuance_league: input.read_u16()?,
            nominal_amount_minor: input.read_u256()?,
        });
    }
    let partial_count = input.read_u32()?;
    validate_shard_size(
        usize::try_from(partial_count).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?,
    )?;
    let mut ordered_league_partials = Vec::new();
    ordered_league_partials
        .try_reserve_exact(
            usize::try_from(partial_count).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?,
        )
        .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
    for _ in 0..partial_count {
        ordered_league_partials.push(FidelityLeaguePartialV1 {
            league_id: input.read_u16()?,
            count: input.read_u32()?,
            nominal_amount_minor: input.read_u256()?,
        });
    }
    input.finish()?;
    let output = FidelityMapOutputV1 {
        observations,
        aggregate: FidelityAggregateV1 {
            start_ordinal,
            end_ordinal,
            tribute_count,
            checked_total_nominal,
            ordered_league_partials,
        },
    };
    validate_fidelity_map_output(&output)?;
    if encode_fidelity_map_output(&output, limits)? != encoded {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "Fidelity map canonical re-encoding",
        ));
    }
    Ok(output)
}

fn validate_fidelity_map_output(output: &FidelityMapOutputV1) -> Result<(), LysisArtifactErrorV1> {
    validate_shard_size(output.observations.len())?;
    validate_shard_size(output.aggregate.ordered_league_partials.len())?;
    let observation_count = u32::try_from(output.observations.len())
        .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
    if output.aggregate.tribute_count != observation_count
        || output.aggregate.end_ordinal
            != output
                .aggregate
                .start_ordinal
                .checked_add(observation_count)
                .ok_or(LysisArtifactErrorV1::LengthOverflow)?
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "Fidelity map aggregate range",
        ));
    }

    let mut checked_total_nominal = U256::ZERO;
    let mut previous_id = None;
    let mut expected_partials = BTreeMap::<u16, (u32, U256)>::new();
    for (offset, observation) in output.observations.iter().enumerate() {
        let offset = u32::try_from(offset).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
        if observation.raw_ordinal
            != output
                .aggregate
                .start_ordinal
                .checked_add(offset)
                .ok_or(LysisArtifactErrorV1::LengthOverflow)?
            || observation.pre_distribution_league != observation.issuance_league
            || previous_id.is_some_and(|previous| previous >= observation.tribute_id)
        {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "Fidelity map observation order",
            ));
        }
        previous_id = Some(observation.tribute_id);
        checked_total_nominal = checked_total_nominal
            .checked_add(observation.nominal_amount_minor)
            .ok_or(LysisArtifactErrorV1::InvalidEncoding(
                "Fidelity map nominal overflow",
            ))?;
        let expected = expected_partials
            .entry(observation.issuance_league)
            .or_insert((0, U256::ZERO));
        expected.0 = expected
            .0
            .checked_add(1)
            .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
        expected.1 = expected
            .1
            .checked_add(observation.nominal_amount_minor)
            .ok_or(LysisArtifactErrorV1::InvalidEncoding(
                "Fidelity map league nominal overflow",
            ))?;
    }
    if checked_total_nominal != output.aggregate.checked_total_nominal {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "Fidelity map aggregate nominal",
        ));
    }

    let (checked_partial_count, checked_partial_nominal) = checked_league_partials(
        &output.aggregate.ordered_league_partials,
        "Fidelity map league partial order",
        "Fidelity map partial nominal overflow",
    )?;
    if checked_partial_count != observation_count
        || checked_partial_nominal != checked_total_nominal
        || output
            .aggregate
            .ordered_league_partials
            .iter()
            .map(|partial| {
                (
                    partial.league_id,
                    (partial.count, partial.nominal_amount_minor),
                )
            })
            .ne(expected_partials
                .iter()
                .map(|(league, values)| (*league, *values)))
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "Fidelity map league partial totals",
        ));
    }
    Ok(())
}

fn validate_fixed_reduce_output(output: &FixedReduceOutputV1) -> Result<(), LysisArtifactErrorV1> {
    validate_raw_coverage_carrier(&output.coverage)?;
    let Some(aggregate) = &output.aggregate else {
        if output.coverage.start_ordinal != output.coverage.end_ordinal
            || !output.ordered_fractions.is_empty()
        {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "empty fixed reducer output",
            ));
        }
        return Ok(());
    };

    let coverage_differs = aggregate.start_ordinal != output.coverage.start_ordinal
        || aggregate.end_ordinal != output.coverage.end_ordinal;
    let aggregate_empty =
        aggregate.checked_total_nominal.is_zero() || aggregate.ordered_league_partials.is_empty();
    if coverage_differs
        || aggregate.start_ordinal >= aggregate.end_ordinal
        || aggregate.tribute_count != aggregate.end_ordinal - aggregate.start_ordinal
        || aggregate_empty
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "fixed reducer aggregate range",
        ));
    }
    let (checked_count, checked_nominal) = checked_league_partials(
        &aggregate.ordered_league_partials,
        "fixed reducer league partial order",
        "fixed reducer nominal overflow",
    )?;
    if checked_count != aggregate.tribute_count
        || checked_nominal != aggregate.checked_total_nominal
        || (!output.ordered_fractions.is_empty()
            && (output.ordered_fractions.len() != aggregate.ordered_league_partials.len()
                || output
                    .ordered_fractions
                    .iter()
                    .zip(&aggregate.ordered_league_partials)
                    .any(|(fraction, partial)| fraction.league != partial.league_id)))
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "fixed reducer aggregate totals",
        ));
    }
    Ok(())
}

fn checked_league_partials(
    partials: &[FidelityLeaguePartialV1],
    order_error: &'static str,
    overflow_error: &'static str,
) -> Result<(u32, U256), LysisArtifactErrorV1> {
    let mut checked_count = 0_u32;
    let mut checked_nominal = U256::ZERO;
    let mut previous_league = None;
    for partial in partials {
        if partial.count == 0
            || partial.nominal_amount_minor.is_zero()
            || previous_league.is_some_and(|previous| previous >= partial.league_id)
        {
            return Err(LysisArtifactErrorV1::InvalidEncoding(order_error));
        }
        previous_league = Some(partial.league_id);
        checked_count = checked_count
            .checked_add(partial.count)
            .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
        checked_nominal = checked_nominal
            .checked_add(partial.nominal_amount_minor)
            .ok_or(LysisArtifactErrorV1::InvalidEncoding(overflow_error))?;
    }
    Ok((checked_count, checked_nominal))
}
