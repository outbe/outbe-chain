//! Bounded ROOT_REDUCE summary and closed result-finalization inputs.

use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::{
    CanonicalReader, CanonicalWriter, ListKind, SchemaLimits,
};

use super::{artifacts::LysisArtifactErrorV1, planner::PRIMARY_WORK_SHARD_SIZE};

const ROOT_REDUCE_SUMMARY_MAGIC: [u8; 4] = *b"LYQ1";
const MAX_LIST_SUBTREE_HEIGHT: u16 = 31;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LysisListSubtreeCarrierV1 {
    pub list_kind: ListKind,
    pub start_ordinal: u32,
    pub real_count: u32,
    pub subtree_height: u16,
    pub subtree_index: u32,
    pub tree_root: B256,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootReduceSummaryV1 {
    pub protocol_bundle_hash: B256,
    pub job_id: B256,
    pub attempt: u32,
    pub plan_hash: B256,
    pub covered_primary_start: u32,
    pub covered_primary_count: u32,
    pub nod_actions: LysisListSubtreeCarrierV1,
    pub bucket_records: LysisListSubtreeCarrierV1,
    pub contributor_actions: LysisListSubtreeCarrierV1,
    pub output_manifest_entries: LysisListSubtreeCarrierV1,
    pub result_chunk_hashes: LysisListSubtreeCarrierV1,
    pub tribute_count: u32,
    pub nod_count: u32,
    pub bucket_count: u32,
    pub contributor_count: u32,
    pub tribute_nominal_total: U256,
    pub eligible_nominal_total: U256,
    pub nod_gratis_consumed: U256,
    pub nod_cost_total: U256,
    pub first_error_ordinal: Option<u32>,
}

pub fn encode_root_reduce_summary(
    summary: &RootReduceSummaryV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, LysisArtifactErrorV1> {
    validate_root_reduce_summary(summary)?;
    let mut output = CanonicalWriter::new(limits.codec);
    output.write_fixed(&ROOT_REDUCE_SUMMARY_MAGIC)?;
    output.write_b256(summary.protocol_bundle_hash)?;
    output.write_b256(summary.job_id)?;
    output.write_u32(summary.attempt)?;
    output.write_b256(summary.plan_hash)?;
    output.write_u32(summary.covered_primary_start)?;
    output.write_u32(summary.covered_primary_count)?;
    for carrier in [
        &summary.nod_actions,
        &summary.bucket_records,
        &summary.contributor_actions,
        &summary.output_manifest_entries,
        &summary.result_chunk_hashes,
    ] {
        encode_list_subtree_carrier(&mut output, carrier)?;
    }
    output.write_u32(summary.tribute_count)?;
    output.write_u32(summary.nod_count)?;
    output.write_u32(summary.bucket_count)?;
    output.write_u32(summary.contributor_count)?;
    output.write_u256(summary.tribute_nominal_total)?;
    output.write_u256(summary.eligible_nominal_total)?;
    output.write_u256(summary.nod_gratis_consumed)?;
    output.write_u256(summary.nod_cost_total)?;
    output.write_option(summary.first_error_ordinal.as_ref(), |writer, ordinal| {
        writer.write_u32(*ordinal)
    })?;
    Ok(output.into_bytes())
}

pub fn decode_root_reduce_summary(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<RootReduceSummaryV1, LysisArtifactErrorV1> {
    let mut input = CanonicalReader::new(encoded, limits.codec)?;
    if input.read_fixed::<4>()? != ROOT_REDUCE_SUMMARY_MAGIC {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "root reducer summary header",
        ));
    }
    let summary = RootReduceSummaryV1 {
        protocol_bundle_hash: input.read_b256()?,
        job_id: input.read_b256()?,
        attempt: input.read_u32()?,
        plan_hash: input.read_b256()?,
        covered_primary_start: input.read_u32()?,
        covered_primary_count: input.read_u32()?,
        nod_actions: decode_list_subtree_carrier(&mut input)?,
        bucket_records: decode_list_subtree_carrier(&mut input)?,
        contributor_actions: decode_list_subtree_carrier(&mut input)?,
        output_manifest_entries: decode_list_subtree_carrier(&mut input)?,
        result_chunk_hashes: decode_list_subtree_carrier(&mut input)?,
        tribute_count: input.read_u32()?,
        nod_count: input.read_u32()?,
        bucket_count: input.read_u32()?,
        contributor_count: input.read_u32()?,
        tribute_nominal_total: input.read_u256()?,
        eligible_nominal_total: input.read_u256()?,
        nod_gratis_consumed: input.read_u256()?,
        nod_cost_total: input.read_u256()?,
        first_error_ordinal: input.read_option(|reader| reader.read_u32())?,
    };
    input.finish()?;
    validate_root_reduce_summary(&summary)?;
    if encode_root_reduce_summary(&summary, limits)? != encoded {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "root reducer summary canonical re-encoding",
        ));
    }
    Ok(summary)
}

fn encode_list_subtree_carrier(
    output: &mut CanonicalWriter,
    carrier: &LysisListSubtreeCarrierV1,
) -> Result<(), LysisArtifactErrorV1> {
    validate_list_subtree_carrier(carrier)?;
    output.write_u16(carrier.list_kind.id())?;
    output.write_u32(carrier.start_ordinal)?;
    output.write_u32(carrier.real_count)?;
    output.write_u16(carrier.subtree_height)?;
    output.write_u32(carrier.subtree_index)?;
    output.write_b256(carrier.tree_root)?;
    Ok(())
}

