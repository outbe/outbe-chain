//! Consensus stack - wires P2P, Simplex engine, application handler, and executor.
//!
//! This is the entry point for the consensus layer, called from the consensus
//! runtime thread in `main.rs`.
//!
//! Startup flow:
//! 1. Load signing key and validator set
//! 2. Set up P2P network (register ALL channels including DKG)
//! 3. Start P2P network and register peers
//! 4. Obtain threshold material:
//!    a. From saved DKG state on disk (restart precedence)
//!    b. From CLI args (`--consensus.signing-share` + `--consensus.public-polynomial`)
//!    c. Via interactive DKG ceremony during fresh genesis formation
//! 5. Create Muxers for epoch-scoped consensus channels
//! 6. Enter epoch loop:
//!    a. Register epoch sub-channels, build HybridScheme + Reporter
//!    b. Start Simplex engine
//!    c. Monitor for reshare triggers (pending_set_change in EVM state)
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
use commonware_p2p::{
    authenticated::lookup, utils::mux::Muxer, Address, AddressableManager, Receiver as P2pReceiver,
    Sender as P2pSender,
};
use commonware_runtime::{
    buffer::paged::CacheRef, BufferPooler, Clock, Metrics, Network, Quota, Resolver, Spawner,
    Storage,
};
use commonware_utils::{ordered::Map, TryCollect as _, NZU32};
use eyre::{ensure, Result, WrapErr};
use rand_core_commonware::CryptoRng;
use reth_ethereum::chainspec::EthChainSpec as _;
use reth_ethereum::network::api::{NetworkInfo, Peers, PeersInfo};
use reth_ethereum::provider::{BlockHashReader, StateProviderFactory};
use reth_node_builder::ConsensusEngineHandle;
use reth_provider::HeaderProvider;
use std::collections::BTreeMap;
use std::future::Future;
use std::net::SocketAddr;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64, NonZeroUsize};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tracing::{debug, info, warn};

