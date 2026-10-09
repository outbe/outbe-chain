use alloy_primitives::B256;

use crate::{
    codec::check_cap,
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

/// Incremental implementation of the frozen ordered-list commitment.
///
/// The caller declares the exact population up front. This type accepts leaves
/// in canonical index order. It retains only one pending hash per tree level, so
/// memory use is independent of the complete catalog size.
pub struct StreamingOrderedListRoot {
    kind: ListKind,
    expected_count: u32,
    pushed_count: u32,
    frontier: [Option<B256>; 33],
}

impl StreamingOrderedListRoot {
    pub fn new(kind: ListKind, expected_count: u32) -> Result<Self, ProtocolError> {
        if expected_count > 0 {
            expected_count
                .checked_next_power_of_two()
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "streaming ordered-list padded item count",
                })?;
        }
        Ok(Self {
            kind,
            expected_count,
            pushed_count: 0,
            frontier: [None; 33],
        })
    }

    pub fn push(&mut self, item: &[u8], max_item_bytes: usize) -> Result<(), ProtocolError> {
        if self.pushed_count >= self.expected_count {
            return Err(ProtocolError::InvalidInvariant(
                "streaming ordered-list exact item count",
            ));
        }
        check_cap("ordered-list item bytes", max_item_bytes, item.len())?;
        let hash = leaf_hash(self.kind, self.pushed_count, item)?;
        self.push_hash(self.pushed_count, hash)?;
        self.pushed_count += 1;
        Ok(())
    }

    pub fn finish(mut self) -> Result<B256, ProtocolError> {
        if self.pushed_count != self.expected_count {
            return Err(ProtocolError::InvalidInvariant(
                "streaming ordered-list exact item count",
            ));
        }
        if self.expected_count == 0 {
            return hash_framed(HashDomain::ListEmpty, &self.kind.id().to_be_bytes());
        }

        let padded_count = self.expected_count.checked_next_power_of_two().ok_or(
            ProtocolError::IntegerOverflow {
                what: "streaming ordered-list padded item count",
            },
        )?;
        for index in self.expected_count..padded_count {
            self.push_hash(index, pad_hash(self.kind, index)?)?;
            self.pushed_count += 1;
        }
        let tree_height = u16::try_from(padded_count.trailing_zeros()).map_err(|_| {
            ProtocolError::IntegerOverflow {
                what: "streaming ordered-list tree height",
            }
        })?;
        let tree_root = self.frontier[usize::from(tree_height)].ok_or(
            ProtocolError::InvalidInvariant("streaming ordered-list complete tree"),
        )?;
        root_hash(self.kind, self.expected_count, tree_height, tree_root)
    }

    fn push_hash(&mut self, index: u32, mut hash: B256) -> Result<(), ProtocolError> {
        let mut position = index;
        let mut level = 0_usize;
        loop {
            if position & 1 == 0 {
                self.frontier[level] = Some(hash);
                return Ok(());
            }
            let left = self.frontier[level]
                .take()
                .ok_or(ProtocolError::InvalidInvariant(
                    "streaming ordered-list left sibling",
                ))?;
            let parent_level =
                u16::try_from(level + 1).map_err(|_| ProtocolError::IntegerOverflow {
                    what: "streaming ordered-list parent level",
                })?;
            hash = node_hash(self.kind, parent_level, position >> 1, left, hash)?;
            position >>= 1;
            level += 1;
        }
    }
}

pub fn ordered_list_root<T: AsRef<[u8]>>(
    kind: ListKind,
    items: &[T],
    limits: OrderedListLimits,
) -> Result<B256, ProtocolError> {
    let real_count = validate_ordered_list_items(items, limits)?;
    if items.is_empty() {
        return hash_framed(HashDomain::ListEmpty, &kind.id().to_be_bytes());
    }
    let mut nodes = build_ordered_list_nodes(kind, items, limits.max_tree_allocation_bytes)?;
    let (tree_height, tree_root) = fold_ordered_list_nodes(kind, &mut nodes)?;
    root_hash(kind, real_count, tree_height, tree_root)
}

fn validate_ordered_list_items<T: AsRef<[u8]>>(
    items: &[T],
    limits: OrderedListLimits,
) -> Result<u32, ProtocolError> {
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
    Ok(real_count)
}

