mod job {
    include!("../../src/test_support/job.rs");
}

mod protocol {
    include!("../../src/test_support/protocol.rs");
}

use alloy_primitives::B256;
use outbe_ocomp_protocol::{control::FinalizedJobSpecV1, profile::ProtocolBundleV1};

/// Keep system temp-directory symlinks outside storage path validation.
#[allow(dead_code)]
pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)
}

pub fn protocol_bundle() -> ProtocolBundleV1 {
    protocol::protocol_bundle_fixture()
}

#[allow(dead_code)]
pub fn finalized_job_spec(
    seed: u8,
    cursor: u64,
    chain_id: u64,
    genesis_hash: B256,
) -> FinalizedJobSpecV1 {
    job::finalized_single_tribute_job(
        job::FixtureJobIdentity {
            seed,
            cursor,
            chain_id,
            genesis_hash,
        },
        &protocol_bundle(),
        || job::FixtureJobTiming {
            open_height: cursor + 1,
            deadline_height: cursor + 1_801,
        },
    )
}
