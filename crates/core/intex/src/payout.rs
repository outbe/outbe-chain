//! Certified contributor payout: canonical leaf bytes and chunk-range membership.
//!
//! Contributor records are committed by Lysis into an ordered-list tree whose
//! root is the only on-chain authority ([`crate::schema::IntexContract`]
//! `ocomp_contributor_root`). A payout batch carries the records of one result
//! chunk plus the sibling hashes above that chunk's subtree, so the chain
//! rebuilds the root from the supplied bytes instead of trusting the sender.
//!
//! One proof covers up to 256 adjacent leaves: the batch reconstructs the lower
//! eight levels from its own records, and only the levels above need siblings.

use alloy_primitives::{Address, B256, U256};
use outbe_ocomp_protocol::list::{
    leaf_hash, node_hash, pad_hash, root_hash, streaming_ordered_list_membership_proof,
};
use outbe_ocomp_protocol::{ListKind, ProtocolError};
use outbe_primitives::error::{PrecompileError, Result};

use crate::errors::IntexError;

/// Canonical `ContributorActionV1` record: owner(20) ++ tribute id(36) ++ nominal(32).
pub const CONTRIBUTOR_LEAF_BYTES: usize = 88;

/// Result chunks hold at most 256 contributor records, so one batch is a
/// chunk-aligned subtree eight levels deep.
pub const CONTRIBUTOR_CHUNK_HEIGHT: u32 = 8;

/// Maximum leaves in one payout batch.
pub const CONTRIBUTOR_CHUNK_CAPACITY: u32 = 1 << CONTRIBUTOR_CHUNK_HEIGHT;

const KIND: ListKind = ListKind::ContributorActions;

/// One certified contributor record as committed in the Lysis result tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContributorLeafData {
    pub owner: Address,
    pub source_tribute_id: [u8; 36],
    pub nominal: U256,
}

/// Rebuilds the exact canonical bytes the Lysis finalizer hashed into the tree.
pub fn encode_contributor_leaf(leaf: &ContributorLeafData) -> [u8; CONTRIBUTOR_LEAF_BYTES] {
    let mut out = [0u8; CONTRIBUTOR_LEAF_BYTES];
    out[..20].copy_from_slice(leaf.owner.as_slice());
    out[20..56].copy_from_slice(&leaf.source_tribute_id);
    out[56..].copy_from_slice(&leaf.nominal.to_be_bytes::<32>());
    out
}

/// Verifies one chunk-aligned leaf range against the certified contributor root.
///
/// `leaves` must be a full chunk or the final partial chunk, and `start_index`
/// must be chunk-aligned. Padding slots are derived locally, never supplied by
/// the caller.
pub fn verify_contributor_leaf_range(
    real_count: u32,
    start_index: u32,
    leaves: &[[u8; CONTRIBUTOR_LEAF_BYTES]],
    siblings: &[B256],
    expected_root: B256,
) -> Result<()> {
    if expected_root.is_zero() {
        return Err(IntexError::BadContributorBatch("certified root is empty").into());
    }
    let shape = RangeShape::resolve(real_count, start_index, leaves.len(), siblings.len())?;

    let mut nodes: Vec<B256> = Vec::with_capacity(shape.capacity as usize);
    for slot in 0..shape.capacity {
        let index = start_index + slot;
        let hash = match leaves.get(slot as usize) {
            Some(leaf) => leaf_hash(KIND, index, leaf),
            None => pad_hash(KIND, index),
        };
        nodes.push(hash.map_err(protocol_error)?);
    }

    let mut width = shape.capacity;
    let mut level: u16 = 1;
    while width > 1 {
        for j in 0..(width / 2) as usize {
            let parent_index = (start_index >> level) + j as u32;
            nodes[j] = node_hash(KIND, level, parent_index, nodes[2 * j], nodes[2 * j + 1])
                .map_err(protocol_error)?;
        }
        width /= 2;
        level += 1;
    }

    let mut hash = nodes[0];
    let mut position = start_index >> shape.sub_height;
    for (offset, sibling) in siblings.iter().enumerate() {
        let level = u16::try_from(shape.sub_height as usize + offset + 1)
            .map_err(|_| IntexError::BadContributorBatch("proof level overflow"))?;
        let parent_index = position >> 1;
        hash = if position & 1 == 0 {
            node_hash(KIND, level, parent_index, hash, *sibling)
        } else {
            node_hash(KIND, level, parent_index, *sibling, hash)
        }
        .map_err(protocol_error)?;
        position = parent_index;
    }

    let actual = root_hash(KIND, real_count, shape.tree_height, hash).map_err(protocol_error)?;
    if actual != expected_root {
        return Err(IntexError::ContributorProofMismatch.into());
    }
    Ok(())
}