fn build_ordered_list_nodes<T: AsRef<[u8]>>(
    kind: ListKind,
    items: &[T],
    max_tree_allocation_bytes: usize,
) -> Result<Vec<B256>, ProtocolError> {
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
        max_tree_allocation_bytes,
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
    Ok(nodes)
}

fn fold_ordered_list_nodes(
    kind: ListKind,
    nodes: &mut [B256],
) -> Result<(u16, B256), ProtocolError> {
    let padded_count = nodes.len();
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
    Ok((tree_height, nodes[0]))
}

/// Exact population and leaf position for a streaming membership proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrderedListProofTarget {
    kind: ListKind,
    real_count: u32,
    target_index: u32,
}

impl OrderedListProofTarget {
    #[must_use]
    pub const fn new(kind: ListKind, real_count: u32, target_index: u32) -> Self {
        Self {
            kind,
            real_count,
            target_index,
        }
    }
}

/// Builds one bottom-up membership path while streaming the exact ordered
/// population once. Memory is bounded by the tree height and does not grow
/// with the population size.
pub fn streaming_ordered_list_membership_proof<I, T>(
    target: OrderedListProofTarget,
    items: I,
    max_item_bytes: usize,
) -> Result<Vec<B256>, ProtocolError>
where
    I: IntoIterator<Item = T>,
    T: AsRef<[u8]>,
{
    try_streaming_ordered_list_membership_proof(
        target,
        items.into_iter().map(Ok::<T, ProtocolError>),
        max_item_bytes,
    )
}

/// Fallible-input form of [`streaming_ordered_list_membership_proof`].
///
/// This lets a disk-backed catalog validate and encode each item lazily while
/// preserving its own typed error and the same bounded frontier memory.
pub fn try_streaming_ordered_list_membership_proof<I, T, E>(
    target: OrderedListProofTarget,
    items: I,
    max_item_bytes: usize,
) -> Result<Vec<B256>, E>
where
    I: IntoIterator<Item = Result<T, E>>,
    T: AsRef<[u8]>,
    E: From<ProtocolError>,
{
    let mut proof = MembershipProofBuilder::new(target, max_item_bytes).map_err(E::from)?;
    let mut input = items.into_iter();
    for index in 0..target.real_count {
        let item = input
            .next()
            .ok_or(ProtocolError::InvalidInvariant(
                "streaming ordered-list membership exact item count",
            ))
            .map_err(E::from)??;
        proof.push_real(index, &item).map_err(E::from)?;
    }
    if input.next().is_some() {
        return Err(ProtocolError::InvalidInvariant(
            "streaming ordered-list membership exact item count",
        )
        .into());
    }
    for index in target.real_count..proof.padded_count {
        proof.push_padding(index).map_err(E::from)?;
    }
    proof.finish().map_err(E::from)
}

struct MembershipProofBuilder {
    target: OrderedListProofTarget,
    max_item_bytes: usize,
    padded_count: u32,
    tree_height: usize,
    frontier: [Option<MembershipFrontierNode>; u32::BITS as usize + 1],
    siblings: Vec<B256>,
}

impl MembershipProofBuilder {
    fn new(target: OrderedListProofTarget, max_item_bytes: usize) -> Result<Self, ProtocolError> {
        if target.real_count == 0 || target.target_index >= target.real_count {
            return Err(ProtocolError::InvalidInvariant(
                "streaming ordered-list membership bounds",
            ));
        }
        let padded_count = target.real_count.checked_next_power_of_two().ok_or(
            ProtocolError::IntegerOverflow {
                what: "streaming ordered-list membership padded count",
            },
        )?;
        let tree_height = usize::try_from(padded_count.trailing_zeros()).map_err(|_| {
            ProtocolError::IntegerOverflow {
                what: "streaming ordered-list membership tree height",
            }
        })?;
        let frontier = [None; u32::BITS as usize + 1];
        let mut siblings = Vec::new();
        let proof_bytes = tree_height
            .checked_mul(core::mem::size_of::<B256>())
            .ok_or(ProtocolError::IntegerOverflow {
                what: "streaming ordered-list membership proof bytes",
            })?;
        siblings
            .try_reserve_exact(tree_height)
            .map_err(|_| ProtocolError::AllocationFailed {
                what: "streaming ordered-list membership proof",
                bytes: proof_bytes,
            })?;
        Ok(Self {
            target,
            max_item_bytes,
            padded_count,
            tree_height,
            frontier,
            siblings,
        })
    }

