use alloy_primitives::B256;
use outbe_compressed_entities::WwdEntityId;
use outbe_ocomp_protocol::list::{
    leaf_hash, node_hash, ordered_list_root, pad_hash, root_hash, OrderedListLimits,
};
use outbe_ocomp_protocol::registry::ListKind;
use outbe_ocomp_protocol::{CanonicalReader, CanonicalWriter, SchemaLimits};

use super::{
    validate_shard_size, LysisArtifactErrorV1, RawCoverageCarrierV1, COVERAGE_RECORD_BYTES,
    PRIMARY_SUBTREE_HEIGHT, PRIMARY_WORK_SHARD_SIZE, RAW_COVERAGE_CARRIER_MAGIC,
};

impl RawCoverageCarrierV1 {
    pub fn from_records(
        total_count: u32,
        records: &[(u32, WwdEntityId)],
    ) -> Result<Self, LysisArtifactErrorV1> {
        if total_count == 0 || records.is_empty() {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "raw coverage carrier records",
            ));
        }
        validate_shard_size(records.len())?;

        let start_ordinal = records[0].0;
        let expected_count =
            PRIMARY_WORK_SHARD_SIZE.min(total_count.checked_sub(start_ordinal).ok_or(
                LysisArtifactErrorV1::InvalidEncoding("raw coverage carrier range"),
            )?);
        if !start_ordinal.is_multiple_of(PRIMARY_WORK_SHARD_SIZE)
            || records.len() != expected_count as usize
            || records.iter().enumerate().any(|(offset, (ordinal, _))| {
                u32::try_from(offset)
                    .ok()
                    .and_then(|offset| start_ordinal.checked_add(offset))
                    != Some(*ordinal)
            })
            || records.windows(2).any(|pair| pair[0].1 >= pair[1].1)
        {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "raw coverage carrier canonical records",
            ));
        }

        let primary_count = total_count.div_ceil(PRIMARY_WORK_SHARD_SIZE);
        let subtree_height = if primary_count == 1 {
            u16::try_from(
                total_count
                    .checked_next_power_of_two()
                    .ok_or(LysisArtifactErrorV1::LengthOverflow)?
                    .trailing_zeros(),
            )
            .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?
        } else {
            PRIMARY_SUBTREE_HEIGHT
        };
        let subtree_index = start_ordinal >> subtree_height;
        let tree_root =
            raw_coverage_subtree_root(start_ordinal, subtree_height, total_count, records)?;
        Ok(Self {
            start_ordinal,
            end_ordinal: start_ordinal
                .checked_add(expected_count)
                .ok_or(LysisArtifactErrorV1::LengthOverflow)?,
            subtree_height,
            subtree_index,
            tree_root,
        })
    }

    pub fn canonical_empty(
        total_count: u32,
        primary_ordinal: u32,
    ) -> Result<Self, LysisArtifactErrorV1> {
        let primary_count = total_count.div_ceil(PRIMARY_WORK_SHARD_SIZE);
        let padded_primary_count = primary_count
            .checked_next_power_of_two()
            .ok_or(LysisArtifactErrorV1::LengthOverflow)?
            .max(2);
        if primary_ordinal < primary_count || primary_ordinal >= padded_primary_count {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "raw coverage empty carrier position",
            ));
        }
        let start = primary_ordinal
            .checked_mul(PRIMARY_WORK_SHARD_SIZE)
            .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
        Ok(Self {
            start_ordinal: total_count,
            end_ordinal: total_count,
            subtree_height: PRIMARY_SUBTREE_HEIGHT,
            subtree_index: primary_ordinal,
            tree_root: raw_coverage_subtree_root(start, PRIMARY_SUBTREE_HEIGHT, total_count, &[])?,
        })
    }

    pub fn merge(left: &Self, right: &Self) -> Result<Self, LysisArtifactErrorV1> {
        validate_raw_coverage_carrier(left)?;
        validate_raw_coverage_carrier(right)?;
        let expected_right_index = left
            .subtree_index
            .checked_add(1)
            .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
        if left.subtree_height != right.subtree_height
            || left.subtree_index & 1 != 0
            || right.subtree_index != expected_right_index
            || left.end_ordinal != right.start_ordinal
        {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "raw coverage carrier adjacency",
            ));
        }
        let subtree_height = left
            .subtree_height
            .checked_add(1)
            .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
        let subtree_index = left.subtree_index >> 1;
        Ok(Self {
            start_ordinal: left.start_ordinal,
            end_ordinal: right.end_ordinal,
            subtree_height,
            subtree_index,
            tree_root: node_hash(
                ListKind::RawTributeCoverage,
                subtree_height,
                subtree_index,
                left.tree_root,
                right.tree_root,
            )?,
        })
    }

    pub fn final_root(&self, total_count: u32) -> Result<B256, LysisArtifactErrorV1> {
        validate_raw_coverage_carrier(self)?;
        if total_count == 0
            || self.start_ordinal != 0
            || self.end_ordinal != total_count
            || self.subtree_index != 0
        {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "raw coverage final carrier range",
            ));
        }
        let tree_height = u16::try_from(
            total_count
                .checked_next_power_of_two()
                .ok_or(LysisArtifactErrorV1::LengthOverflow)?
                .trailing_zeros(),
        )
        .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
        if self.subtree_height != tree_height {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "raw coverage final carrier height",
            ));
        }
        root_hash(
            ListKind::RawTributeCoverage,
            total_count,
            tree_height,
            self.tree_root,
        )
        .map_err(LysisArtifactErrorV1::Protocol)
    }
}