/// Builds the sibling path for one chunk-aligned range by streaming the exact
/// contributor population once.
///
/// Off-chain counterpart of [`verify_contributor_leaf_range`]: it takes any
/// in-range leaf's membership path and drops the levels the batch rebuilds from
/// its own records.
pub fn build_contributor_range_proof<I, T>(
    real_count: u32,
    start_index: u32,
    items: I,
) -> Result<Vec<B256>>
where
    I: IntoIterator<Item = T>,
    T: AsRef<[u8]>,
{
    let shape = RangeShape::resolve_bounds(real_count, start_index)?;
    let mut path = streaming_ordered_list_membership_proof(
        KIND,
        real_count,
        start_index,
        items,
        CONTRIBUTOR_LEAF_BYTES,
    )
    .map_err(protocol_error)?;
    Ok(path.split_off(shape.sub_height as usize))
}

/// Resolved geometry of one payout batch inside the contributor tree.
struct RangeShape {
    /// Slots covered by the batch subtree: `1 << sub_height`.
    capacity: u32,
    /// Levels the batch rebuilds from its own leaves.
    sub_height: u32,
    /// Height of the whole padded tree.
    tree_height: u16,
}

impl RangeShape {
    fn resolve(
        real_count: u32,
        start_index: u32,
        leaf_count: usize,
        sibling_count: usize,
    ) -> Result<Self> {
        let shape = Self::resolve_bounds(real_count, start_index)?;

        if leaf_count == 0 {
            return Err(IntexError::BadContributorBatch("batch carries no leaves").into());
        }
        let len = u32::try_from(leaf_count)
            .map_err(|_| IntexError::BadContributorBatch("batch leaf count overflow"))?;
        if len > shape.capacity {
            return Err(IntexError::BadContributorBatch("batch exceeds chunk capacity").into());
        }
        let end = start_index
            .checked_add(len)
            .ok_or(IntexError::BadContributorBatch("batch range overflow"))?;
        if end > real_count {
            return Err(IntexError::BadContributorBatch(
                "batch reaches past the contributor count",
            )
            .into());
        }
        // A short batch is only legal as the final chunk; otherwise a caller
        // could claim padding slots or split a chunk.
        if len != shape.capacity && end != real_count {
            return Err(IntexError::BadContributorBatch("partial batch is not the tail").into());
        }
        let expected_siblings = usize::from(shape.tree_height) - shape.sub_height as usize;
        if sibling_count != expected_siblings {
            return Err(IntexError::BadContributorBatch("proof height mismatch").into());
        }
        Ok(shape)
    }

