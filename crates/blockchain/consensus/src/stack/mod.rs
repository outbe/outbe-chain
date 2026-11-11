//! Consensus stack - wires P2P, Simplex engine, application handler, and executor.
//!
//! This is the entry point for the consensus layer, called from the consensus
//! runtime thread in `main.rs`.
//!
//! Startup flow:
//! 1. Load signing key and validator set
//! 2. Configure P2P network (register ALL channels including DKG)
//! 3. Start P2P network, register peers, and create Muxers for epoch-scoped
//!    consensus channels (all in transport startup)
//! 4. Obtain threshold material:
//!    a. From saved DKG state on disk (restart precedence)
//!    b. From finalized on-chain `BoundaryOutcome` (shareless Verifier join/restart)
//!    c. Via interactive DKG ceremony during fresh genesis formation
//! 5. Enter epoch loop:
//!    a. Register epoch sub-channels, build HybridScheme + Reporter
//!    b. Start Simplex engine
//!    c. Monitor for the reshare trigger (the block-height freeze schedule, `freeze_height`)
//!    d. On reshare: run DKG in parallel, then abort engine + restart at new epoch

use alloy_primitives::{Address as EthAddress, Bytes, B256};
use commonware_codec::Encode as _;
use commonware_consensus::{
    simplex,
    types::{Epoch, Height, Round, ViewDelta},
    Reporters,
};
use commonware_cryptography::{
    bls12381::{
        self,
        dkg::feldman_desmedt::Output,
        primitives::{group::Share, sharing::Sharing, variant::MinSig},
    },
    Signer as _,
};
use commonware_p2p::{authenticated::lookup, utils::mux::Muxer, Address, AddressableManager};
use commonware_runtime::{
    buffer::paged::CacheRef, BufferPooler, Clock, Metrics, Network, Quota, Resolver, Spawner,
    Storage,
};
use commonware_utils::{ordered::Map, TryCollect as _, NZU32};
use eyre::{Result, WrapErr};
use rand_core_commonware::CryptoRng;
use reth_ethereum::chainspec::EthChainSpec as _;
use reth_ethereum::provider::{BlockHashReader, StateProviderFactory};
use reth_ethereum::storage::HeaderProvider;
use std::collections::BTreeMap;
use std::future::Future;
use std::net::SocketAddr;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64, NonZeroUsize};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tracing::{debug, info, warn};

use crate::args::ConsensusArgs;
use crate::validators;
use crate::{
    ancestry_readiness::AncestryReadiness,
    application::{
        actor::OutbeApplication,
        handler::{ApplicationDeps, ApplicationHandler},
        ApplicationEpochFence,
    },
    bls,
    committee_provider::CommitteeProvider,
    config,
    digest::Digest,
    dkg_actor,
    dkg_manager::{self, Mailbox as DkgManagerMailbox},
    executor::actor::{ExecutorActor, FinalizedExecutionBarrier},
    finalization::{
        actor::{FinalizationActor, FinalizationActorBuilder, FinalizationActorDeps},
        block_cache::BlockCache,
        state::new_finalization_view,
    },
    hybrid::{
        election::{HybridElectorConfigProvider, HybridRandom},
        HybridScheme, HybridSchemeProvider, VrfMaterialProvider,
    },
    reporter::{OutbeReporter, ReporterContinuity},
    vrf_safety::VrfSafetyGate,
};
use outbe_radicle::integration::{RadicleVotingGate, RadicleVotingGateError};

use crate::execution_port::{
    EngineClient as _, ExecutionNodeBackend, ExecutionProviderAccess, PayloadBuilderPort as _,
};
use commonware_p2p::{Receiver as P2pReceiver, Sender as P2pSender};
use outbe_primitives::{
    consensus::{ConsensusExecutionBridge, DkgBoundaryArtifact},
    projection::ProjectionCheckpoint,
};
use outbe_primitives::{OutbeHeader, OutbePayloadTypes};
use reth_ethereum::node::api::ConsensusEngineHandle;
use reth_ethereum::storage::{BlockNumReader, BlockReader, TransactionVariant};

pub mod dkg;
pub mod epoch;
pub mod follower;
pub mod marshal_archive;
pub mod recovery;
pub mod recovery_marshal;
pub mod services;
pub mod shutdown;
pub mod startup;
use dkg::handoff::{
    activate_vrf_material_and_publish_local_share, classify_local_reshare_role,
    find_exact_finalized_preannounce_carrier, frozen_dkg_target_expired,
    next_consensus_epoch_after_dkg_activation, next_dkg_cycle_after_restored_target,
    ordered_validator_addresses, pending_dkg_handoff_decision, pending_freeze_block_hash_decision,
    publish_randomness_status, refresh_validator_set_at_height, refresh_verifier_join_prev_output,
    restart_dkg_manager_from_finalized_history, select_pending_canonical_output,
    should_start_dkg_rotation, startup_live_join_scan_height, startup_pending_dkg_epoch_plan,
    DealerOnlyDkgActivation, DkgCeremonyReplaySpec, DkgRotationParams, DkgTaskOutcome,
    FrozenDkgTarget, FrozenValidatorSetRefresh, LocalDkgRole, PendingDkgActivation,
    PendingDkgHandoffDecision, PendingFreezeBlockHashDecision, RestoredPendingDkgActivation,
    StartupPendingDkgEpochPlan,
};
pub use dkg::persistence::migrate_dkg_keys_if_needed;
pub use epoch::run::run_consensus_stack;
pub use services::{
    ConsensusExecutionServices, ConsensusRadicleServices, ConsensusShutdownServices,
    ConsensusStackServices,
};

