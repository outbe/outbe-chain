//! Test fixtures for catalog, materialization, and finalized job scenarios.

#[path = "test_support/common.rs"]
mod common;
pub use common::*;

#[path = "test_support/job.rs"]
mod job;
pub use job::{finalized_single_tribute_job, FixtureJobIdentity, FixtureJobTiming};

#[path = "test_support/completion.rs"]
mod completion;
pub use completion::zero_completion_summary;

#[path = "test_support/unused_result.rs"]
mod unused_result;
pub use unused_result::{unused_lysis_result, FixtureResultCommitments};
