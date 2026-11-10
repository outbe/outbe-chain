//! Consensus-owned DKG, certified follower, recovery, and lifecycle services.

use alloy_primitives::{Address as EthAddress, Bytes, B256};
use commonware_codec::Encode as _;
use commonware_consensus::types::{Epoch, Height, Round, ViewDelta};
use commonware_cryptography::{
    bls12381::{
        self,
        dkg::feldman_desmedt::Output,
        primitives::{group::Share, sharing::Sharing, variant::MinSig},
    },
    Signer as _,
};
use commonware_p2p::{Receiver as P2pReceiver, Sender as P2pSender};
use commonware_runtime::{
    buffer::paged::CacheRef, BufferPooler, Clock, Metrics, Network, Resolver, Spawner, Storage,
};
use commonware_utils::TryCollect as _;
use eyre::{Result, WrapErr};
use rand_core_commonware::CryptoRng;
use reth_ethereum::network::api::{NetworkInfo, Peers, PeersInfo};
use reth_ethereum::provider::{BlockHashReader, StateProviderFactory};
use reth_node_builder::ConsensusEngineHandle;
use reth_provider::HeaderProvider;
use std::future::Future;
use std::num::{NonZeroU16, NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tracing::{info, warn};

use crate::args::ConsensusArgs;
use crate::validators;
use crate::{
    bls, config,
    digest::Digest,
    dkg_actor,
    dkg_manager::{self, Mailbox as DkgManagerMailbox},
    executor::actor::{ExecutorActor, FinalizedExecutionBarrier, RecoveredForkchoiceAttempt},
    hybrid::{HybridScheme, HybridSchemeProvider, VrfMaterialProvider},
    vrf_safety::VrfSafetyGate,
};

use crate::execution_port::{ExecutionProviderAccess, ExecutionStateAccess};
use outbe_primitives::{
    consensus::{ConsensusExecutionBridge, DkgBoundaryArtifact},
    projection::ProjectionCheckpoint,
    reshare_artifact::{
        decode_boundary_artifact, decode_outbe_block_artifacts, encode_boundary_artifact,
        ConsensusHeaderArtifact,
    },
    OutbeHeader, OutbePayloadTypes,
};
use reth_ethereum::storage::{BlockNumReader, BlockReader, TransactionVariant};

pub mod dkg;
pub mod follower;
pub mod marshal_archive;
pub mod recovery;
pub mod recovery_marshal;
pub mod services;
pub mod shutdown;

use dkg::handoff::{
    active_set_hash_from_addresses, next_consensus_epoch_after_dkg_activation,
    ordered_validator_addresses, refresh_validator_set_at_height, startup_live_join_scan_height,
    DealerOnlyDkgActivation, DkgRotationParams, FrozenDkgTarget, FrozenValidatorSetRefresh,
    PendingDkgActivation, RestoredPendingDkgActivation,
};
pub use dkg::persistence::migrate_dkg_keys_if_needed;
use dkg::persistence::{
    clear_pending_dkg_boundary, decode_boundary_output, dkg_retry_store, load_pending_dkg_state,
    load_saved_dkg_state, recover_pending_dkg_boundary_snapshot, remove_pending_dkg_state,
    retire_activated_dkg_retry_state, save_dkg_state, DkgStateMaterial, DkgStateStore,
    PendingDkgBoundarySnapshot, DKG_OUTPUT_FILE, DKG_POLYNOMIAL_FILE, DKG_SHARE_FILE,
};
use dkg::startup::{
    participants_from_validator_set, recover_latest_boundary_artifact,
    validator_set_for_dkg_output_players,
};
use recovery::anchor::{
    certified_follower_replay_suffix_bounds, select_certified_follower_recovery_height,
    validate_ancestor_follower_recovery_record, CertifiedFollowerRecoveryAnchor,
    CertifiedFollowerRecoveryFloors, RecoveredApplicationFinalization,
};
use recovery::forkchoice::confirm_recovered_forkchoice;
use services::EngineHandle;
pub use services::{
    ConsensusExecutionServices, ConsensusRadicleServices, ConsensusShutdownServices,
    ConsensusStackServices,
};

/// Read `epochLengthBlocks` from genesis.json `config` section.
/// Falls back to [`config::DEFAULT_EPOCH_LENGTH_BLOCKS`] if absent.
pub fn epoch_length_blocks_from_genesis(node: &impl StackNodeAccess) -> Result<u32> {
    use crate::errors::StackStartupError;
    let extra = &node.chain_spec().genesis.config.extra_fields;
    for (legacy_key, err) in [
        ("epochDuration", StackStartupError::DeprecatedEpochDuration),
        (
            "dkgRotationIntervalBlocks",
            StackStartupError::DeprecatedDkgRotationIntervalBlocks,
        ),
    ] {
        if extra.get(legacy_key).is_some() {
            return Err(err.into());
        }
    }
    let Some(parsed) = extra.get_deserialized::<u32>("epochLengthBlocks") else {
        return Ok(config::DEFAULT_EPOCH_LENGTH_BLOCKS);
    };
    let blocks =
        parsed.map_err(|err| StackStartupError::InvalidEpochLengthBlocks(err.to_string()))?;
    (blocks > 0)
        .then_some(blocks)
        .ok_or_else(|| StackStartupError::EpochLengthBlocksZero.into())
}

pub fn genesis_hash(node: &impl StackNodeAccess) -> Result<B256> {
    let hash = node
        .provider()
        .block_hash(0)
        .map_err(|e| crate::errors::StackStartupError::GetGenesisHash(format!("{e}")))?;
    require_genesis_hash(hash)
}

pub fn require_genesis_hash(hash: Option<B256>) -> Result<B256> {
    hash.ok_or_else(|| crate::errors::StackStartupError::MissingGenesisBlockHash.into())
}

/// Read the canonical height-0 genesis block from the provider and wrap it as a
/// [`ConsensusBlock`](crate::block::ConsensusBlock).
///
/// commonware 2026.5.0 replaced the removed `Automaton::genesis` call with an
/// explicit `marshal::Config.start`. Marshal's `Start::Genesis` anchor must be
/// the real height-0 block (the actor asserts `anchor.height() == 0`). So we
/// read the canonical genesis block straight from the execution DB rather than
/// synthesizing one. Sealing comes from the provider's stored block, so the
/// anchor's `block_hash()` is byte-identical to the chain `genesis_hash`.
pub fn genesis_consensus_block(
    node: &impl StackNodeAccess,
) -> Result<crate::block::ConsensusBlock> {
    let recovered = node
        .provider()
        .recovered_block(0u64.into(), TransactionVariant::NoHash)
        .map_err(|e| crate::errors::StackStartupError::ReadGenesisBlock(format!("{e}")))?
        .ok_or(crate::errors::StackStartupError::MissingGenesisBlock)?;
    Ok(crate::block::ConsensusBlock::from_sealed(
        recovered.into_sealed_block(),
    ))
}

pub fn nonzero_u16(value: u16, name: &str) -> Result<NonZeroU16> {
    NonZeroU16::new(value).ok_or_else(|| {
        crate::errors::StackStartupError::ValueMustBePositive(name.to_string()).into()
    })
}

pub fn nonzero_usize(value: usize, name: &str) -> Result<NonZeroUsize> {
    NonZeroUsize::new(value).ok_or_else(|| {
        crate::errors::StackStartupError::ValueMustBePositive(name.to_string()).into()
    })
}

pub fn nonzero_u64(value: u64, name: &str) -> Result<NonZeroU64> {
    NonZeroU64::new(value).ok_or_else(|| {
        crate::errors::StackStartupError::ValueMustBePositive(name.to_string()).into()
    })
}

/// Map `marshal::core::Actor::init`'s `Option<Height>` to the executor/startup
/// finalized height:
/// - `None` (no durable consensus finalization yet) means a fresh genesis node.
///   It maps to height 0.
/// - `Some(n)` resumes from the durable finalized height `n` (finalization is
///   monotonic).
///
/// A restarted node that already finalized must NOT be reset toward genesis.
/// This mapping is a separate function so that the regression test exercises
/// this exact mapping rather than stdlib `unwrap_or`.
pub fn map_marshal_init_height(opt: Option<Height>) -> Height {
    opt.unwrap_or(Height::zero())
}

/// Node capabilities needed by the migrated CL stacks during engine compatibility.
pub trait StackNodeAccess:
    ExecutionProviderAccess + crate::execution_port::RecoveryForkchoiceReader
{
    fn chain_spec(&self) -> Arc<reth_chainspec::ChainSpec<OutbeHeader>>;
    fn beacon_engine_handle(&self) -> EngineHandle;
    fn collect_genesis_peer_evidence(
        &self,
    ) -> impl Future<Output = crate::execution_port::GenesisPeerEvidence> + Send;
}

impl<Node, AddOns> StackNodeAccess for reth_node_builder::FullNode<Node, AddOns>
where
    Node: reth_node_builder::FullNodeComponents<
        Types: reth_node_builder::NodeTypes<
            ChainSpec = reth_chainspec::ChainSpec<OutbeHeader>,
            Payload = OutbePayloadTypes,
        >,
        Provider: ExecutionStateAccess + crate::execution_port::RecoveryForkchoiceReader,
        Network: Peers + PeersInfo + NetworkInfo,
    >,
    AddOns: reth_node_builder::rpc::RethRpcAddOns<Node>,
    Self: Send + Sync + 'static,
{
    fn chain_spec(&self) -> Arc<reth_chainspec::ChainSpec<OutbeHeader>> {
        self.chain_spec()
    }

    fn beacon_engine_handle(&self) -> EngineHandle {
        self.add_ons_handle.beacon_engine_handle.clone()
    }

    async fn collect_genesis_peer_evidence(&self) -> crate::execution_port::GenesisPeerEvidence {
        use crate::execution_port::{GenesisPeerEvidence, GenesisPeerStatus};
        let peers_result = self.network.get_all_peers().await;
        let (peer_query_failed, peers) = match peers_result {
            Ok(peers) => (
                false,
                peers
                    .into_iter()
                    .map(|peer| GenesisPeerStatus {
                        genesis: peer.status.genesis,
                        blockhash: peer.status.blockhash,
                        latest_block: peer.status.latest_block,
                    })
                    .collect(),
            ),
            Err(error) => {
                warn!(
                    ?error,
                    "failed to query Reth peers during genesis formation gate"
                );
                (true, Vec::new())
            }
        };
        GenesisPeerEvidence {
            connected_peers: self.network.num_connected_peers(),
            is_syncing: self.network.is_syncing(),
            is_initially_syncing: self.network.is_initially_syncing(),
            peer_query_failed,
            peers,
        }
    }
}

impl<Node, AddOns> crate::execution_port::RecoveryForkchoiceReader
    for reth_node_builder::FullNode<Node, AddOns>
where
    Node: reth_node_builder::FullNodeComponents<
        Provider: ExecutionStateAccess + crate::execution_port::RecoveryForkchoiceReader,
    >,
    AddOns: reth_node_builder::NodeAddOns<Node>,
{
    fn recovery_forkchoice(
        &self,
    ) -> Result<crate::execution_port::RecoveryForkchoice, crate::errors::StackRecoveryError> {
        self.provider.recovery_forkchoice()
    }
}

impl<N> crate::execution_port::RecoveryForkchoiceReader
    for reth_provider::providers::BlockchainProvider<N>
where
    N: reth_provider::providers::ProviderNodeTypes<Primitives = outbe_primitives::OutbePrimitives>,
{
    fn recovery_forkchoice(
        &self,
    ) -> Result<crate::execution_port::RecoveryForkchoice, crate::errors::StackRecoveryError> {
        crate::execution_port::RecoveryForkchoiceReader::recovery_forkchoice(
            &self.canonical_in_memory_state(),
        )
    }
}

pub fn provider_matches_consensus_tip(
    provider: &impl BlockHashReader,
    tip: crate::marshal_update_reporter::ConsensusTip,
    required_height: u64,
) -> Result<bool> {
    let tip_height = tip.height.get();
    if tip_height < required_height {
        return Ok(false);
    }
    provider
        .block_hash(tip_height)
        .map(|hash| hash == Some(tip.digest.0))
        .map_err(|err| {
            crate::errors::StackEpochError::read_provider_block_hash_at_consensus_tip(
                tip_height, err,
            )
            .into()
        })
}

pub fn elapsed_since(now: SystemTime, since: SystemTime) -> Duration {
    match now.duration_since(since) {
        Ok(elapsed) => elapsed,
        Err(_) => Duration::ZERO,
    }
}

#[cfg(test)]
mod helper_contract_tests {
    use super::*;
    use crate::execution_port::{
        GenesisPeerEvidence, RecoveryForkchoice, RecoveryForkchoiceReader,
    };
    use reth_provider::test_utils::MockEthProvider;

    #[derive(Clone)]
    struct GenesisNode(
        MockEthProvider<outbe_primitives::OutbePrimitives, reth_chainspec::ChainSpec<OutbeHeader>>,
    );

    impl ExecutionProviderAccess for GenesisNode {
        type Provider = MockEthProvider<
            outbe_primitives::OutbePrimitives,
            reth_chainspec::ChainSpec<OutbeHeader>,
        >;
        fn provider(&self) -> &Self::Provider {
            &self.0
        }
    }

    impl RecoveryForkchoiceReader for GenesisNode {
        fn recovery_forkchoice(
            &self,
        ) -> Result<RecoveryForkchoice, crate::errors::StackRecoveryError> {
            unreachable!("genesis configuration does not read forkchoice")
        }
    }

    impl StackNodeAccess for GenesisNode {
        fn chain_spec(&self) -> Arc<reth_chainspec::ChainSpec<OutbeHeader>> {
            self.0.chain_spec.clone()
        }
        fn beacon_engine_handle(&self) -> EngineHandle {
            unreachable!("genesis configuration does not access the engine")
        }
        async fn collect_genesis_peer_evidence(&self) -> GenesisPeerEvidence {
            unreachable!("genesis configuration does not access peers")
        }
    }

    // CHARACTERIZATION: the relocated parser must preserve legacy-key precedence and typed failures.
    #[test]
    fn genesis_epoch_length_preserves_configuration_contract() {
        use crate::errors::StackStartupError;
        for (config, expected) in [
            (
                serde_json::json!({}),
                Ok(config::DEFAULT_EPOCH_LENGTH_BLOCKS),
            ),
            (serde_json::json!({"epochLengthBlocks": 7}), Ok(7)),
            (serde_json::json!({"epochLengthBlocks": 0}), Err("zero")),
            (
                serde_json::json!({"epochLengthBlocks": "7"}),
                Err("invalid"),
            ),
            (serde_json::json!({"epochLengthBlocks": -1}), Err("invalid")),
            (
                serde_json::json!({"epochLengthBlocks": 4294967296_u64}),
                Err("invalid"),
            ),
            (
                serde_json::json!({"epochDuration": null, "dkgRotationIntervalBlocks": 9, "epochLengthBlocks": "invalid"}),
                Err("duration"),
            ),
            (
                serde_json::json!({"dkgRotationIntervalBlocks": null, "epochLengthBlocks": 0}),
                Err("rotation"),
            ),
        ] {
            let mut spec = reth_chainspec::ChainSpec::<OutbeHeader>::default();
            spec.genesis.config = serde_json::from_value(config).unwrap();
            let node = GenesisNode(MockEthProvider::new().with_chain_spec(spec));
            let actual = epoch_length_blocks_from_genesis(&node);
            match expected {
                Ok(blocks) => assert_eq!(actual.unwrap(), blocks),
                Err(kind) => {
                    let error = actual.unwrap_err();
                    let error = error.downcast_ref::<StackStartupError>().unwrap();
                    assert!(matches!(
                        (kind, error),
                        ("zero", StackStartupError::EpochLengthBlocksZero)
                            | ("invalid", StackStartupError::InvalidEpochLengthBlocks(_))
                            | ("duration", StackStartupError::DeprecatedEpochDuration)
                            | (
                                "rotation",
                                StackStartupError::DeprecatedDkgRotationIntervalBlocks
                            )
                    ));
                }
            }
        }
    }

    // CHARACTERIZATION: matching height is insufficient without the same canonical identity.
    #[test]
    fn consensus_tip_match_preserves_height_hash_and_provider_failure() {
        struct HashRead(Result<Option<B256>, reth_provider::ProviderError>);
        impl BlockHashReader for HashRead {
            fn block_hash(&self, _: u64) -> reth_provider::ProviderResult<Option<B256>> {
                self.0.clone()
            }
            fn canonical_hashes_range(
                &self,
                _: u64,
                _: u64,
            ) -> reth_provider::ProviderResult<Vec<B256>> {
                unreachable!("tip matching reads one canonical hash")
            }
        }
        let tip = crate::marshal_update_reporter::ConsensusTip {
            round: Round::new(Epoch::new(0), commonware_consensus::types::View::new(7)),
            height: Height::new(42),
            digest: Digest(B256::repeat_byte(0x11)),
        };
        for (hash, required, expected) in [
            (Some(tip.digest.0), 41, true),
            (Some(tip.digest.0), 42, true),
            (Some(tip.digest.0), 43, false),
            (Some(B256::repeat_byte(0x22)), 42, false),
            (None, 42, false),
        ] {
            assert_eq!(
                provider_matches_consensus_tip(&HashRead(Ok(hash)), tip, required).unwrap(),
                expected
            );
        }
        let failed = HashRead(Err(reth_provider::ProviderError::UnsupportedProvider));
        assert!(!provider_matches_consensus_tip(&failed, tip, 43).unwrap());
        let error = provider_matches_consensus_tip(&failed, tip, 42).unwrap_err();
        assert!(error
            .downcast_ref::<crate::errors::StackEpochError>()
            .is_some());
    }
}
