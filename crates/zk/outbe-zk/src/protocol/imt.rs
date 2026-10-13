//! Incremental Merkle Tree (IMT) over BN254 and Poseidon2.
//!
//! [`Imt`] is a binary, append-only Merkle tree whose node hash is
//! `Poseidon2([domain, left, right])`. The caller supplies the domain,
//! empty leaf, and depth. [`InclusionPath`] is one membership proof (domain,
//! leaf index, and sibling hashes) that resolves a root and yields the
//! in-circuit direction bits.
//!
//! With `empty_leaf = 0`, this mirrors the on-chain commitment tree (`CommitmentWindowBase` in
//! outbe-chain-research): same shape, same domain-separated node hash, same
//! `empty_leaf = 0`, and the leaf stored at each position is the entity hash
//! directly. The depth and domain are caller parameters (the canonical
//! commitment tree fixes both).

use crate::error::Error;
use crate::{primitive::hash, Fr};

/// Result of a single frontier append — mirrors the on-chain `0x0204`
/// precompile output `(changedLevel, newSubtree, newRoot)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Append {
    /// The single frontier level whose stored subtree changes.
    pub changed_level: usize,
    /// The new value to store at `frontier[changed_level]`.
    pub new_subtree: Fr,
    /// The tree root after the append operation.
    pub new_root: Fr,
}

/// A binary, append-only incremental Merkle tree
/// with stored node levels for O(depth) appends and membership paths,
/// O(1) root reads, and O(leaves + depth) storage.
pub struct Imt {
    domain: Fr,
    depth: usize,
    /// Empty subtree roots by height, `depth + 1` entries.
    /// `empty_roots[0]` is the empty leaf.
    empty_roots: Vec<Fr>,
    /// Occupied nodes, bottom-up: leaves at level 0, root at level `depth`.
    /// Missing siblings use `empty_roots[level]` instead of occupying storage.
    levels: Vec<Vec<Fr>>,
}

impl Imt {
    fn validate_depth(depth: usize) -> Result<(), Error> {
        if (1..=63).contains(&depth) {
            return Ok(());
        }
        Err(Error::Merkle(format!(
            "tree depth must be in 1..=63, got {depth}"
        )))
    }

    /// Parent node hash: `Hash([domain, left, right])`.
    pub fn node_hash(domain: Fr, left: Fr, right: Fr) -> Result<Fr, Error> {
        hash::poseidon2(&[domain, left, right])
    }

    /// Build `depth + 1` empty subtree roots: `empty_roots[h]` is the root
    /// of an empty subtree of height `h`; `empty_roots[0] = empty_leaf`.
    /// Each higher root hashes two copies of the preceding root:
    /// `empty_roots[h] = Hash([domain, empty_roots[h-1], empty_roots[h-1]])`.
    pub fn empty_roots(domain: Fr, empty_leaf: Fr, depth: usize) -> Result<Vec<Fr>, Error> {
        Self::validate_depth(depth)?;
        let mut empty_roots = Vec::with_capacity(depth + 1);
        empty_roots.push(empty_leaf);
        for level in 1..=depth {
            empty_roots.push(Self::node_hash(
                domain,
                empty_roots[level - 1],
                empty_roots[level - 1],
            )?);
        }
        Ok(empty_roots)
    }

    /// Root of an empty depth-`depth` tree.
    pub fn empty_root(domain: Fr, empty_leaf: Fr, depth: usize) -> Result<Fr, Error> {
        Ok(Self::empty_roots(domain, empty_leaf, depth)?[depth])
    }

