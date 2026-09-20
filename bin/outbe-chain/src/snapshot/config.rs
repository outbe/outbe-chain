//! Resolve ordinary node configuration without opening stores or launching services.

use std::{ffi::OsString, path::PathBuf, sync::Arc};

use clap::{CommandFactory, FromArgMatches};
use outbe_offchain_storage::{StorageBackend, StorageConfig};
use outbe_snapshot::layout::ProtectedPaths;
use reth_chainspec::ChainSpec;
use reth_cli::chainspec::ChainSpecParser as _;
use reth_ethereum::cli::interface::{Cli, Commands};

use crate::{ConsensusArgs, OutbeChainSpecParser, OutbeHeader, OutbeRpcModuleValidator};

type NativeCli = Cli<OutbeChainSpecParser, ConsensusArgs, OutbeRpcModuleValidator>;

pub(crate) struct NodeInputs {
    cli: NativeCli,
    chain_source: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct NativeLayout {
    pub chain: Arc<ChainSpec<OutbeHeader>>,
    pub chain_root: PathBuf,
    pub consensus_root: PathBuf,
    pub ocomp_root: PathBuf,
    pub offchain_root: PathBuf,
    pub static_files_root: PathBuf,
    pub execution_rocksdb_root: PathBuf,
    pub projection_start_block: u64,
    pub protected: ProtectedPaths,
}

/// Parse trailing ordinary node options; never run or configure the parsed command.
pub(crate) fn parse_node_inputs(
    arguments: impl IntoIterator<Item = OsString>,
) -> eyre::Result<NodeInputs> {
    let _ = crate::outbe_default_txpool_values().try_init();
    let argv = [OsString::from("outbe-chain"), OsString::from("node")]
        .into_iter()
        .chain(arguments);
    let mut matches = NativeCli::command().try_get_matches_from(argv)?;
    let chain_source = matches
        .subcommand_matches("node")
        .and_then(|node| node.get_raw("chain"))
        .and_then(|mut values| values.next())
        .filter(|value| {
            !OutbeChainSpecParser::SUPPORTED_CHAINS
                .iter()
                .any(|name| *value == std::ffi::OsStr::new(name))
        })
        .map(PathBuf::from)
        // Reth tries a file first, then accepts inline genesis JSON.
        .filter(|path| path.is_file());
    Ok(NodeInputs {
        cli: NativeCli::from_arg_matches_mut(&mut matches)?,
        chain_source,
    })
}

pub(crate) fn resolve_layout(inputs: &NodeInputs) -> eyre::Result<NativeLayout> {
    let Commands::Node(node) = &inputs.cli.command else {
        eyre::bail!("snapshot native inputs must describe a node");
    };
    let datadir = node.datadir.clone().resolve_datadir(node.chain.chain());
    let chain_root = datadir.data_dir().to_path_buf();
    let args = &node.ext;
    let consensus_root = args
        .storage_dir
        .clone()
        .unwrap_or_else(|| chain_root.join("consensus"));
    let ocomp_root = chain_root
        .parent()
        .ok_or_else(|| eyre::eyre!("node data directory has no OCOMP parent"))?
        .join("ocomp/domain-v1");
    let storage_path = args.offchain_data()?.storage_config;
    let storage = StorageConfig::load(&storage_path)?;
    let StorageBackend::RocksDb(rocks) = storage.backend else {
        eyre::bail!("filesystem snapshots require RocksDB offchain storage");
    };
    let mut protected = vec![
        storage_path,
        args.keys_dir
            .clone()
            .unwrap_or_else(|| chain_root.join("keys")),
        node.config.clone().unwrap_or_else(|| datadir.config()),
        node.network
            .p2p_secret_key
            .clone()
            .unwrap_or_else(|| datadir.p2p_secret()),
        node.rpc
            .auth_jwtsecret
            .clone()
            .unwrap_or_else(|| datadir.jwt()),
    ];
    protected.extend(inputs.chain_source.clone());
    protected.extend(
        [
            args.signing_key.clone(),
            args.signing_share.clone(),
            args.public_polynomial.clone(),
            args.dkg_output.clone(),
            args.validator_evm_key.clone(),
            args.effective_validator_evm_key()?,
        ]
        .into_iter()
        .flatten(),
    );
    Ok(NativeLayout {
        chain: node.chain.clone(),
        chain_root,
        consensus_root,
        ocomp_root,
        offchain_root: rocks.path,
        static_files_root: datadir.static_files(),
        execution_rocksdb_root: datadir.rocksdb(),
        projection_start_block: storage.start_block,
        protected: ProtectedPaths(protected),
    })
}
