//! Bounded, permissionless, append-only registry of public Heartwood repositories.

pub mod errors;
pub mod payload;
pub mod precompile;
pub mod publication;
pub mod schema;
pub mod vote_target;

mod runtime;

pub use runtime::{repo_id_from_storage_key, repo_id_storage_key, RepoId, RepositoryRegistration};
pub use schema::CodeRegistry;
pub use schema::PatchApprovalRecord;

#[cfg(test)]
mod tests;