    /// Stateless frontier append (the `0x0204` precompile kernel): append `leaf`
    /// at `next_index` using the given `frontier` and `empty_roots`, returning
    /// the single changed slot and the new root.
    pub fn frontier_append(
        domain: Fr,
        frontier: &[Fr],
        next_index: u64,
        leaf: Fr,
        empty_roots: &[Fr],
    ) -> Result<Append, Error> {
        let depth = frontier.len();
        Self::validate_depth(depth)?;
        if empty_roots.len() != depth + 1 {
            return Err(Error::Merkle(format!(
                "frontier_append: empty_roots.len() ({}) must be frontier.len()+1 ({})",
                empty_roots.len(),
                depth + 1
            )));
        }
        if next_index >= (1u64 << depth) {
            return Err(Error::Merkle(format!(
                "frontier_append: next_index {next_index} overflows depth-{depth} tree"
            )));
        }

        let mut current = leaf;
        let mut index = next_index;
        let mut changed: Option<(usize, Fr)> = None;
        for level in 0..depth {
            if index & 1 == 0 {
                // Left child: the accumulated subtree becomes the new frontier
                // entry the first time we hit a zero bit; pair with the empty subtree.
                if changed.is_none() {
                    changed = Some((level, current));
                }
                current = Self::node_hash(domain, current, empty_roots[level])?;
            } else {
                // Right child: merge with the waiting left sibling.
                current = Self::node_hash(domain, frontier[level], current)?;
            }
            index >>= 1;
        }

        let (changed_level, new_subtree) = match changed {
            Some(c) => c,
            // Tree-completing append: no new left subtree waits; report the top
            // slot unchanged.
            None => (depth - 1, frontier[depth - 1]),
        };
        Ok(Append {
            changed_level,
            new_subtree,
            new_root: current,
        })
    }

    /// Recompute a root from an inclusion path: the `leaf`, its `leaf_index`,
    /// and the sibling hash at each level bottom-up. The index bits select
    /// left/right per level, so no per-element side flag is stored.
    pub fn root_from_inclusion_path(
        domain: Fr,
        leaf: Fr,
        leaf_index: u64,
        siblings: &[Fr],
    ) -> Result<Fr, Error> {
        let depth = siblings.len();
        Self::validate_depth(depth)?;
        if leaf_index >= (1u64 << depth) {
            return Err(Error::Merkle(format!(
                "root_from_inclusion_path: leaf_index {leaf_index} overflows depth-{depth} tree"
            )));
        }
        let mut current = leaf;
        let mut index = leaf_index;
        for sibling in siblings {
            current = if index & 1 == 0 {
                Self::node_hash(domain, current, *sibling)?
            } else {
                Self::node_hash(domain, *sibling, current)?
            };
            index >>= 1;
        }
        Ok(current)
    }

    /// A new tree with a caller-supplied empty leaf (for example, a chain-tagged
    /// PayNote or Emit empty leaf). Supports depths 1 through 63.
    pub fn new(domain: Fr, empty_leaf: Fr, depth: usize) -> Result<Self, Error> {
        let empty_roots = Self::empty_roots(domain, empty_leaf, depth)?;
        Ok(Self {
            domain,
            depth,
            levels: vec![Vec::new(); depth + 1],
            empty_roots,
        })
    }

    /// Tree depth.
    pub fn depth(&self) -> usize {
        self.depth
    }
    /// Index the next appended leaf will occupy.
    pub fn next_index(&self) -> usize {
        self.leaves().len()
    }
    /// Current tree root, read in O(1).
    pub fn root(&self) -> Fr {
        self.levels[self.depth]
            .first()
            .copied()
            .unwrap_or(self.empty_roots[self.depth])
    }

    /// Append `leaf`, updating only its ancestors in O(depth). Returns the
    /// leaf's index and the equivalent stateless frontier change.
    /// Hashing errors leave the tree unchanged.
    pub fn append(&mut self, leaf: Fr) -> Result<(u64, Append), Error> {
        let index = self.next_index() as u64;
        if index >= (1u64 << self.depth) {
            return Err(Error::Merkle("commitment tree is full".into()));
        }
        let mut position = self.leaves().len();
        let mut current = leaf;
        let mut updates = Vec::with_capacity(self.depth + 1);
        updates.push(current);
        for level in 0..self.depth {
            current = if position & 1 == 0 {
                Self::node_hash(self.domain, current, self.empty_roots[level])?
            } else {
                Self::node_hash(self.domain, self.levels[level][position - 1], current)?
            };
            updates.push(current);
            position >>= 1;
        }

        // Match frontier_append's single-slot result without retaining a frontier.
        let changed_level = index.trailing_ones() as usize;
        let change = if changed_level == self.depth {
            // A full tree leaves the top frontier slot (its left subtree) unchanged.
            Append {
                changed_level: self.depth - 1,
                new_subtree: self.levels[self.depth - 1][0],
                new_root: current,
            }
        } else {
            Append {
                changed_level,
                new_subtree: updates[changed_level],
                new_root: current,
            }
        };

        // All fallible hashing is complete before any stored nodes change.
        position = self.leaves().len();
        for (nodes, value) in self.levels.iter_mut().zip(updates) {
            if position == nodes.len() {
                nodes.push(value);
            } else {
                nodes[position] = value;
            }
            position >>= 1;
        }
        Ok((index, change))
    }

