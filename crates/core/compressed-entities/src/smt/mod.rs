//! Private CKB SMT engine and CES2 Poseidon codec.

#[allow(dead_code)]
mod codec;
#[allow(dead_code)]
mod facade;

#[allow(unused_imports)]
pub(crate) use codec::{hash_error, PoseidonCkbHasher};
#[allow(unused_imports)]
pub use facade::TreeError;
#[allow(unused_imports)]
pub(crate) use facade::{
    derive_tree_key, PoseidonSmt, SortedNodeObserver, SortedPoseidonRootReducer, TreeKey, TreeLeaf,
    TreeProof, TreeRoot,
};

#[cfg(test)]
mod tests;