    fn push_real<T: AsRef<[u8]>>(&mut self, index: u32, item: &T) -> Result<(), ProtocolError> {
        check_cap(
            "ordered-list item bytes",
            self.max_item_bytes,
            item.as_ref().len(),
        )?;
        let hash = leaf_hash(self.target.kind, index, item.as_ref())?;
        let contains_target = index == self.target.target_index;
        self.push_hash(index, hash, contains_target)
    }

    fn push_padding(&mut self, index: u32) -> Result<(), ProtocolError> {
        let hash = pad_hash(self.target.kind, index)?;
        self.push_hash(index, hash, false)
    }

    fn push_hash(
        &mut self,
        index: u32,
        mut hash: B256,
        mut contains_target: bool,
    ) -> Result<(), ProtocolError> {
        let mut position = index;
        let mut level = 0_usize;
        loop {
            if position & 1 == 0 {
                self.frontier[level] = Some(MembershipFrontierNode {
                    hash,
                    contains_target,
                });
                return Ok(());
            }
            let left = self.frontier[level]
                .take()
                .ok_or(ProtocolError::InvalidInvariant(
                    "streaming ordered-list membership left sibling",
                ))?;
            if left.contains_target {
                self.siblings.push(hash);
            } else if contains_target {
                self.siblings.push(left.hash);
            }
            contains_target |= left.contains_target;
            let parent_level =
                u16::try_from(level + 1).map_err(|_| ProtocolError::IntegerOverflow {
                    what: "streaming ordered-list membership parent level",
                })?;
            hash = node_hash(
                self.target.kind,
                parent_level,
                position >> 1,
                left.hash,
                hash,
            )?;
            position >>= 1;
            level += 1;
        }
    }

    fn finish(self) -> Result<Vec<B256>, ProtocolError> {
        if self.siblings.len() != self.tree_height
            || self.frontier[self.tree_height].is_none_or(|node| !node.contains_target)
        {
            return Err(ProtocolError::InvalidInvariant(
                "streaming ordered-list membership complete tree",
            ));
        }
        Ok(self.siblings)
    }
}

#[derive(Clone, Copy)]
struct MembershipFrontierNode {
    hash: B256,
    contains_target: bool,
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

/// Verifies one real leaf against the frozen ordered-list commitment without
/// materializing the complete catalog. Siblings are ordered bottom-up.
pub fn verify_ordered_list_membership(
    target: OrderedListProofTarget,
    item: &[u8],
    siblings: &[B256],
    expected_root: B256,
) -> Result<(), ProtocolError> {
    let OrderedListProofTarget {
        kind,
        real_count,
        target_index: index,
    } = target;
    if real_count == 0 || index >= real_count || expected_root.is_zero() {
        return Err(ProtocolError::InvalidInvariant(
            "ordered-list membership bounds",
        ));
    }
    let padded_count =
        real_count
            .checked_next_power_of_two()
            .ok_or(ProtocolError::IntegerOverflow {
                what: "ordered-list membership padded count",
            })?;
    let tree_height = u16::try_from(padded_count.trailing_zeros()).map_err(|_| {
        ProtocolError::IntegerOverflow {
            what: "ordered-list membership tree height",
        }
    })?;
    if siblings.len() != usize::from(tree_height) {
        return Err(ProtocolError::InvalidInvariant(
            "ordered-list membership proof height",
        ));
    }

    let mut position = index;
    let mut hash = leaf_hash(kind, index, item)?;
    for (offset, sibling) in siblings.iter().enumerate() {
        let level = u16::try_from(offset + 1).map_err(|_| ProtocolError::IntegerOverflow {
            what: "ordered-list membership level",
        })?;
        let parent_index = position >> 1;
        hash = if position & 1 == 0 {
            node_hash(kind, level, parent_index, hash, *sibling)?
        } else {
            node_hash(kind, level, parent_index, *sibling, hash)?
        };
        position = parent_index;
    }
    let actual_root = root_hash(kind, real_count, tree_height, hash)?;
    if actual_root != expected_root {
        return Err(ProtocolError::InvalidInvariant(
            "ordered-list membership root",
        ));
    }
    Ok(())
}

fn index_as_u32(index: usize) -> Result<u32, ProtocolError> {
    u32::try_from(index).map_err(|_| ProtocolError::IntegerOverflow {
        what: "ordered-list index",
    })
}