    /// Geometry that depends only on the population and the batch start.
    fn resolve_bounds(real_count: u32, start_index: u32) -> Result<Self> {
        if real_count == 0 {
            return Err(IntexError::BadContributorBatch("empty contributor population").into());
        }
        if start_index >= real_count {
            return Err(IntexError::BadContributorBatch("batch starts past the population").into());
        }
        let padded =
            real_count
                .checked_next_power_of_two()
                .ok_or(IntexError::BadContributorBatch(
                    "contributor population overflow",
                ))?;
        let tree_height_bits = padded.trailing_zeros();
        let tree_height = u16::try_from(tree_height_bits)
            .map_err(|_| IntexError::BadContributorBatch("tree height overflow"))?;
        let sub_height = tree_height_bits.min(CONTRIBUTOR_CHUNK_HEIGHT);
        let capacity = 1_u32 << sub_height;
        if start_index % capacity != 0 {
            return Err(IntexError::BadContributorBatch("batch start is not chunk-aligned").into());
        }
        Ok(Self {
            capacity,
            sub_height,
            tree_height,
        })
    }
}

fn protocol_error(error: ProtocolError) -> PrecompileError {
    PrecompileError::Revert(format!("contributor proof: {error}"))
}

#[cfg(any(test, feature = "test-utils"))]
pub mod test_support {
    use super::*;
    use outbe_ocomp_protocol::StreamingOrderedListRoot;

    /// Deterministic contributor record for tests.
    pub fn contributor_leaf(index: u32, nominal: u64) -> ContributorLeafData {
        let mut owner = [0u8; 20];
        owner[16..].copy_from_slice(&index.to_be_bytes());
        let mut source_tribute_id = [0u8; 36];
        source_tribute_id[32..].copy_from_slice(&index.to_be_bytes());
        ContributorLeafData {
            owner: Address::from(owner),
            source_tribute_id,
            nominal: U256::from(nominal),
        }
    }

    pub fn encode_all(leaves: &[ContributorLeafData]) -> Vec<[u8; CONTRIBUTOR_LEAF_BYTES]> {
        leaves.iter().map(encode_contributor_leaf).collect()
    }

    /// Builds the certified root exactly as the Lysis finalizer does.
    pub fn contributor_root(leaves: &[ContributorLeafData]) -> B256 {
        let count = u32::try_from(leaves.len()).expect("contributor count fits u32");
        let mut root = StreamingOrderedListRoot::new(KIND, count).expect("streaming root");
        for encoded in encode_all(leaves) {
            root.push(&encoded, CONTRIBUTOR_LEAF_BYTES)
                .expect("push contributor leaf");
        }
        root.finish().expect("finish contributor root")
    }

    /// Sibling path for the batch starting at `start_index`.
    pub fn contributor_range_proof(leaves: &[ContributorLeafData], start_index: u32) -> Vec<B256> {
        let count = u32::try_from(leaves.len()).expect("contributor count fits u32");
        build_contributor_range_proof(count, start_index, encode_all(leaves))
            .expect("build range proof")
    }

