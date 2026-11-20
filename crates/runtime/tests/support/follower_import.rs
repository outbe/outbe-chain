//! Deliver finalized blocks to the real Outbe executor, backed by a Reth node.
use std::{path::Path, sync::Arc, time::Duration};

use commonware_consensus::{marshal::Update, types::Height, Reporter as _};
use commonware_runtime::Supervisor as _;
use commonware_utils::acknowledgement::{Acknowledgement as _, Exact};
use outbe_consensus::{
    block::ConsensusBlock,
    execution_port::ExecutionNodeBackend as _,
    executor::{
        actor::{ExecutorActor, RecoveredFinalizedState},
        Mailbox,
    },
    hybrid::HybridSchemeProvider,
    stack::recovery_marshal::start_recovery_marshal_in_partition,
};
use outbe_engine::RethExecutionBackend;
use outbe_execution::{OutbeFullNode, OutbeNode};
use outbe_primitives::{
    consensus::{ConsensusExecutionBridge, ConsensusOperatingMode},
    OutbeBlock, OutbeHeader,
};
use reth_chainspec::ChainSpec;
use reth_ethereum::provider::db::{init_db, mdbx::DatabaseArguments, DatabaseEnv};
use reth_node_api::{NodeTypesWithDBAdapter, TreeConfig};
use reth_node_builder::{EngineNodeLauncher, Node as _, NodeBuilder, NodeConfig};
use reth_node_core::args::{DatadirArgs, DiscoveryArgs, NetworkArgs};
use reth_primitives_traits::{SealedBlock, SealedHeader};
use reth_provider::{providers::BlockchainProvider, BlockIdReader, HeaderProvider};

pub(super) type FollowerProvider =
    BlockchainProvider<NodeTypesWithDBAdapter<OutbeNode, DatabaseEnv>>;

pub(super) async fn launch_with_seed(
    spec: Arc<ChainSpec<OutbeHeader>>,
    datadir: &Path,
    signer: Option<Arc<outbe_evm::OutbeEvmSigner>>,
    seed: impl FnOnce(&FollowerProvider) -> eyre::Result<()> + Send + 'static,
) -> eyre::Result<OutbeFullNode> {
    launch_profile(
        spec,
        datadir,
        (ConsensusOperatingMode::Follower, signer),
        seed,
    )
    .await
}

pub(super) async fn launch_profile(
    spec: Arc<ChainSpec<OutbeHeader>>,
    datadir: &Path,
    (mode, signer): (
        ConsensusOperatingMode,
        Option<Arc<outbe_evm::OutbeEvmSigner>>,
    ),
    seed: impl FnOnce(&FollowerProvider) -> eyre::Result<()> + Send + 'static,
) -> eyre::Result<OutbeFullNode> {
    let config = NodeConfig::new(spec)
        .with_datadir_args(DatadirArgs {
            datadir: datadir.to_path_buf().into(),
            ..Default::default()
        })
        .with_network(NetworkArgs {
            discovery: DiscoveryArgs {
                disable_discovery: true,
                ..Default::default()
            },
            ..Default::default()
        })
        .with_unused_ports();
    let db = init_db(config.datadir().db(), DatabaseArguments::test())?;
    let bridge = ConsensusExecutionBridge::new();
    bridge.set_operating_mode(mode);
    let runtime = Arc::new(outbe_runtime::OutbeRuntime::new());
    let execution = OutbeNode {
        shutdown: Default::default(),
        bridge: Some(bridge),
        evm_signer: signer,
        runtime_host: Some(runtime.clone()),
        pool_policy: runtime,
        rpc_protection: Default::default(),
    };
    let handle = NodeBuilder::new(config)
        .with_database(db)
        .with_launch_context(reth_tasks::Runtime::test())
        .with_types_and_provider::<OutbeNode, BlockchainProvider<_>>()
        .with_components(execution.components_builder())
        .with_add_ons(execution.add_ons())
        .on_component_initialized(move |node| seed(&node.provider))
        .launch_with_fn(|builder| {
            let launcher = EngineNodeLauncher::new(
                builder.task_executor().clone(),
                builder.config().datadir(),
                TreeConfig::default()
                    .with_persistence_threshold(0)
                    .with_memory_block_buffer_target(0),
            );
            builder.launch_with(launcher)
        })
        .await?;
    Ok(handle.node)
}

pub(super) struct FollowerImport {
    mailbox: Mailbox,
    finalized: tokio::sync::mpsc::UnboundedReceiver<u64>,
    executor: commonware_runtime::Handle<eyre::Result<()>>,
    _resolver:
        commonware_consensus::marshal::resolver::handler::Handler<outbe_consensus::digest::Digest>,
    _marshal: commonware_runtime::Handle<()>,
}

impl FollowerImport {
    pub(super) async fn start(
        ctx: commonware_runtime::tokio::Context,
        node: OutbeFullNode,
        genesis: OutbeBlock,
        anchor: &SealedHeader<OutbeHeader>,
    ) -> Self {
        let backend = RethExecutionBackend::new(node);
        let genesis = ConsensusBlock::from_sealed(SealedBlock::seal_slow(genesis));
        let (sender, finalized) = tokio::sync::mpsc::unbounded_channel();
        let (actor, mailbox) = ExecutorActor::new(
            ctx.child("executor"),
            backend.beacon_engine_handle(),
            RecoveredFinalizedState {
                genesis_hash: genesis.block_hash(),
                last_finalized_height: anchor.header().inner.number,
                last_finalized_hash: anchor.hash(),
            },
            Some(sender),
        );
        let (marshal, resolver, marshal_handle) = start_recovery_marshal_in_partition(
            ctx.child("marshal"),
            HybridSchemeProvider::new(),
            mailbox.clone(),
            "follower-import".into(),
            genesis,
        )
        .await;
        let executor = actor
            .with_finalized_execution_barrier(backend.finalized_execution_barrier())
            .start(marshal, Height::new(anchor.header().inner.number));
        Self {
            mailbox,
            finalized,
            executor,
            _resolver: resolver,
            _marshal: marshal_handle,
        }
    }

    pub(super) async fn finalize(&mut self, block: OutbeBlock) {
        let height = block.header.inner.number;
        let block = ConsensusBlock::from_sealed(SealedBlock::seal_slow(block));
        let (ack, wait) = Exact::handle();
        assert_eq!(
            self.mailbox.report(Update::Block(Arc::new(block), ack)),
            commonware_actor::Feedback::Ok
        );
        tokio::time::timeout(Duration::from_secs(35), async {
            if wait.await.is_err() {
                let result = (&mut self.executor).await;
                panic!("follower executor did not acknowledge height {height}: {result:?}");
            }
            loop {
                let observed = self
                    .finalized
                    .recv()
                    .await
                    .expect("execution-finalized notification");
                if observed != 0 {
                    assert_eq!(observed, height);
                    break;
                }
            }
        })
        .await
        .expect("follower delivery and finalization deadline");
    }
}

pub(super) fn assert_finalized(
    provider: &impl BlockIdReader,
    headers: &impl HeaderProvider<Header = OutbeHeader>,
    height: u64,
    expected_hash: alloy_primitives::B256,
) -> SealedHeader<OutbeHeader> {
    let finalized = provider.finalized_block_num_hash().unwrap().unwrap();
    let canonical = provider.chain_info().unwrap();
    assert_eq!((finalized.number, finalized.hash), (height, expected_hash));
    assert_eq!(
        (canonical.best_number, canonical.best_hash),
        (height, expected_hash)
    );
    headers.sealed_header(height).unwrap().unwrap()
}