    /// Leaves in append order.
    pub fn leaves(&self) -> &[Fr] {
        &self.levels[0]
    }

    /// Membership path for an appended leaf under the current root.
    /// Reads O(depth) stored siblings without hashing.
    pub fn inclusion_path(&self, leaf_index: u64) -> Result<InclusionPath, Error> {
        let mut index = usize::try_from(leaf_index)
            .map_err(|_| Error::Merkle("leaf index exceeds usize".into()))?;
        if index >= self.leaves().len() {
            return Err(Error::Merkle("leaf index outside tree".into()));
        }
        let mut siblings = Vec::with_capacity(self.depth);
        for level in 0..self.depth {
            siblings.push(
                self.levels[level]
                    .get(index ^ 1)
                    .copied()
                    .unwrap_or(self.empty_roots[level]),
            );
            index >>= 1;
        }
        Ok(InclusionPath {
            domain: self.domain,
            leaf_index,
            siblings,
        })
    }

    /// The inclusion path for `leaf_index` in an otherwise-empty tree of this
    /// depth (every sibling is the empty-subtree root at its level) — the path
    /// for a freshly-inserted leaf before any sibling is filled.
    pub fn empty_inclusion_path(&self, leaf_index: u64) -> InclusionPath {
        InclusionPath {
            domain: self.domain,
            leaf_index,
            siblings: self.empty_roots[..self.depth].to_vec(),
        }
    }
}

/// A Merkle inclusion (membership) proof: the leaf's index plus the sibling
/// hash at each level, bottom-up. `siblings.len()` is the tree depth.
#[derive(Clone)]
pub struct InclusionPath {
    /// Domain separator prepended to every inner-node hash.
    pub domain: Fr,
    /// The leaf's position in the tree.
    pub leaf_index: u64,
    /// Sibling hashes, bottom-up.
    pub siblings: Vec<Fr>,
}

impl InclusionPath {
    /// The tree depth this path covers.
    pub fn depth(&self) -> usize {
        self.siblings.len()
    }

    /// Resolve the tree root this path commits `leaf` to.
    pub fn root(&self, leaf: Fr) -> Result<Fr, Error> {
        Imt::root_from_inclusion_path(self.domain, leaf, self.leaf_index, &self.siblings)
    }

