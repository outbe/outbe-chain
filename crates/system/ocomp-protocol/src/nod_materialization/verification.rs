use super::*;
use crate::list::{leaf_hash, node_hash, pad_hash};

pub(super) struct BatchShape {
    pub(super) tree_height: u16,
    actual_subtree_height: u16,
    capacity: u32,
    expected_actions: u32,
}

pub(super) fn validate_shape(
    batch: &NodMaterializationBatchV1,
    head: &NodMaterializationHeadV1,
    configured_subtree_height: u8,
) -> Result<BatchShape, ProtocolError> {
    require(
        batch.queue_sequence == head.queue_sequence,
        "materialization queue binding",
    )?;
    require(
        batch.first_nod_ordinal == head.next_nod_ordinal,
        "materialization cursor binding",
    )?;

    let padded_count =
        head.nod_count
            .checked_next_power_of_two()
            .ok_or(ProtocolError::IntegerOverflow {
                what: "materialization padded NOD count",
            })?;
    let tree_height = u16::try_from(padded_count.trailing_zeros()).map_err(|_| {
        ProtocolError::IntegerOverflow {
            what: "materialization tree height",
        }
    })?;
    let actual_subtree_height = tree_height
        .checked_sub(batch.root_path.len() as u16)
        .ok_or(ProtocolError::InvalidInvariant(
            "materialization root path exceeds tree height",
        ))?;
    require(
        actual_subtree_height <= u16::from(configured_subtree_height),
        "materialization subtree exceeds configured height",
    )?;
    let capacity = 1_u32.checked_shl(u32::from(actual_subtree_height)).ok_or(
        ProtocolError::IntegerOverflow {
            what: "materialization batch capacity",
        },
    )?;
    require(
        capacity <= MAX_NOD_MATERIALIZATION_ACTIONS as u32,
        "materialization configured capacity",
    )?;
    require(
        batch.first_nod_ordinal.is_multiple_of(capacity),
        "materialization cursor alignment",
    )?;
    let remaining = head.nod_count.checked_sub(head.next_nod_ordinal).ok_or(
        ProtocolError::InvalidInvariant("materialization remaining NOD count"),
    )?;
    let expected_actions = remaining.min(capacity);
    require(
        batch.actions.len() == expected_actions as usize,
        "materialization exact batch action count",
    )?;

    Ok(BatchShape {
        tree_height,
        actual_subtree_height,
        capacity,
        expected_actions,
    })
}

pub(super) fn subtree_hash(
    batch: &NodMaterializationBatchV1,
    head: &NodMaterializationHeadV1,
    shape: &BatchShape,
    limits: &SchemaLimits,
) -> Result<B256, ProtocolError> {
    let mut nodes = Vec::with_capacity(shape.capacity as usize);
    for offset in 0..shape.capacity {
        let ordinal =
            batch
                .first_nod_ordinal
                .checked_add(offset)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "materialization NOD ordinal",
                })?;
        if offset < shape.expected_actions {
            let action = &batch.actions[offset as usize];
            require(
                action.raw_ordinal == ordinal && action.wwd == head.worldwide_day,
                "materialization ordered action binding",
            )?;
            nodes.push(leaf_hash(
                ListKind::NodActions,
                ordinal,
                &action.encode_canonical_record(limits)?,
            )?);
        } else {
            nodes.push(pad_hash(ListKind::NodActions, ordinal)?);
        }
    }

    let mut width = shape.capacity as usize;
    let mut level = 1_u16;
    while width > 1 {
        for index in 0..width / 2 {
            let global_index = (batch.first_nod_ordinal >> level)
                .checked_add(
                    u32::try_from(index).map_err(|_| ProtocolError::IntegerOverflow {
                        what: "materialization subtree index",
                    })?,
                )
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "materialization global subtree index",
                })?;
            nodes[index] = node_hash(
                ListKind::NodActions,
                level,
                global_index,
                nodes[index * 2],
                nodes[index * 2 + 1],
            )?;
        }
        width /= 2;
        level = level.checked_add(1).ok_or(ProtocolError::IntegerOverflow {
            what: "materialization subtree level",
        })?;
    }

    Ok(nodes[0])
}

pub(super) fn root_from_path(
    batch: &NodMaterializationBatchV1,
    shape: &BatchShape,
    mut hash: B256,
) -> Result<B256, ProtocolError> {
    let mut position = batch.first_nod_ordinal >> shape.actual_subtree_height;
    for (offset, sibling) in batch.root_path.iter().enumerate() {
        let child_level = shape
            .actual_subtree_height
            .checked_add(
                u16::try_from(offset).map_err(|_| ProtocolError::IntegerOverflow {
                    what: "materialization root path level",
                })?,
            )
            .ok_or(ProtocolError::IntegerOverflow {
                what: "materialization root path level",
            })?;
        let parent_level = child_level
            .checked_add(1)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "materialization parent level",
            })?;
        hash = if position & 1 == 0 {
            node_hash(
                ListKind::NodActions,
                parent_level,
                position >> 1,
                hash,
                *sibling,
            )?
        } else {
            node_hash(
                ListKind::NodActions,
                parent_level,
                position >> 1,
                *sibling,
                hash,
            )?
        };
        position >>= 1;
    }
    Ok(hash)
}
