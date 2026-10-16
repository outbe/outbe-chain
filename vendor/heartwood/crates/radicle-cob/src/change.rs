// Copyright © 2021 The Radicle Link Contributors

use oid::Oid;

mod signature;
pub mod store;
pub use signature::{EntrySignature, VerifiedBls};
pub use store::{Contents, EntryId, Storage, Template, Timestamp};

use crate::signatures::ExtendedSignature;

/// A single change in the change graph.
pub type Entry = store::Entry<Oid, Oid, ExtendedSignature<crypto::RepositoryKey, EntrySignature>>;
