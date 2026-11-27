//! Composition root for the Outbe node.
//!
//! Infrastructure crates stay free of domain crates. This crate installs the
//! domain implementations and launches the process.

mod admission;
pub(crate) mod admission_authority;
mod authority;
mod configuration;
mod consensus;
pub mod consensus_metrics;
pub mod domain_rpc;
pub mod equivocation_slasher;
mod errors;
mod execution_runtime;
pub mod fs_epoch_rpc;
mod identity;
mod launch;
pub mod marker_addresses;
pub mod metrics_recorder;
pub mod precompiles;
mod radicle;
mod shutdown;
mod system_tx;
mod tee;
mod tee_canary_probe;
mod tee_supervisor;
mod wiring;

#[cfg(test)]
mod launch_tests;

pub use configuration::{
    apply_outbe_gas_price_oracle_defaults, command_requires_crs, outbe_default_txpool_values,
};
#[cfg(feature = "test-hooks")]
pub use launch::prepare_validator_for_test;
pub use launch::run_node;
pub(crate) use launch::LaunchConfig;
pub use metrics_recorder::install_outbe_prometheus_recorder;
pub use system_tx::{
    BLOCK_ORDER, END_BLOCK_FACTORY_STEP_GAS_LIMIT, HYPERLANE_STEP_GAS_LIMIT,
    ORACLE_SLASH_STEP_GAS_LIMIT,
};
#[cfg(any(test, feature = "test-hooks"))]
pub use tee_canary_probe::OfferCanaryProbe;
#[cfg(feature = "test-hooks")]
pub use tee_supervisor::test_hooks::{
    tee_launch_profile_for_test, LifecycleAuthorityProbe, LifecycleAuthorityProbeError,
    LifecycleSupervisorProbe,
};
pub use wiring::OutbeRuntime;

#[cfg(test)]
pub(crate) use admission::LocalTeeAdmissionAnchorV1;
pub(crate) use admission::{tee_lease_exit_reason, TeeLeaseGuardGateV1};
pub(crate) use clap::Parser;
pub(crate) use commonware_runtime::{Runner, Spawner, Supervisor};
pub(crate) use configuration::{
    configure_outbe_engine_args, outbe_default_rpc_values, validate_adr005_node_mode,
};
pub(crate) use eyre::WrapErr;
pub(crate) use identity::load_reth_p2p_node_host_signer;
pub(crate) use outbe_consensus::args::ConsensusArgs;
pub(crate) use outbe_engine::bridge::ConsensusExecutionBridge;
pub(crate) use outbe_engine::{run_consensus_with_execution, RethExecutionBackend};
pub(crate) use outbe_evm::OutbeEvmSigner;
pub(crate) use outbe_execution::{
    state_persistence::{validate_state_persistence_runtime_config, StatePersistenceRuntimeConfig},
    OutbeBeaconConsensus, OutbeFullNode, OutbeNode,
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
