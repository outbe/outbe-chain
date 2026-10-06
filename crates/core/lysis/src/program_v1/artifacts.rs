//! Canonical bounded artifacts for the fixed Lysis V1 work graph.

use std::fmt;

use alloy_primitives::B256;
use outbe_primitives::time::WorldwideDay;

use super::phases::{FidelityAggregateV1, GratisIncomingV1, GratisLeafPrefixV1};
use super::planner::PRIMARY_WORK_SHARD_SIZE;
use super::{LeagueFractionV1, ProgramErrorV1, TributeInputV1};

mod coverage;
mod fidelity;
mod gratis;
mod runs;

pub use coverage::{decode_raw_coverage_carrier, encode_raw_coverage_carrier};
pub use fidelity::{
    decode_fidelity_map_output, decode_fixed_reduce_output, encode_fidelity_map_output,
    encode_fixed_reduce_output,
};
pub use gratis::{
    decode_gratis_prefix_down_output, decode_gratis_segment_summary,
    encode_gratis_prefix_down_output, encode_gratis_segment_summary, gratis_summary_coverage,
};
pub use runs::{
    decode_amount_run, decode_enumerated_run, decode_finalized_output_run, encode_amount_run,
    encode_enumerated_run, encode_finalized_output_run, enumerate_tributes,
};

const ENUMERATED_RUN_MAGIC: [u8; 4] = *b"LYE1";
const FIDELITY_MAP_MAGIC: [u8; 4] = *b"LYF1";
const FIXED_REDUCE_MAGIC: [u8; 4] = *b"LYR1";
const AMOUNT_RUN_MAGIC: [u8; 4] = *b"LYA1";
const GRATIS_SUMMARY_MAGIC: [u8; 4] = *b"LYG1";
const GRATIS_PREFIX_DOWN_MAGIC: [u8; 4] = *b"LYD1";
const FINALIZED_OUTPUT_MAGIC: [u8; 4] = *b"LYO1";
const RAW_COVERAGE_CARRIER_MAGIC: [u8; 4] = *b"LYC1";
const COVERAGE_RECORD_BYTES: usize = 36;
const PRIMARY_SUBTREE_HEIGHT: u16 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawCoverageCarrierV1 {
    pub start_ordinal: u32,
    pub end_ordinal: u32,
    pub subtree_height: u16,
    pub subtree_index: u32,
    pub tree_root: B256,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixedReduceOutputV1 {
    pub aggregate: Option<FidelityAggregateV1>,
    pub coverage: RawCoverageCarrierV1,
    pub ordered_fractions: Vec<LeagueFractionV1>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GratisPrefixDownOutputV1 {
    Branch([Option<GratisIncomingV1>; 2]),
    Leaf(GratisLeafPrefixV1),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GratisSummaryCoverageV1 {
    pub root: B256,
    pub count: u32,
}

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

fn validate_shard_size(count: usize) -> Result<(), LysisArtifactErrorV1> {
    if count == 0 || count > PRIMARY_WORK_SHARD_SIZE as usize {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "enumerated run shard size",
        ));
    }
    Ok(())
}
