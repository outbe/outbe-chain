//! Composition root for the Outbe node.
//!
//! Infrastructure crates stay free of domain crates. This crate installs the
//! domain implementations and launches the process.

mod admission;
mod authority;
mod configuration;
mod consensus;
mod errors;
mod execution_runtime;
mod identity;
mod launch;
mod precompiles;
mod radicle;
mod shutdown;
mod system_tx;
mod tee;
mod upgrade;
mod upgrade_monitor;
mod upgrade_ports;
mod upgrade_promotion;
mod wiring;
mod wiring_state;

#[cfg(test)]
mod launch_tests;

pub use configuration::{
    apply_outbe_gas_price_oracle_defaults, command_requires_crs, outbe_default_txpool_values,
};
pub use launch::run_node;
pub(crate) use launch::LaunchConfig;
pub use wiring::OutbeRuntime;

#[cfg(test)]
pub(crate) use admission::{
    local_validator_tee_gate_v1, select_validator_upstream_anchor_v1, LocalTeeAdmissionAnchorV1,
};
pub(crate) use admission::{
    read_gated_finalized_local_tee_admission, require_upstream_fullnode_tee_admission,
    require_validator_tee_recovery_complete_v1, run_tee_lease_guard_v1,
    tee_lease_admission_rejection, tee_lease_exit_reason,
    validator_admission_anchor_from_durable_v1, validator_recovery_startup_admission_rejection,
    TeeLeaseGuardGateV1,
};
pub(crate) use clap::Parser;
pub(crate) use commonware_runtime::{Runner, Spawner, Supervisor};
pub(crate) use configuration::{
    configure_outbe_engine_args, outbe_default_rpc_values, validate_adr005_node_mode,
};
pub(crate) use eyre::WrapErr;
pub(crate) use identity::load_reth_p2p_node_host_signer;
pub(crate) use outbe_consensus::executor::actor::FinalizedExecutionBarrier;
pub(crate) use outbe_engine::args::ConsensusArgs;
pub(crate) use outbe_engine::bridge::ConsensusExecutionBridge;
pub(crate) use outbe_engine::finalization::{RethDurableExecutionState, RethFinalizationBarrier};
pub(crate) use outbe_evm::OutbeEvmSigner;
pub(crate) use outbe_node::{
    state_persistence::{validate_state_persistence_runtime_config, StatePersistenceRuntimeConfig},
    OutbeBeaconConsensus, OutbeFullNode, OutbeNode,
};
pub(crate) use outbe_operator::tee::{
    inspect_upgrade_journal_v1, read_finalized_registry_view_v1, record_upgrade_finalized_v1,
    record_upgrade_missed_cutoff_v1, record_upgrade_promoted_v1, NodeBindingSelectorV1,
    UpgradeJournalStateV1,
};
pub(crate) use outbe_primitives::OutbeHeader;
pub(crate) use reth_chainspec::{ChainSpec, EthChainSpec};
pub(crate) use reth_cli::chainspec::ChainSpecParser;
pub(crate) use reth_ethereum::cli::interface::Cli;
pub(crate) use reth_node_builder::NodeHandle;
pub(crate) use reth_provider::{BlockIdReader, HeaderProvider, StateProviderFactory};
pub(crate) use reth_rpc_server_types::{RethRpcModule, RpcModuleValidator};
#[cfg(test)]
pub(crate) use shutdown::{abort_and_wait_supervised, handle_consensus_thread_join};
pub(crate) use shutdown::{
    await_consensus_stack_shutdown, await_radicle_drain, consensus_shutdown_result,
    run_with_lifetime_pin, ConsensusThreadGuard, RADICLE_DRAIN_DEADLINE,
};
pub(crate) use std::{
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};
pub(crate) use tokio::sync::oneshot;
pub(crate) use tracing::info;
pub(crate) use upgrade::{
    run_upgrade_promotion_worker_v1, UpgradePromotionWorkerConfigV1, TEE_UPGRADE_CRITICAL_BLOCKS,
    TEE_UPGRADE_POLL_SECS, TEE_UPGRADE_WARNING_BLOCKS,
};
