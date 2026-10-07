use alloy_primitives::{B256, U256};
use outbe_compressed_entities::{derive_poseidon_entity_id, WwdEntityId};
use outbe_nod::NodContract;
use outbe_ocomp_protocol::{CanonicalReader, CanonicalWriter, SchemaLimits};
use outbe_primitives::time::WorldwideDay;

use super::coverage::raw_coverage_root;
use super::{
    validate_shard_size, EnumeratedRunV1, EnumeratedTributeRecordV1, LysisArtifactErrorV1,
    AMOUNT_RUN_MAGIC, ENUMERATED_RUN_MAGIC, FINALIZED_OUTPUT_MAGIC, PRIMARY_WORK_SHARD_SIZE,
};
use crate::program_v1::execute::validate_canonical_tributes;
use crate::program_v1::phases::{
    AmountRecordV1, AmountRunV1, FinalizedContributorV1, FinalizedOutputRecordV1,
    FinalizedOutputRunV1,
};
use crate::program_v1::{NodActionV1, TributeInputV1};

impl EnumeratedRunV1 {
    pub fn coverage_root(&self) -> Result<B256, LysisArtifactErrorV1> {
        validate_enumerated_run(self)?;
        raw_coverage_root(
            self.ordered_records
                .iter()
                .map(|record| (record.raw_ordinal, record.tribute.tribute_id)),
        )
    }
}

impl AmountRunV1 {
    pub fn coverage_root(&self) -> Result<B256, LysisArtifactErrorV1> {
        validate_amount_run(self)?;
        raw_coverage_root(
            self.ordered_records
                .iter()
                .map(|record| (record.raw_ordinal, record.tribute_id)),
        )
    }
}

impl FinalizedOutputRunV1 {
    pub fn coverage_root(&self) -> Result<B256, LysisArtifactErrorV1> {
        validate_finalized_output_run(self)?;
        raw_coverage_root(
            self.ordered_records
                .iter()
                .map(|record| (record.raw_ordinal, record.nod_action.source_tribute_id)),
        )
    }
}

pub fn enumerate_tributes(
    start_ordinal: u32,
    worldwide_day: WorldwideDay,
    tributes: &[TributeInputV1],
) -> Result<EnumeratedRunV1, LysisArtifactErrorV1> {
    validate_shard_size(tributes.len())?;
    validate_canonical_tributes(worldwide_day, tributes)?;
    let count = u32::try_from(tributes.len()).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
    let end_ordinal = start_ordinal
        .checked_add(count)
        .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
    let ordered_records = tributes
        .iter()
        .enumerate()
        .map(|(offset, tribute)| {
            let offset = u32::try_from(offset).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
            Ok(EnumeratedTributeRecordV1 {
                raw_ordinal: start_ordinal
                    .checked_add(offset)
                    .ok_or(LysisArtifactErrorV1::LengthOverflow)?,
                tribute: tribute.clone(),
            })
        })
        .collect::<Result<Vec<_>, LysisArtifactErrorV1>>()?;
    Ok(EnumeratedRunV1 {
        start_ordinal,
        end_ordinal,
        worldwide_day,
        ordered_records,
    })
}

