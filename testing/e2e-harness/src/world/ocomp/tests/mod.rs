use std::process::Command;

use crate::env::Environment;
use crate::internal::proc::ChildGuard;
use alloy_primitives::B256;
#[cfg(feature = "ocomp-integration")]
use outbe_chain_constants::GENESIS_CONFIG_KEY;
#[cfg(feature = "ocomp-integration")]
use outbe_metadosis::{WwdDayType, WwdStatus};
#[cfg(feature = "ocomp-integration")]
use outbe_ocomp_protocol::profile::poc_schema_limits;

use super::*;

mod evidence;
mod fixtures;
#[cfg(feature = "ocomp-integration")]
mod genesis;
mod harness;
#[cfg(feature = "ocomp-integration")]
mod launch_material;
mod process_isolation;
#[cfg(feature = "ocomp-integration")]
mod readiness;
mod restart;

use fixtures::launch_identity_evidence;

use harness::{
    child_guard, stopped_outage_fixture, topology, topology_with_validators, CHILD_MODE,
};

#[cfg(feature = "ocomp-integration")]
use fixtures::{
    append_artifact_fixture_log, artifact_fixture_vote_path, canonical_artifact_fixture,
    completed_job_topology, prepare_measurement_genesis_fixture,
    prepare_public_measurement_genesis_fixture,
    prepare_public_measurement_genesis_fixture_with_vote_window, stage_completed_job_footprint,
};

#[cfg(feature = "ocomp-integration")]
use harness::TestTopology;