fn decode_list_subtree_carrier(
    input: &mut CanonicalReader<'_>,
) -> Result<LysisListSubtreeCarrierV1, LysisArtifactErrorV1> {
    let carrier = LysisListSubtreeCarrierV1 {
        list_kind: ListKind::try_from(input.read_u16()?)?,
        start_ordinal: input.read_u32()?,
        real_count: input.read_u32()?,
        subtree_height: input.read_u16()?,
        subtree_index: input.read_u32()?,
        tree_root: input.read_b256()?,
    };
    validate_list_subtree_carrier(&carrier)?;
    Ok(carrier)
}

fn validate_root_reduce_summary(
    summary: &RootReduceSummaryV1,
) -> Result<(), LysisArtifactErrorV1> {
    if summary.protocol_bundle_hash.is_zero()
        || summary.job_id.is_zero()
        || summary.plan_hash.is_zero()
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "root reducer summary binding",
        ));
    }
    require_carrier(
        &summary.nod_actions,
        ListKind::NodActions,
        summary.tribute_count,
    )?;
    require_carrier(
        &summary.bucket_records,
        ListKind::BucketRecords,
        summary.bucket_count,
    )?;
    require_carrier(
        &summary.contributor_actions,
        ListKind::ContributorActions,
        summary.contributor_count,
    )?;
    require_carrier(
        &summary.output_manifest_entries,
        ListKind::CompleteOutputManifest,
        summary.nod_count,
    )?;
    require_carrier(
        &summary.result_chunk_hashes,
        ListKind::ResultChunkHashes,
        summary.covered_primary_count,
    )?;

    let action_start = summary
        .covered_primary_start
        .checked_mul(PRIMARY_WORK_SHARD_SIZE)
        .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
    if summary.nod_actions.start_ordinal != action_start
        || summary.bucket_records.start_ordinal != action_start
        || summary.contributor_actions.start_ordinal != action_start
        || summary.output_manifest_entries.start_ordinal != action_start
        || summary.result_chunk_hashes.start_ordinal != summary.covered_primary_start
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "root reducer summary list start",
        ));
    }

    if summary.covered_primary_count == 0 {
        if summary.tribute_count != 0
            || summary.nod_count != 0
            || summary.bucket_count != 0
            || summary.contributor_count != 0
            || !summary.tribute_nominal_total.is_zero()
            || !summary.eligible_nominal_total.is_zero()
            || !summary.nod_gratis_consumed.is_zero()
            || !summary.nod_cost_total.is_zero()
            || summary.first_error_ordinal.is_some()
        {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "root reducer empty summary",
            ));
        }
        return Ok(());
    }

    let covered_capacity = summary
        .covered_primary_count
        .checked_mul(PRIMARY_WORK_SHARD_SIZE)
        .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
    let minimum_count = covered_capacity
        .checked_sub(PRIMARY_WORK_SHARD_SIZE - 1)
        .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
    if summary.tribute_count < minimum_count
        || summary.tribute_count > covered_capacity
        || summary.nod_count != summary.tribute_count
        || summary.bucket_count != summary.nod_count
        || summary.contributor_count > summary.tribute_count
        || summary.eligible_nominal_total > summary.tribute_nominal_total
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "root reducer summary counts or totals",
        ));
    }
    if summary.first_error_ordinal.is_some_and(|ordinal| {
        action_start
            .checked_add(summary.tribute_count)
            .is_none_or(|end| ordinal < action_start || ordinal >= end)
    }) {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "root reducer summary first error",
        ));
    }
    Ok(())
}

fn require_carrier(
    carrier: &LysisListSubtreeCarrierV1,
    expected_kind: ListKind,
    expected_count: u32,
) -> Result<(), LysisArtifactErrorV1> {
    validate_list_subtree_carrier(carrier)?;
    if carrier.list_kind != expected_kind || carrier.real_count != expected_count {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "root reducer summary carrier binding",
        ));
    }
    Ok(())
}

fn validate_list_subtree_carrier(
    carrier: &LysisListSubtreeCarrierV1,
) -> Result<(), LysisArtifactErrorV1> {
    if !matches!(
        carrier.list_kind,
        ListKind::NodActions
            | ListKind::BucketRecords
            | ListKind::ContributorActions
            | ListKind::CompleteOutputManifest
            | ListKind::ResultChunkHashes
    ) || carrier.subtree_height > MAX_LIST_SUBTREE_HEIGHT
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "Lysis list subtree kind or height",
        ));
    }
    let capacity = 1_u32
        .checked_shl(u32::from(carrier.subtree_height))
        .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
    let expected_start = carrier
        .subtree_index
        .checked_mul(capacity)
        .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
    if carrier.start_ordinal != expected_start {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "Lysis list subtree position",
        ));
    }
    if carrier.real_count == 0 {
        if !carrier.tree_root.is_zero() || carrier.subtree_height != 0 {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "Lysis empty list subtree",
            ));
        }
        return Ok(());
    }
    let minimum_count = capacity
        .checked_shr(1)
        .unwrap_or_default()
        .checked_add(1)
        .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
    if carrier.real_count < minimum_count
        || carrier.real_count > capacity
        || carrier.tree_root.is_zero()
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "Lysis list subtree canonical size",
        ));
    }
    Ok(())
}
