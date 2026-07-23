use alloy_primitives::B256;

use crate::{
    error::ProtocolError,
    hash::hash_framed,
    registry::{HashDomain, ListKind},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrderedListLimits {
    pub max_items: usize,
    pub max_item_bytes: usize,
    pub max_tree_allocation_bytes: usize,
}

impl OrderedListLimits {
    #[must_use]
    pub const fn new(
        max_items: usize,
        max_item_bytes: usize,
        max_tree_allocation_bytes: usize,
    ) -> Self {
        Self {
            max_items,
            max_item_bytes,
            max_tree_allocation_bytes,
        }
    }
}

pub fn ordered_list_root<T: AsRef<[u8]>>(
    kind: ListKind,
    items: &[T],
    limits: OrderedListLimits,
) -> Result<B256, ProtocolError> {
    check_cap("ordered-list item count", limits.max_items, items.len())?;
    let real_count = u32::try_from(items.len()).map_err(|_| ProtocolError::IntegerOverflow {
        what: "ordered-list item count",
    })?;
    for item in items {
        check_cap(
            "ordered-list item bytes",
            limits.max_item_bytes,
            item.as_ref().len(),
        )?;
        u32::try_from(item.as_ref().len()).map_err(|_| ProtocolError::IntegerOverflow {
            what: "ordered-list item length",
        })?;
    }

    if items.is_empty() {
        return hash_framed(HashDomain::ListEmpty, &kind.id().to_be_bytes());
    }

    let padded_count =
        items
            .len()
            .checked_next_power_of_two()
            .ok_or(ProtocolError::IntegerOverflow {
                what: "ordered-list padded item count",
            })?;
    let allocation_bytes = padded_count
        .checked_mul(core::mem::size_of::<B256>())
        .ok_or(ProtocolError::IntegerOverflow {
            what: "ordered-list tree allocation bytes",
        })?;
    check_cap(
        "ordered-list tree allocation bytes",
        limits.max_tree_allocation_bytes,
        allocation_bytes,
    )?;

    let mut nodes = Vec::new();
    nodes
        .try_reserve_exact(padded_count)
        .map_err(|_| ProtocolError::AllocationFailed {
            what: "ordered-list tree",
            bytes: allocation_bytes,
        })?;
    for (index, item) in items.iter().enumerate() {
        nodes.push(leaf_hash(kind, index_as_u32(index)?, item.as_ref())?);
    }
    for index in items.len()..padded_count {
        nodes.push(pad_hash(kind, index_as_u32(index)?)?);
    }

    let tree_height = u16::try_from(padded_count.trailing_zeros()).map_err(|_| {
        ProtocolError::IntegerOverflow {
            what: "ordered-list tree height",
        }
    })?;
    let mut width = padded_count;
    let mut level = 1_u16;
    while width > 1 {
        let parent_count = width / 2;
        for index in 0..parent_count {
            nodes[index] = node_hash(
                kind,
                level,
                index_as_u32(index)?,
                nodes[index * 2],
                nodes[index * 2 + 1],
            )?;
        }
        width = parent_count;
        level = level.checked_add(1).ok_or(ProtocolError::IntegerOverflow {
            what: "ordered-list node level",
        })?;
    }

    root_hash(kind, real_count, tree_height, nodes[0])
}

pub fn leaf_hash(kind: ListKind, index: u32, item: &[u8]) -> Result<B256, ProtocolError> {
    let item_len = u32::try_from(item.len()).map_err(|_| ProtocolError::IntegerOverflow {
        what: "ordered-list item length",
    })?;
    let capacity = 10_usize
        .checked_add(item.len())
        .ok_or(ProtocolError::IntegerOverflow {
            what: "ordered-list leaf preimage",
        })?;
    let mut payload = Vec::new();
    payload
        .try_reserve_exact(capacity)
        .map_err(|_| ProtocolError::AllocationFailed {
            what: "ordered-list leaf payload",
            bytes: capacity,
        })?;
    payload.extend_from_slice(&kind.id().to_be_bytes());
    payload.extend_from_slice(&index.to_be_bytes());
    payload.extend_from_slice(&item_len.to_be_bytes());
    payload.extend_from_slice(item);
    hash_framed(HashDomain::ListLeaf, &payload)
}

pub fn pad_hash(kind: ListKind, index: u32) -> Result<B256, ProtocolError> {
    let mut payload = [0_u8; 6];
    payload[..2].copy_from_slice(&kind.id().to_be_bytes());
    payload[2..].copy_from_slice(&index.to_be_bytes());
    hash_framed(HashDomain::ListPad, &payload)
}

pub fn node_hash(
    kind: ListKind,
    level: u16,
    index: u32,
    left: B256,
    right: B256,
) -> Result<B256, ProtocolError> {
    let mut payload = [0_u8; 72];
    payload[..2].copy_from_slice(&kind.id().to_be_bytes());
    payload[2..4].copy_from_slice(&level.to_be_bytes());
    payload[4..8].copy_from_slice(&index.to_be_bytes());
    payload[8..40].copy_from_slice(left.as_slice());
    payload[40..].copy_from_slice(right.as_slice());
    hash_framed(HashDomain::ListNode, &payload)
}

pub fn root_hash(
    kind: ListKind,
    real_count: u32,
    tree_height: u16,
    tree_root: B256,
) -> Result<B256, ProtocolError> {
    let mut payload = [0_u8; 40];
    payload[..2].copy_from_slice(&kind.id().to_be_bytes());
    payload[2..6].copy_from_slice(&real_count.to_be_bytes());
    payload[6..8].copy_from_slice(&tree_height.to_be_bytes());
    payload[8..].copy_from_slice(tree_root.as_slice());
    hash_framed(HashDomain::ListRoot, &payload)
}

fn index_as_u32(index: usize) -> Result<u32, ProtocolError> {
    u32::try_from(index).map_err(|_| ProtocolError::IntegerOverflow {
        what: "ordered-list index",
    })
}

fn check_cap(what: &'static str, limit: usize, actual: usize) -> Result<(), ProtocolError> {
    if actual <= limit {
        Ok(())
    } else {
        Err(ProtocolError::CapacityExceeded {
            what,
            limit,
            actual,
        })
    }
}