use crate::args::ConsensusArgs;
use crate::ce_recovery::CeStartupRecovery;
use crate::validators;
use outbe_consensus::{
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
    executor::actor::{ExecutorActor, FinalizedCeCommitter, RecoveredForkchoiceAttempt},
    finalization::{
        actor::{FinalizationActor, FinalizationActorDeps},
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
use outbe_node::{
    ocomp::retention::{RetainedTributeWriter, SharedOcompRetentionSelector},
    projection::ProjectionRetentionFence,
};
use outbe_radicle::integration::{RadicleVotingGate, RadicleVotingGateError};

use outbe_node::OutbeFullNode;
use outbe_ocomp_protocol::profile::poc_schema_limits;
use outbe_primitives::{
    consensus::{ConsensusExecutionBridge, DkgBoundaryArtifact},
    projection::{ProjectionCheckpoint, ProjectionReadinessHandle, WaitOutcome},
    reshare_artifact::{
        decode_boundary_artifact, decode_outbe_block_artifacts, encode_boundary_artifact,
        ConsensusHeaderArtifact,
    },
    system_tx::OcompLifecycleActivation,
    OutbeHeader, OutbePayloadTypes,
};
use reth_ethereum::storage::{BlockNumReader, BlockReader, TransactionVariant};

mod dkg;
mod epoch;
mod follower;
mod recovery;
mod services;
mod shutdown;
mod startup;
#[cfg(test)]
mod tests;

pub use dkg::persistence::migrate_dkg_keys_if_needed;

pub use epoch::run::run_consensus_stack;

pub use services::ConsensusStackServices;

pub(crate) use startup::{build_peer_map, map_marshal_init_height};

use dkg::handoff::{
    activate_vrf_material_and_publish_local_share, active_set_hash_from_addresses,
    classify_local_reshare_role, find_exact_finalized_preannounce_carrier,
    frozen_dkg_target_expired, next_consensus_epoch_after_dkg_activation,
    next_dkg_cycle_after_restored_target, ordered_validator_addresses,
    pending_dkg_handoff_decision, pending_freeze_block_hash_decision, publish_randomness_status,
    refresh_validator_set_at_height, refresh_verifier_join_prev_output,
    restart_dkg_manager_from_finalized_history, select_pending_canonical_output,
    should_start_dkg_rotation, startup_live_join_scan_height, startup_pending_dkg_epoch_plan,
    DealerOnlyDkgActivation, DkgCeremonyReplaySpec, DkgRotationParams, DkgTaskOutcome,
    FrozenDkgTarget, FrozenValidatorSetRefresh, LocalDkgRole, PendingDkgActivation,
    PendingDkgHandoffDecision, PendingFreezeBlockHashDecision, RestoredPendingDkgActivation,
    StartupPendingDkgEpochPlan,
};

use dkg::persistence::{
    build_completed_dkg_boundary, clear_pending_dkg_boundary, decode_boundary_output,
    dkg_retry_store, load_pending_dkg_state, load_saved_dkg_state,
    persist_completed_dkg_before_activation, persist_observed_dkg_boundary_before_activation,
    recover_pending_dkg_boundary_snapshot, remove_pending_dkg_state,
    restore_pending_dkg_activation, retire_activated_dkg_retry_state, save_dkg_state,
    PendingDkgBoundarySnapshot, DKG_OUTPUT_FILE, DKG_POLYNOMIAL_FILE, DKG_SHARE_FILE,
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
    validate_validator_evm_signer, wait_for_radicle_role_change,
};

use epoch::watchdog::{
    elapsed_since, execution_watchdog_decision, provider_matches_consensus_tip,
    ExecutionWatchdogDecision, ExecutionWatchdogObservation,
};

use follower::{run_follow_stack, spawn_finalization_drainer};

use recovery::anchor::{
    certified_follower_replay_suffix_bounds, durable_recovery_anchor_height,
    reconcile_recovered_execution_head, recover_ce_at_reconciled_anchor,
    select_certified_follower_recovery_height, unfinalized_head_lead_is_recoverable,
    validate_certified_follower_recovery_record, CertifiedFollowerRecoveryAnchor,
    CertifiedFollowerRecoveryFloors, RecoveredApplicationFinalization,
};

use recovery::forkchoice::{
    confirm_recovered_forkchoice, read_reth_recovery_forkchoice,
    recover_application_finalized_round, wait_for_recovered_projection,
};

use services::EngineHandle;

use shutdown::{supervise_epoch_loop_result, EpochLoopAction, EpochLoopOutcome};

use startup::{
    block_timing_from_genesis, epoch_length_blocks_from_genesis, genesis_consensus_block,
    genesis_hash, nonzero_u16, nonzero_u64, nonzero_usize, ocomp_p2p_namespace,
    parse_consensus_peers, validate_testnet_only_flags,
};

#[cfg(test)]
use dkg::handoff::preannounce_matches_pending;

#[cfg(test)]
use dkg::persistence::{
    decode_pending_dkg_boundary_snapshot, encode_pending_dkg_boundary_snapshot,
    load_pending_dkg_boundary, pending_boundary_is_finalized, save_pending_dkg_boundary,
    save_pending_dkg_state, DKG_DEALER_RETRY_FILE, DKG_PENDING_BOUNDARY_FILE,
    DKG_PENDING_BOUNDARY_TMP_FILE, DKG_PENDING_OUTPUT_FILE, DKG_PENDING_POLYNOMIAL_FILE,
    DKG_PENDING_SHARE_FILE, DKG_PLAYER_RETRY_FILE,
};

#[cfg(test)]
use dkg::startup::{
    genesis_formation_gate_decision, genesis_formation_required_remote_peers,
    missing_current_threshold_material_error, vrf_material_matches_recovered_boundary,
    GenesisFormationGate, RethGenesisPeerEvidence, RethGenesisPeerStatus, StartupDkgContext,
};

#[cfg(test)]
use epoch::run::{epoch_elector_config, radicle_channel_config};

#[cfg(test)]
use follower::{
    build_certified_follower_parent_record, follower_height_has_certified_finalization,
};

#[cfg(test)]
use recovery::anchor::MAX_UNFINALIZED_HEAD_LEAD;

#[cfg(test)]
use recovery::forkchoice::{
    classify_recovered_fcu_attempt, RecoveredFcuAction, RecoveredRethForkchoice,
};

#[cfg(test)]
use shutdown::preserve_stack_result_after_drain;

#[cfg(test)]
use startup::{read_ms, require_genesis_hash, validate_timing};