    /// Packed certified-generation selector: version (low u64) | count << 64.
    pub fn metadata_word(series_version: u64, contributor_count: u32) -> U256 {
        U256::from(series_version) | (U256::from(contributor_count) << 64)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use outbe_ocomp_protocol::common::EntityId36;
    use outbe_ocomp_protocol::profile::poc_schema_limits;
    use outbe_ocomp_protocol::result::ContributorActionV1;

    fn batch(
        leaves: &[ContributorLeafData],
        start: u32,
        len: u32,
    ) -> Vec<[u8; CONTRIBUTOR_LEAF_BYTES]> {
        encode_all(&leaves[start as usize..(start + len) as usize])
    }

    fn population(count: u32) -> Vec<ContributorLeafData> {
        (0..count)
            .map(|i| contributor_leaf(i, u64::from(i) + 1))
            .collect()
    }

    #[test]
    fn canonical_leaf_encoding_matches_protocol_codec() {
        let leaf = contributor_leaf(7, 12_345);
        let action = ContributorActionV1 {
            owner: leaf.owner,
            source_tribute_id: EntityId36(leaf.source_tribute_id),
            nominal_amount_minor: leaf.nominal,
        };
        let expected = action
            .encode_canonical_record(&poc_schema_limits())
            .expect("canonical record");
        assert_eq!(
            encode_contributor_leaf(&leaf).as_slice(),
            expected.as_slice()
        );
    }

    #[test]
    fn full_chunk_and_tail_verify() {
        let leaves = population(600);
        let root = contributor_root(&leaves);

        for start in [0_u32, 256, 512] {
            let len = 256.min(600 - start);
            verify_contributor_leaf_range(
                600,
                start,
                &batch(&leaves, start, len),
                &contributor_range_proof(&leaves, start),
                root,
            )
            .unwrap_or_else(|e| panic!("chunk at {start} must verify: {e:?}"));
        }
    }

    #[test]
    fn small_tree_and_single_leaf_verify() {
        for count in [1_u32, 5, 256] {
            let leaves = population(count);
            let root = contributor_root(&leaves);
            verify_contributor_leaf_range(
                count,
                0,
                &batch(&leaves, 0, count),
                &contributor_range_proof(&leaves, 0),
                root,
            )
            .unwrap_or_else(|e| panic!("population {count} must verify: {e:?}"));
        }
    }

    #[test]
    fn misaligned_start_is_rejected() {
        let leaves = population(600);
        let root = contributor_root(&leaves);
        let err = verify_contributor_leaf_range(
            600,
            128,
            &batch(&leaves, 128, 256),
            &contributor_range_proof(&leaves, 0),
            root,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("chunk-aligned"), "{err:?}");
    }

    #[test]
    fn partial_non_tail_batch_is_rejected() {
        let leaves = population(600);
        let root = contributor_root(&leaves);
        let err = verify_contributor_leaf_range(
            600,
            0,
            &batch(&leaves, 0, 100),
            &contributor_range_proof(&leaves, 0),
            root,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("not the tail"), "{err:?}");
    }

    #[test]
    fn range_past_population_is_rejected() {
        let leaves = population(600);
        let root = contributor_root(&leaves);
        let mut oversized = batch(&leaves, 512, 88);
        oversized.push(encode_contributor_leaf(&contributor_leaf(600, 1)));
        let err = verify_contributor_leaf_range(
            600,
            512,
            &oversized,
            &contributor_range_proof(&leaves, 512),
            root,
        )
        .unwrap_err();
        assert!(
            format!("{err:?}").contains("past the contributor count"),
            "{err:?}"
        );
    }

    #[test]
    fn wrong_sibling_count_is_rejected() {
        let leaves = population(600);
        let root = contributor_root(&leaves);
        let mut proof = contributor_range_proof(&leaves, 0);
        proof.pop();
        let err = verify_contributor_leaf_range(600, 0, &batch(&leaves, 0, 256), &proof, root)
            .unwrap_err();
        assert!(format!("{err:?}").contains("proof height"), "{err:?}");
    }

    #[test]
    fn tampered_leaf_is_rejected() {
        let leaves = population(600);
        let root = contributor_root(&leaves);
        let mut tampered = batch(&leaves, 0, 256);
        tampered[3][87] ^= 1;
        let err = verify_contributor_leaf_range(
            600,
            0,
            &tampered,
            &contributor_range_proof(&leaves, 0),
            root,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("does not match"), "{err:?}");
    }

    #[test]
    fn foreign_root_is_rejected() {
        let leaves = population(600);
        let other = contributor_root(&population(601));
        let err = verify_contributor_leaf_range(
            600,
            0,
            &batch(&leaves, 0, 256),
            &contributor_range_proof(&leaves, 0),
            other,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("does not match"), "{err:?}");
    }

    #[test]
    fn empty_population_and_empty_batch_are_rejected() {
        let leaves = population(600);
        let root = contributor_root(&leaves);
        assert!(verify_contributor_leaf_range(0, 0, &batch(&leaves, 0, 1), &[], root).is_err());
        assert!(verify_contributor_leaf_range(
            600,
            0,
            &[],
            &contributor_range_proof(&leaves, 0),
            root
        )
        .is_err());
    }
}