    /// Little-endian path bits: `true` means the current node is the left
    /// child; `false` means it is the right child.
    /// Rejects invalid depths and leaf indices outside the path's tree.
    pub fn circuit_indices(&self) -> Result<Vec<bool>, Error> {
        let depth = self.depth();
        Imt::validate_depth(depth)?;
        if self.leaf_index >= (1u64 << depth) {
            return Err(Error::Merkle(format!(
                "circuit_indices: leaf_index {} overflows depth-{depth} tree",
                self.leaf_index
            )));
        }
        Ok((0..depth)
            .map(|i| (self.leaf_index >> i) & 1 == 0)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ff::Zero;

    /// Appending a leaf to an empty tree gives the same root its (empty-tree)
    /// inclusion path resolves to.
    #[test]
    fn append_matches_inclusion_path() {
        let domain = Fr::from(42u64);
        let mut tree = Imt::new(domain, Fr::zero(), 8).unwrap();
        let path = tree.empty_inclusion_path(0);
        let leaf = Fr::from(7u64);
        let (index, append) = tree.append(leaf).unwrap();
        assert_eq!(index, 0);
        assert_eq!(path.root(leaf).unwrap(), append.new_root);
        assert_eq!(path.circuit_indices().unwrap(), vec![true; 8]);
    }

    #[test]
    fn circuit_indices_validate_path_and_preserve_bit_order() {
        let mut path = InclusionPath {
            domain: Fr::from(42u64),
            leaf_index: 0,
            siblings: Vec::new(),
        };
        for depth in [0, 64, 65] {
            path.siblings = vec![Fr::zero(); depth];
            assert!(path.circuit_indices().is_err());
        }
        for depth in [1, 32, 63] {
            path.siblings = vec![Fr::zero(); depth];
            path.leaf_index = (1u64 << depth) - 1;
            assert_eq!(path.circuit_indices().unwrap(), vec![false; depth]);
            path.leaf_index += 1;
            assert!(path.circuit_indices().is_err());
        }
        path.siblings = vec![Fr::zero(); 4];
        path.leaf_index = 0b0101;
        assert_eq!(
            path.circuit_indices().unwrap(),
            vec![false, true, false, true]
        );
    }

    #[test]
    fn domain_separates_empty_roots() {
        let root_a = Imt::empty_root(Fr::from(1u64), Fr::zero(), 32).unwrap();
        let root_b = Imt::empty_root(Fr::from(2u64), Fr::zero(), 32).unwrap();
        assert_ne!(root_a, root_b);
    }
}

#[cfg(test)]
mod retained_tree_tests {
    use super::*;
    use ark_ff::Zero;

    #[test]
    fn stored_paths_and_appends_match_frontier() {
        for depth in [1, 4] {
            for empty_leaf in [Fr::zero(), Fr::from(99u64)] {
                let domain = Fr::from(42u64);
                let mut tree = Imt::new(domain, empty_leaf, depth).unwrap();
                let empty_roots = Imt::empty_roots(domain, empty_leaf, depth).unwrap();
                let mut frontier = vec![Fr::zero(); depth];
                let mut saved_paths = Vec::new();
                assert_eq!(tree.root(), empty_roots[depth]);
                assert_eq!(tree.next_index(), 0);
                assert!(tree.inclusion_path(0).is_err());
                for i in 0..(1u64 << depth) {
                    let leaf = Fr::from(i + 7);
                    let expected =
                        Imt::frontier_append(domain, &frontier, i, leaf, &empty_roots).unwrap();
                    let (index, change) = tree.append(leaf).unwrap();
                    assert_eq!(index, i);
                    assert_eq!(tree.next_index(), (i + 1) as usize);
                    assert_eq!(change, expected);
                    assert_eq!(tree.root(), expected.new_root);
                    frontier[expected.changed_level] = expected.new_subtree;
                    for j in 0..=i {
                        let path = tree.inclusion_path(j).unwrap();
                        let leaf = tree.leaves()[j as usize];
                        assert_eq!(path.root(leaf).unwrap(), tree.root());
                        saved_paths.push((path, leaf, tree.root()));
                    }
                    assert!(tree.inclusion_path(i + 1).is_err());
                }
                // A captured path still proves its original root after later appends.
                for (path, leaf, root) in saved_paths {
                    assert_eq!(path.root(leaf).unwrap(), root);
                }
                let levels = tree.levels.clone();
                let root = tree.root();
                assert!(tree.append(Fr::from(100u64)).is_err());
                assert_eq!(tree.levels, levels);
                assert_eq!(tree.root(), root);
                assert_eq!(tree.next_index(), 1usize << depth);
                assert!(tree.inclusion_path(u64::MAX).is_err());
            }
        }
    }

    #[test]
    fn depth_boundaries_and_zero_empty_leaf() {
        let domain = Fr::from(42u64);
        for depth in [0, 64, usize::MAX] {
            assert!(Imt::new(domain, Fr::zero(), depth).is_err());
        }
        assert!(Imt::frontier_append(domain, &[], 0, Fr::zero(), &[Fr::zero()]).is_err());
        assert!(Imt::root_from_inclusion_path(domain, Fr::zero(), 0, &[Fr::zero(); 64]).is_err());
        for depth in [1, 32, 63] {
            let mut tree = Imt::new(domain, Fr::zero(), depth).unwrap();
            let old_path = tree.empty_inclusion_path(0);
            tree.append(Fr::from(7u64)).unwrap();
            assert_eq!(old_path.siblings, tree.inclusion_path(0).unwrap().siblings);
            assert_eq!(old_path.root(Fr::from(7u64)).unwrap(), tree.root());
        }
    }
}
