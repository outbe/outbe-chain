//! Canonical bounded artifacts for the fixed Lysis V1 work graph.

use std::fmt;

use alloy_primitives::B256;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::EntityId36;
use outbe_ocomp_protocol::list::{ordered_list_root, OrderedListLimits};
use outbe_ocomp_protocol::registry::ListKind;
use outbe_ocomp_protocol::{CanonicalReader, CanonicalWriter, SchemaLimits};

use super::execute::validate_canonical_tributes;
use super::planner::PRIMARY_WORK_SHARD_SIZE;
use super::{ProgramErrorV1, TributeInputV1};

const ENUMERATED_RUN_MAGIC: [u8; 4] = *b"LYE1";
const COVERAGE_RECORD_BYTES: usize = 40;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnumeratedTributeRecordV1 {
    pub raw_ordinal: u32,
    pub tribute: TributeInputV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnumeratedRunV1 {
    pub start_ordinal: u32,
    pub end_ordinal: u32,
    pub worldwide_day: WorldwideDay,
    pub ordered_records: Vec<EnumeratedTributeRecordV1>,
}

impl EnumeratedRunV1 {
    pub fn coverage_root(&self) -> Result<B256, LysisArtifactErrorV1> {
        validate_enumerated_run(self)?;
        let mut records = Vec::new();
        records
            .try_reserve_exact(self.ordered_records.len())
            .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
        for record in &self.ordered_records {
            let mut encoded = [0_u8; COVERAGE_RECORD_BYTES];
            encoded[..4].copy_from_slice(&record.raw_ordinal.to_be_bytes());
            encoded[4..].copy_from_slice(record.tribute.tribute_id.as_bytes());
            records.push(encoded);
        }
        ordered_list_root(
            ListKind::RawTributeCoverage,
            &records,
            OrderedListLimits {
                max_items: PRIMARY_WORK_SHARD_SIZE as usize,
                max_item_bytes: COVERAGE_RECORD_BYTES,
                max_tree_allocation_bytes: PRIMARY_WORK_SHARD_SIZE as usize
                    * core::mem::size_of::<B256>(),
            },
        )
        .map_err(LysisArtifactErrorV1::Protocol)
    }
}

#[derive(Debug)]
pub enum LysisArtifactErrorV1 {
    InvalidEncoding(&'static str),
    LengthOverflow,
    Program(ProgramErrorV1),
    Protocol(outbe_ocomp_protocol::ProtocolError),
}

impl fmt::Display for LysisArtifactErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEncoding(message) => {
                write!(formatter, "invalid Lysis V1 artifact: {message}")
            }
            Self::LengthOverflow => formatter.write_str("Lysis V1 artifact length overflow"),
            Self::Program(error) => write!(formatter, "{error}"),
            Self::Protocol(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for LysisArtifactErrorV1 {}

impl From<ProgramErrorV1> for LysisArtifactErrorV1 {
    fn from(error: ProgramErrorV1) -> Self {
        Self::Program(error)
    }
}

impl From<outbe_ocomp_protocol::ProtocolError> for LysisArtifactErrorV1 {
    fn from(error: outbe_ocomp_protocol::ProtocolError) -> Self {
        Self::Protocol(error)
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
        output.write_entity_id36(record.tribute.tribute_id.as_bytes())?;
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
        let tribute_id = EntityId36::try_from(input.read_entity_id36()?.as_slice())
            .map_err(|_| LysisArtifactErrorV1::InvalidEncoding("Tribute id"))?;
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

fn validate_shard_size(count: usize) -> Result<(), LysisArtifactErrorV1> {
    if count == 0 || count > PRIMARY_WORK_SHARD_SIZE as usize {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "enumerated run shard size",
        ));
    }
    Ok(())
}