pub fn encode_raw_coverage_carrier(
    carrier: &RawCoverageCarrierV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, LysisArtifactErrorV1> {
    validate_raw_coverage_carrier(carrier)?;
    let mut output = CanonicalWriter::new(limits.codec);
    output.write_fixed(&RAW_COVERAGE_CARRIER_MAGIC)?;
    output.write_u32(carrier.start_ordinal)?;
    output.write_u32(carrier.end_ordinal)?;
    output.write_u16(carrier.subtree_height)?;
    output.write_u32(carrier.subtree_index)?;
    output.write_b256(carrier.tree_root)?;
    Ok(output.into_bytes())
}

pub fn decode_raw_coverage_carrier(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<RawCoverageCarrierV1, LysisArtifactErrorV1> {
    let mut input = CanonicalReader::new(encoded, limits.codec)?;
    if input.read_fixed::<4>()? != RAW_COVERAGE_CARRIER_MAGIC {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "raw coverage carrier header",
        ));
    }
    let carrier = RawCoverageCarrierV1 {
        start_ordinal: input.read_u32()?,
        end_ordinal: input.read_u32()?,
        subtree_height: input.read_u16()?,
        subtree_index: input.read_u32()?,
        tree_root: input.read_b256()?,
    };
    input.finish()?;
    validate_raw_coverage_carrier(&carrier)?;
    if encode_raw_coverage_carrier(&carrier, limits)? != encoded {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "raw coverage carrier canonical re-encoding",
        ));
    }
    Ok(carrier)
}

pub(super) fn validate_raw_coverage_carrier(
    carrier: &RawCoverageCarrierV1,
) -> Result<(), LysisArtifactErrorV1> {
    if carrier.start_ordinal > carrier.end_ordinal
        || carrier.subtree_height > 32
        || carrier.tree_root.is_zero()
        || (carrier.subtree_height == 32 && carrier.subtree_index != 0)
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "raw coverage carrier fields",
        ));
    }
    Ok(())
}

fn raw_coverage_subtree_root(
    subtree_start: u32,
    subtree_height: u16,
    total_count: u32,
    records: &[(u32, WwdEntityId)],
) -> Result<B256, LysisArtifactErrorV1> {
    let width = 1_u32
        .checked_shl(u32::from(subtree_height))
        .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
    if width > PRIMARY_WORK_SHARD_SIZE
        || !subtree_start.is_multiple_of(width)
        || records.len() > width as usize
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "raw coverage subtree geometry",
        ));
    }

    let mut nodes = Vec::new();
    nodes
        .try_reserve_exact(width as usize)
        .map_err(|_| LysisArtifactErrorV1::LengthOverflow)?;
    for offset in 0..width {
        let raw_ordinal = subtree_start
            .checked_add(offset)
            .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
        if raw_ordinal < total_count {
            let record_offset = raw_ordinal
                .checked_sub(subtree_start)
                .ok_or(LysisArtifactErrorV1::LengthOverflow)?
                as usize;
            let (record_ordinal, tribute_id) =
                records
                    .get(record_offset)
                    .ok_or(LysisArtifactErrorV1::InvalidEncoding(
                        "raw coverage subtree record",
                    ))?;
            if *record_ordinal != raw_ordinal {
                return Err(LysisArtifactErrorV1::InvalidEncoding(
                    "raw coverage subtree ordinal",
                ));
            }
            let mut encoded = [0_u8; COVERAGE_RECORD_BYTES];
            encoded[..4].copy_from_slice(&raw_ordinal.to_be_bytes());
            encoded[4..].copy_from_slice(tribute_id.as_slice());
            nodes.push(leaf_hash(
                ListKind::RawTributeCoverage,
                raw_ordinal,
                &encoded,
            )?);
        } else {
            nodes.push(pad_hash(ListKind::RawTributeCoverage, raw_ordinal)?);
        }
    }
    if nodes.is_empty() {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "raw coverage subtree width",
        ));
    }

    let mut current_width = width as usize;
    let mut level = 1_u16;
    while current_width > 1 {
        let parent_count = current_width / 2;
        let first_parent_index = subtree_start >> level;
        for index in 0..parent_count {
            let parent_index = first_parent_index
                .checked_add(
                    u32::try_from(index).map_err(|_| LysisArtifactErrorV1::LengthOverflow)?,
                )
                .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
            nodes[index] = node_hash(
                ListKind::RawTributeCoverage,
                level,
                parent_index,
                nodes[index * 2],
                nodes[index * 2 + 1],
            )?;
        }
        current_width = parent_count;
        level = level
            .checked_add(1)
            .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
    }
    Ok(nodes[0])
}

pub(super) fn raw_coverage_root(
    records: impl IntoIterator<Item = (u32, WwdEntityId)>,
) -> Result<B256, LysisArtifactErrorV1> {
    let mut encoded_records = Vec::new();
    for (raw_ordinal, tribute_id) in records {
        if encoded_records.len() >= PRIMARY_WORK_SHARD_SIZE as usize {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "raw coverage item cap",
            ));
        }
        let mut encoded = [0_u8; COVERAGE_RECORD_BYTES];
        encoded[..4].copy_from_slice(&raw_ordinal.to_be_bytes());
        encoded[4..].copy_from_slice(tribute_id.as_slice());
        encoded_records.push(encoded);
    }
    ordered_list_root(
        ListKind::RawTributeCoverage,
        &encoded_records,
        OrderedListLimits {
            max_items: PRIMARY_WORK_SHARD_SIZE as usize,
            max_item_bytes: COVERAGE_RECORD_BYTES,
            max_tree_allocation_bytes: PRIMARY_WORK_SHARD_SIZE as usize
                * core::mem::size_of::<B256>(),
        },
    )
    .map_err(LysisArtifactErrorV1::Protocol)
}