use dkg::persistence::{
    build_completed_dkg_boundary, decode_boundary_output, dkg_retry_store,
    persist_completed_dkg_before_activation, persist_observed_dkg_boundary_before_activation,
    restore_pending_dkg_activation, retire_activated_dkg_retry_state, DkgBoundaryContext,
    DkgStateStore, PendingDkgBoundarySnapshot,
};

use dkg::promotion::{
    adopt_finalized_boundary_carrier, promote_committed_boundary, ActiveDkgMaterial,
    BoundaryPromotion, RetireScope,
};

use dkg::startup::{
    build_genesis_dkg_boundary_artifact, obtain_threshold_material,
    ordered_addresses_from_recovered_boundary, participants_from_validator_set,
    recover_latest_boundary_artifact, resolve_startup_dkg_snapshot, select_recovery_participants,
    should_coordinate_genesis_tee_bootstrap, startup_dkg_mode, tee_bootstrap_setup,
    validate_offer_key_before_threshold_work, validate_recovered_vrf_material,
    validator_set_for_dkg_output_players, vrf_group_public_key_hash, StartupDkgMode,
    ThresholdMaterial,
};

use epoch::signer::{
    epoch_validation_inputs, radicle_signer_enabled, register_epoch_validation_providers,
    validate_validator_evm_signer, wait_for_radicle_role_change, EpochValidationCommittee,
    EpochValidationProviders, ValidatorEvmIdentity,
};

pub use epoch::watchdog::{
    elapsed_since, execution_watchdog_decision, provider_matches_consensus_tip,
    ExecutionWatchdogDecision, ExecutionWatchdogObservation,
};

use follower::{run_follow_stack, spawn_finalization_drainer};

use recovery::anchor::{
    durable_recovery_anchor_height, reconcile_recovered_execution_head,
    unfinalized_head_lead_is_recoverable,
};

use recovery::forkchoice::{confirm_recovered_forkchoice, recover_application_finalized_round};

use services::EngineHandle;

use shutdown::{supervise_epoch_loop_result, EpochLoopAction, EpochLoopOutcome};

pub use startup::{
    block_timing_from_genesis, build_peer_map, epoch_length_blocks_from_genesis,
    genesis_consensus_block, genesis_hash, map_marshal_init_height, nonzero_u16, nonzero_u64,
    nonzero_usize, parse_consensus_peers, require_genesis_hash, validate_testnet_only_flags,
};

use crate::executor::actor::RecoveredForkchoiceAttempt;
use dkg::handoff::active_set_hash_from_addresses;
use dkg::persistence::{
    clear_pending_dkg_boundary, load_pending_dkg_state, load_saved_dkg_state,
    recover_pending_dkg_boundary_snapshot, remove_pending_dkg_state, save_dkg_state,
    DkgStateMaterial, DKG_OUTPUT_FILE, DKG_POLYNOMIAL_FILE, DKG_SHARE_FILE,
};
use outbe_primitives::reshare_artifact::{
    decode_boundary_artifact, decode_outbe_block_artifacts, encode_boundary_artifact,
    ConsensusHeaderArtifact,
};
use recovery::anchor::{
    certified_follower_replay_suffix_bounds, select_certified_follower_recovery_height,
    validate_ancestor_follower_recovery_record, CertifiedFollowerRecoveryAnchor,
    CertifiedFollowerRecoveryFloors, RecoveredApplicationFinalization,
};

/// Node capabilities needed by the CL stack, unified with [`ExecutionNodeBackend`].
pub trait StackNodeAccess: ExecutionNodeBackend {}

impl<T: ExecutionNodeBackend> StackNodeAccess for T {}

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

    impl crate::follow::LocalBlockSource for GenesisNode {
        async fn get_block_by_digest(
            &self,
            _: crate::digest::Digest,
        ) -> Option<crate::block::ConsensusBlock> {
            unreachable!("genesis configuration does not read local blocks")
        }
    }

    impl ExecutionNodeBackend for GenesisNode {
        type EngineHandle = EngineHandle;
        type PayloadBuilder = reth_payload_builder::PayloadBuilderHandle<OutbePayloadTypes>;
        type StateAccess = Self::Provider;
        type BlockSource = Self;

        fn chain_spec(&self) -> Arc<reth_chainspec::ChainSpec<OutbeHeader>> {
            self.0.chain_spec.clone()
        }
        fn beacon_engine_handle(&self) -> Self::EngineHandle {
            unreachable!("genesis configuration does not access the engine")
        }
        fn payload_builder_handle(&self) -> Self::PayloadBuilder {
            unreachable!("genesis configuration does not access payload building")
        }
        fn state_access(&self) -> &Self::StateAccess {
            &self.0
        }
        fn block_source(&self) -> Self::BlockSource {
            self.clone()
        }
        fn data_dir(&self) -> std::path::PathBuf {
            unreachable!("genesis configuration does not access disk")
        }
        fn p2p_secret_path(&self) -> std::path::PathBuf {
            unreachable!("genesis configuration does not access disk")
        }
        fn has_p2p_secret_key_hex(&self) -> bool {
            unreachable!("genesis configuration does not access keys")
        }
        fn load_p2p_secret_key(&self, _: std::path::PathBuf) -> Result<[u8; 32]> {
            unreachable!("genesis configuration does not access keys")
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