pub fn encode_amount_run(
    run: &AmountRunV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, LysisArtifactErrorV1> {
    validate_amount_run(run)?;
    let mut encoded = CanonicalWriter::new(limits.codec);
    encoded.write_fixed(&AMOUNT_RUN_MAGIC)?;
    encoded.write_u32(run.start_ordinal)?;
    encoded.write_u32(run.end_ordinal)?;
    encoded.write_u256(run.checked_segment_gratis_total)?;
    encoded.write_u32(
        u32::try_from(run.ordered_records.len())
            .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?,
    )?;
    for record in &run.ordered_records {
        encoded.write_u32(record.raw_ordinal)?;
        encoded.write_b256(*record.tribute_id)?;
        encoded.write_address20(record.owner)?;
        encoded.write_u32(record.worldwide_day.value())?;
        encoded.write_u16(record.league_id)?;
        encoded.write_u256(record.nominal_amount_minor)?;
        encoded.write_u256(record.gratis_fraction_fp)?;
        encoded.write_u256(record.gratis_load_minor)?;
        encoded.write_u256(record.entry_price_minor)?;
        encoded.write_u256(record.settlement_cost_minor)?;
        encoded.write_u16(record.issuance_currency)?;
        encoded.write_u16(record.reference_currency)?;
        encoded.write_bool(record.exclude_from_intex_issuance)?;
    }
    Ok(encoded.into_bytes())
}

pub fn decode_amount_run(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<AmountRunV1, LysisArtifactErrorV1> {
    let mut input = CanonicalReader::new(encoded, limits.codec)?;
    if input.read_fixed::<4>()? != AMOUNT_RUN_MAGIC {
        return Err(LysisArtifactErrorV1::InvalidEncoding("amount run header"));
    }
    let start_ordinal = input.read_u32()?;
    let end_ordinal = input.read_u32()?;
    let checked_segment_gratis_total = input.read_u256()?;
    let count = input.read_u32()?;
    validate_shard_size(usize::try_from(count).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?)?;
    let mut ordered_records = Vec::new();
    ordered_records
        .try_reserve_exact(
            usize::try_from(count).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?,
        )
        .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
    for _ in 0..count {
        ordered_records.push(AmountRecordV1 {
            raw_ordinal: input.read_u32()?,
            tribute_id: WwdEntityId::from(input.read_b256()?),
            owner: input.read_address20()?,
            worldwide_day: WorldwideDay::new(input.read_u32()?),
            league_id: input.read_u16()?,
            nominal_amount_minor: input.read_u256()?,
            gratis_fraction_fp: input.read_u256()?,
            gratis_load_minor: input.read_u256()?,
            entry_price_minor: input.read_u256()?,
            settlement_cost_minor: input.read_u256()?,
            issuance_currency: input.read_u16()?,
            reference_currency: input.read_u16()?,
            exclude_from_intex_issuance: input.read_bool()?,
        });
    }
    input.finish()?;
    let run = AmountRunV1 {
        start_ordinal,
        end_ordinal,
        ordered_records,
        checked_segment_gratis_total,
    };
    validate_amount_run(&run)?;
    if encode_amount_run(&run, limits)? != encoded {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "amount run canonical re-encoding",
        ));
    }
    Ok(run)
}

pub fn encode_finalized_output_run(
    run: &FinalizedOutputRunV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, LysisArtifactErrorV1> {
    validate_finalized_output_run(run)?;
    let mut encoded = CanonicalWriter::new(limits.codec);
    encoded.write_fixed(&FINALIZED_OUTPUT_MAGIC)?;
    encoded.write_u32(run.start_ordinal)?;
    encoded.write_u32(run.end_ordinal)?;
    encoded.write_u32(
        u32::try_from(run.ordered_records.len())
            .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?,
    )?;
    for record in &run.ordered_records {
        let nod = &record.nod_action;
        encoded.write_u32(record.raw_ordinal)?;
        encoded.write_b256(*nod.source_tribute_id)?;
        encoded.write_b256(*nod.nod_id)?;
        encoded.write_address20(nod.owner)?;
        encoded.write_u32(nod.worldwide_day.value())?;
        encoded.write_u16(nod.league_id)?;
        encoded.write_u256(nod.gratis_load_minor)?;
        encoded.write_u256(nod.entry_price_minor)?;
        encoded.write_u256(nod.settlement_cost_minor)?;
        encoded.write_u16(nod.issuance_currency)?;
        encoded.write_u16(nod.reference_currency)?;
        encoded.write_option(record.contributor_action.as_ref(), |writer, contributor| {
            writer.write_address20(contributor.owner)?;
            writer.write_b256(*contributor.source_tribute_id)?;
            writer.write_u256(contributor.nominal_amount_minor)
        })?;
    }
    encoded.write_u256(run.checked_tribute_nominal_total)?;
    Ok(encoded.into_bytes())
}

pub fn decode_finalized_output_run(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<FinalizedOutputRunV1, LysisArtifactErrorV1> {
    let mut input = CanonicalReader::new(encoded, limits.codec)?;
    if input.read_fixed::<4>()? != FINALIZED_OUTPUT_MAGIC {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "finalized output header",
        ));
    }
    let start_ordinal = input.read_u32()?;
    let end_ordinal = input.read_u32()?;
    let count = input.read_u32()?;
    validate_shard_size(usize::try_from(count).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?)?;
    let mut ordered_records = Vec::new();
    ordered_records
        .try_reserve_exact(
            usize::try_from(count).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?,
        )
        .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
    for _ in 0..count {
        let raw_ordinal = input.read_u32()?;
        let source_tribute_id = WwdEntityId::from(input.read_b256()?);
        let nod_id = WwdEntityId::from(input.read_b256()?);
        let owner = input.read_address20()?;
        let worldwide_day = WorldwideDay::new(input.read_u32()?);
        let league_id = input.read_u16()?;
        let gratis_load_minor = input.read_u256()?;
        let entry_price_minor = input.read_u256()?;
        let settlement_cost_minor = input.read_u256()?;
        let issuance_currency = input.read_u16()?;
        let reference_currency = input.read_u16()?;
        let contributor_action = input.read_option(|reader| {
            Ok(FinalizedContributorV1 {
                owner: reader.read_address20()?,
                source_tribute_id: WwdEntityId::from(reader.read_b256()?),
                nominal_amount_minor: reader.read_u256()?,
            })
        })?;
        ordered_records.push(FinalizedOutputRecordV1 {
            raw_ordinal,
            nod_action: NodActionV1 {
                source_tribute_id,
                nod_id,
                owner,
                worldwide_day,
                league_id,
                gratis_load_minor,
                entry_price_minor,
                settlement_cost_minor,
                issuance_currency,
                reference_currency,
            },
            contributor_action,
        });
    }
    let checked_tribute_nominal_total = input.read_u256()?;
    input.finish()?;
    let run = FinalizedOutputRunV1 {
        start_ordinal,
        end_ordinal,
        ordered_records,
        checked_tribute_nominal_total,
    };
    validate_finalized_output_run(&run)?;
    if encode_finalized_output_run(&run, limits)? != encoded {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "finalized output canonical re-encoding",
        ));
    }
    Ok(run)
}

