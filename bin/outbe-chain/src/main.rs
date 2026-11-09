//! Outbe node binary.
//!
//! Custom reth node with Outbe stateful precompiles and Commonware Simplex consensus.
//! Node launch lives in `outbe-runtime`. This binary owns the CLI intercepts.

mod cli;
pub(crate) mod errors;

mod snapshot;
mod tee_genesis;

#[cfg(test)]
mod test_utils;

use clap::Parser;
use outbe_engine::args::ConsensusArgs;
use outbe_primitives::OutbeHeader;
use reth_chainspec::{ChainSpec, EthChainSpec};
use reth_cli::chainspec::ChainSpecParser;
use reth_rpc_server_types::{RethRpcModule, RpcModuleSelection, RpcModuleValidator};
use std::sync::Arc;

use cli::chain_spec::{validate_outbe_chain_spec, OutbeChainSpecParser, OutbeRpcModuleValidator};
use cli::dkg::run_dkg_command;
use cli::version::print_outbe_version;

#[cfg(test)]
use cli::dkg::{parse_dkg_key_backend, DkgCli};
#[cfg(test)]
use outbe_runtime::{command_requires_crs, outbe_default_txpool_values};

fn main() -> eyre::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "dkg" {
        return run_dkg_command(&args);
    }
    if args.len() > 1 && args[1] == "tee" {
        return tee_genesis::run(&args);
    }
    if args.len() > 1 && args[1] == "snapshot" {
        return cli::snapshot::run(&args);
    }

    if args.iter().any(|a| a == "--version" || a == "-V") {
        print_outbe_version();
    }

    outbe_runtime::run_node::<OutbeChainSpecParser, OutbeRpcModuleValidator>()
}
