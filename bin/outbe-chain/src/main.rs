//! Outbe-reth node binary.
//!
//! Custom reth node with Outbe stateful precompiles and Commonware Simplex consensus.
//! Two tokio runtimes: Reth execution (main thread) + Commonware consensus (spawned thread).
//!
//! Also provides the `dkg` subcommand for bootstrapping BLS threshold key material.

use clap::Parser;
use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
use eyre::WrapErr as _;
use outbe_compressed_entities::{
    CandidateCacheLimits, CeMdbx, CompressedTreeService, EnvironmentIdentity, FinalizedMarker,
    ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_consensus::executor::actor::FinalizedCeCommitter;
use outbe_engine::args::ConsensusArgs;
use outbe_engine::bridge::ConsensusExecutionBridge;
use outbe_engine::ce_finalizer::{
    DurableCeState, FinalizedCeTree, RethCeFinalizer, RethDurableCeState,
};
use outbe_engine::ce_recovery::{
    CanonicalCeReplaySource, CeStartupRecovery, CeStartupRecoveryCoordinator, StartupCeTree,
};
use outbe_evm::OutbeEvmSigner;
use outbe_node::{
    compressed_storage::{
        validate_compressed_storage_runtime_config, CompressedStorageRuntimeConfig,
    },
    ocomp::retention::{RetainedTributeWriter, SharedOcompRetentionSelector},
    projection::{
        prepare_offchain_data_projection_with_retention, validate_offchain_data_checkpoint,
        OffchainDataProjectionConfig, ProjectionRetentionFence,
    },
    OutbeBeaconConsensus, OutbeFullNode, OutbeNode,
};
use outbe_operator::tee::{
    inspect_upgrade_journal_v1, read_finalized_registry_view_v1, record_upgrade_finalized_v1,
    record_upgrade_missed_cutoff_v1, record_upgrade_promoted_v1, NodeBindingSelectorV1,
    UpgradeJournalStateV1,
};
use outbe_primitives::projection::{
    projection_readiness, ProjectionCheckpoint, ProjectionReadinessHandle, ProjectionStatus,
};
use outbe_primitives::OutbeHeader;
use reth_chainspec::{ChainSpec, EthChainSpec};
use reth_cli::chainspec::ChainSpecParser;
use reth_ethereum::cli::interface::Cli;
use reth_node_builder::NodeHandle;
use reth_provider::{BlockIdReader, HeaderProvider, StateProviderFactory};
use reth_rpc_server_types::{RethRpcModule, RpcModuleSelection, RpcModuleValidator};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};
use tokio::sync::oneshot;
use tracing::info;

mod cli;
mod execution_runtime;
mod launch;
mod ocomp_exex;
mod ocomp_genesis;
mod tee_genesis;

mod snapshot;

#[cfg(test)]
mod test_utils;

use cli::chain_spec::{validate_outbe_chain_spec, OutbeChainSpecParser, OutbeRpcModuleValidator};

use cli::defaults::{apply_outbe_gas_price_oracle_defaults, initialize_crs_for_command};

use cli::dkg::run_dkg_command;

use cli::version::print_outbe_version;

use launch::admission::{
    read_gated_finalized_local_tee_admission, require_upstream_fullnode_tee_admission,
    require_validator_tee_recovery_complete_v1, run_tee_lease_guard_v1,
    tee_lease_admission_rejection, tee_lease_exit_reason,
    validator_admission_anchor_from_durable_v1, validator_recovery_startup_admission_rejection,
    TeeLeaseGuardGateV1,
};

use launch::bundles::load_installed_ocomp_bundles;

use launch::configuration::{
    configure_outbe_engine_args, outbe_default_rpc_values, outbe_default_txpool_values,
    validate_adr005_node_mode,
};

use launch::identity::load_reth_p2p_node_host_signer;

use launch::node::run_node;

use launch::shutdown::{
    await_consensus_stack_shutdown, await_radicle_drain, consensus_shutdown_result,
    handle_consensus_thread_join, run_with_lifetime_pin, ConsensusThreadGuard, LauncherExitCause,
    RADICLE_DRAIN_DEADLINE,
};

use launch::upgrade::{
    run_upgrade_promotion_worker_v1, UpgradePromotionWorkerConfigV1, TEE_UPGRADE_CRITICAL_BLOCKS,
    TEE_UPGRADE_POLL_SECS, TEE_UPGRADE_WARNING_BLOCKS,
};

#[cfg(test)]
use cli::defaults::command_requires_crs;

#[cfg(test)]
use cli::dkg::{parse_dkg_key_backend, DkgCli};

#[cfg(test)]
use launch::admission::LocalTeeAdmissionAnchorV1;

#[cfg(test)]
use launch::bundles::{ordered_installed_ocomp_bundle_hashes, parse_ocomp_bundle_hashes};

#[cfg(test)]
use launch::node::ocomp_job_available_for_calculation;

#[cfg(test)]
use launch::shutdown::abort_and_wait_supervised;

fn main() -> eyre::Result<()> {
    // Intercept Outbe-owned subcommands before reth CLI parsing.
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "dkg" {
        return run_dkg_command(&args);
    }
    if args.len() > 1 && args[1] == "tee" {
        return tee_genesis::run(&args);
    }
    if args.len() > 1 && args[1] == "ocomp" {
        return ocomp_genesis::run(&args);
    }
    if args.len() > 1 && args[1] == "snapshot" {
        return cli::snapshot::run(&args);
    }

    // Intercept `--version` / `-V` so that the user sees Outbe-side build
    // metadata in addition to Reth's own version string. The Outbe block is
    // printed first; Reth's CLI then prints its own version and exits.
    if args.iter().any(|a| a == "--version" || a == "-V") {
        print_outbe_version();
    }

    run_node()
}