pub fn encode_enumerated_run(
    run: &EnumeratedRunV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, LysisArtifactErrorV1> {
    validate_enumerated_run(run)?;
    let mut output = CanonicalWriter::new(limits.codec);
    output.write_fixed(&ENUMERATED_RUN_MAGIC)?;
    output.write_u32(run.start_ordinal)?;
    output.write_u32(run.end_ordinal)?;
    output.write_u32(run.worldwide_day.value())?;
    output.write_u32(
        u32::try_from(run.ordered_records.len())
            .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?,
    )?;
    for record in &run.ordered_records {
        output.write_u32(record.raw_ordinal)?;
        output.write_b256(*record.tribute.tribute_id)?;
        output.write_address20(record.tribute.owner)?;
        output.write_u16(record.tribute.issuance_currency)?;
        output.write_u256(record.tribute.nominal_amount_minor)?;
        output.write_u16(record.tribute.reference_currency)?;
        output.write_u256(record.tribute.tribute_price_minor)?;
        output.write_bool(record.tribute.exclude_from_intex_issuance)?;
    }
    Ok(output.into_bytes())
}

pub fn decode_enumerated_run(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<EnumeratedRunV1, LysisArtifactErrorV1> {
    let mut input = CanonicalReader::new(encoded, limits.codec)?;
    if input.read_fixed::<4>()? != ENUMERATED_RUN_MAGIC {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "enumerated run header",
        ));
    }
    let start_ordinal = input.read_u32()?;
    let end_ordinal = input.read_u32()?;
    let worldwide_day = WorldwideDay::new(input.read_u32()?);
    let count = input.read_u32()?;
    validate_shard_size(usize::try_from(count).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?)?;
    let mut ordered_records = Vec::new();
    ordered_records
        .try_reserve_exact(
            usize::try_from(count).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?,
        )
        .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
    for _ in 0..count {
        let raw_ordinal = input.read_u32()?;
        let tribute_id = WwdEntityId::from(input.read_b256()?);
        ordered_records.push(EnumeratedTributeRecordV1 {
            raw_ordinal,
            tribute: TributeInputV1 {
                tribute_id,
                owner: input.read_address20()?,
                worldwide_day,
                issuance_currency: input.read_u16()?,
                nominal_amount_minor: input.read_u256()?,
                reference_currency: input.read_u16()?,
                tribute_price_minor: input.read_u256()?,
                exclude_from_intex_issuance: input.read_bool()?,
            },
        });
    }
    input.finish()?;
    let run = EnumeratedRunV1 {
        start_ordinal,
        end_ordinal,
        worldwide_day,
        ordered_records,
    };
    validate_enumerated_run(&run)?;
    if encode_enumerated_run(&run, limits)? != encoded {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "enumerated run canonical re-encoding",
        ));
    }
    Ok(run)
}

fn validate_enumerated_run(run: &EnumeratedRunV1) -> Result<(), LysisArtifactErrorV1> {
    validate_shard_size(run.ordered_records.len())?;
    let tributes = run
        .ordered_records
        .iter()
        .map(|record| record.tribute.clone())
        .collect::<Vec<_>>();
    validate_canonical_tributes(run.worldwide_day, &tributes)?;
    let count = u32::try_from(run.ordered_records.len())
        .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
    if run.end_ordinal
        != run
            .start_ordinal
            .checked_add(count)
            .ok_or(LysisArtifactErrorV1::LengthOverflow)?
        || run
            .ordered_records
            .iter()
            .enumerate()
            .any(|(offset, record)| {
                u32::try_from(offset)
                    .ok()
                    .and_then(|offset| run.start_ordinal.checked_add(offset))
                    != Some(record.raw_ordinal)
            })
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "enumerated run ordinal coverage",
        ));
    }
    Ok(())
}

fn validate_amount_run(run: &AmountRunV1) -> Result<(), LysisArtifactErrorV1> {
    validate_shard_size(run.ordered_records.len())?;
    let count = u32::try_from(run.ordered_records.len())
        .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
    if run.end_ordinal
        != run
            .start_ordinal
            .checked_add(count)
            .ok_or(LysisArtifactErrorV1::LengthOverflow)?
        || run.checked_segment_gratis_total.is_zero()
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "amount run aggregate range",
        ));
    }
    let mut checked_gratis_total = U256::ZERO;
    let mut previous_id = None;
    for (offset, record) in run.ordered_records.iter().enumerate() {
        let offset = u32::try_from(offset).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
        let expected_ordinal = run
            .start_ordinal
            .checked_add(offset)
            .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
        let has_zero_field = record.owner.is_zero()
            || record.worldwide_day.value() == 0
            || [
                record.nominal_amount_minor,
                record.gratis_fraction_fp,
                record.gratis_load_minor,
                record.entry_price_minor,
            ]
            .iter()
            .any(U256::is_zero);
        if record.raw_ordinal != expected_ordinal
            || previous_id.is_some_and(|previous| previous >= record.tribute_id)
            || has_zero_field
        {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "amount run record order",
            ));
        }
        if !NodContract::is_issuable_entry(record.entry_price_minor) {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "amount run Nod entry price bound",
            ));
        }
        previous_id = Some(record.tribute_id);
        checked_gratis_total = checked_gratis_total
            .checked_add(record.gratis_load_minor)
            .ok_or(LysisArtifactErrorV1::InvalidEncoding(
                "amount run Gratis overflow",
            ))?;
    }
    if checked_gratis_total != run.checked_segment_gratis_total {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "amount run Gratis total",
        ));
    }
    Ok(())
}

fn validate_finalized_output_run(run: &FinalizedOutputRunV1) -> Result<(), LysisArtifactErrorV1> {
    validate_shard_size(run.ordered_records.len())?;
    let count = u32::try_from(run.ordered_records.len())
        .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
    if !run.start_ordinal.is_multiple_of(PRIMARY_WORK_SHARD_SIZE)
        || run.checked_tribute_nominal_total.is_zero()
        || run.end_ordinal
            != run
                .start_ordinal
                .checked_add(count)
                .ok_or(LysisArtifactErrorV1::LengthOverflow)?
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "finalized output range",
        ));
    }
    let mut previous_tribute = None;
    for (offset, record) in run.ordered_records.iter().enumerate() {
        let offset = u32::try_from(offset).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
        let nod = &record.nod_action;
        let expected_ordinal = run
            .start_ordinal
            .checked_add(offset)
            .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
        let out_of_order = record.raw_ordinal != expected_ordinal
            || previous_tribute.is_some_and(|previous| previous >= nod.source_tribute_id);
        let has_zero_field = nod.owner.is_zero()
            || nod.worldwide_day.value() == 0
            || nod.gratis_load_minor.is_zero();
        let has_invalid_price = nod.entry_price_minor.is_zero()
            || !NodContract::is_issuable_entry(nod.entry_price_minor)
            || nod.reference_currency == 0;
        if out_of_order
            || has_zero_field
            || has_invalid_price
            || derive_poseidon_entity_id(nod.owner, nod.worldwide_day)
                .map_err(|_| LysisArtifactErrorV1::InvalidEncoding("finalized Nod identity"))?
                != nod.nod_id
        {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "finalized output record",
            ));
        }
        previous_tribute = Some(nod.source_tribute_id);
        if record
            .contributor_action
            .as_ref()
            .is_some_and(|contributor| {
                contributor.owner != nod.owner
                    || contributor.source_tribute_id != nod.source_tribute_id
                    || contributor.nominal_amount_minor.is_zero()
            })
        {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "finalized contributor binding",
            ));
        }
    }
    Ok(())
}
