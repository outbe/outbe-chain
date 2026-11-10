//! Compatibility exports for the consensus-owned stack and engine integration tests.

#[cfg(test)]
use commonware_codec::Encode as _;
#[cfg(test)]
use commonware_cryptography::bls12381::{
    dkg::feldman_desmedt::Output,
    primitives::{group::Share, sharing::Sharing},
};
#[cfg(test)]
use commonware_p2p::authenticated::lookup;
#[cfg(test)]
use commonware_runtime::{Clock, Quota, Spawner};
#[cfg(test)]
use commonware_utils::NZU32;
#[cfg(test)]
use eyre::Result;

#[cfg(test)]
use reth_ethereum::provider::BlockHashReader;
#[cfg(test)]
use reth_provider::HeaderProvider;
#[cfg(test)]
use std::future::Future;

#[cfg(test)]
use crate::args::ConsensusArgs;
#[cfg(test)]
use crate::validators;
#[cfg(test)]
use outbe_consensus::{
    application::ApplicationEpochFence,
    bls, dkg_actor,
    dkg_manager::{self, Mailbox as DkgManagerMailbox},
    executor::actor::ExecutorActor,
    finalization::state::new_finalization_view,
    vrf_safety::VrfSafetyGate,
};

#[cfg(test)]
use outbe_primitives::{
    consensus::{ConsensusExecutionBridge, DkgBoundaryArtifact},
    projection::ProjectionCheckpoint,
};

pub use outbe_consensus::stack::dkg;
pub use outbe_consensus::stack::epoch;
pub use outbe_consensus::stack::follower;
pub use outbe_consensus::stack::marshal_archive;
pub use outbe_consensus::stack::recovery;
#[cfg(any(test, feature = "snapshot-integration"))]
pub use outbe_consensus::stack::recovery_marshal;
pub use outbe_consensus::stack::services;
pub use outbe_consensus::stack::shutdown;
#[cfg(feature = "snapshot-integration")]
#[doc(hidden)]
pub mod snapshot_native;
pub use outbe_consensus::stack::startup;
#[cfg(test)]
mod tests;

pub use dkg::persistence::migrate_dkg_keys_if_needed;

pub use epoch::run::run_consensus_stack;

pub use services::{
    ConsensusExecutionServices, ConsensusRadicleServices, ConsensusShutdownServices,
    ConsensusStackServices,
};

#[cfg(test)]
pub(crate) use startup::{build_peer_map, map_marshal_init_height};

#[cfg(test)]
use dkg::handoff::{
    activate_vrf_material_and_publish_local_share, classify_local_reshare_role,
    find_exact_finalized_preannounce_carrier, frozen_dkg_target_expired,
    next_consensus_epoch_after_dkg_activation, next_dkg_cycle_after_restored_target,
    ordered_validator_addresses, pending_dkg_handoff_decision, pending_freeze_block_hash_decision,
    publish_randomness_status, restart_dkg_manager_from_finalized_history,
    select_pending_canonical_output, should_start_dkg_rotation, startup_live_join_scan_height,
    startup_pending_dkg_epoch_plan, DealerOnlyDkgActivation, DkgCeremonyReplaySpec,
    DkgRotationParams, FrozenDkgTarget, LocalDkgRole, PendingDkgHandoffDecision,
    PendingFreezeBlockHashDecision, StartupPendingDkgEpochPlan,
};

#[cfg(test)]
use dkg::persistence::{
    decode_boundary_output, persist_completed_dkg_before_activation,
    retire_activated_dkg_retry_state, DkgBoundaryContext, DkgStateStore,
    PendingDkgBoundarySnapshot,
};

#[cfg(test)]
use dkg::promotion::{
    adopt_finalized_boundary_carrier, promote_committed_boundary, ActiveDkgMaterial,
    BoundaryPromotion, RetireScope,
};

#[cfg(test)]
use dkg::startup::{
    build_genesis_dkg_boundary_artifact, obtain_threshold_material,
    ordered_addresses_from_recovered_boundary, select_recovery_participants,
    should_coordinate_genesis_tee_bootstrap, startup_dkg_mode,
    validate_offer_key_before_threshold_work, validate_recovered_vrf_material,
    vrf_group_public_key_hash, StartupDkgMode, ThresholdMaterial,
};

#[cfg(test)]
use epoch::signer::{
    radicle_signer_enabled, register_epoch_validation_providers, validate_validator_evm_signer,
    wait_for_radicle_role_change, EpochValidationCommittee, EpochValidationProviders,
    ValidatorEvmIdentity,
};

#[cfg(test)]
use epoch::watchdog::{
    execution_watchdog_decision, provider_matches_consensus_tip, ExecutionWatchdogDecision,
    ExecutionWatchdogObservation,
};

#[cfg(test)]
use follower::spawn_finalization_drainer;

#[cfg(test)]
use recovery::anchor::{
    durable_recovery_anchor_height, reconcile_recovered_execution_head,
    unfinalized_head_lead_is_recoverable,
};

#[cfg(test)]
use recovery::forkchoice::confirm_recovered_forkchoice;

#[cfg(test)]
use shutdown::{supervise_epoch_loop_result, EpochLoopAction, EpochLoopOutcome};

#[cfg(test)]
use startup::{nonzero_u16, nonzero_usize, parse_consensus_peers, validate_testnet_only_flags};

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
    vrf_material_matches_recovered_boundary, GenesisFormationGate, RethGenesisPeerEvidence,
    RethGenesisPeerStatus, StartupDkgContext,
};

#[cfg(test)]
use epoch::simplex::epoch_elector_config;
#[cfg(test)]
use epoch::transport::radicle_channel_config;

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

#[cfg(test)]
use outbe_consensus::{
    executor::actor::RecoveredForkchoiceAttempt,
    stack::{
        dkg::{
            handoff::active_set_hash_from_addresses,
            persistence::{
                clear_pending_dkg_boundary, load_pending_dkg_state, load_saved_dkg_state,
                save_dkg_state, DkgStateMaterial, DKG_OUTPUT_FILE, DKG_POLYNOMIAL_FILE,
                DKG_SHARE_FILE,
            },
        },
        recovery::anchor::{
            certified_follower_replay_suffix_bounds, select_certified_follower_recovery_height,
            validate_ancestor_follower_recovery_record, CertifiedFollowerRecoveryFloors,
        },
    },
};
#[cfg(test)]
use outbe_primitives::reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact};

#[cfg(test)]
use reth_node_builder::ConsensusEngineHandle;

#[cfg(feature = "snapshot-integration")]
use commonware_consensus::types::Round;
#[cfg(feature = "snapshot-integration")]
use outbe_primitives::OutbeHeader;
#[cfg(all(test, feature = "snapshot-integration"))]
#[cfg(test)]
use outbe_primitives::OutbePayloadTypes;
#[cfg(any(test, feature = "snapshot-integration"))]
use recovery::anchor::RecoveredApplicationFinalization;

#[cfg(feature = "snapshot-integration")]
use alloy_primitives::B256;
#[cfg(feature = "snapshot-integration")]
use commonware_consensus::types::{Epoch, Height};
#[cfg(any(test, feature = "snapshot-integration"))]
use commonware_cryptography::bls12381;
#[cfg(feature = "snapshot-integration")]
use commonware_cryptography::bls12381::primitives::variant::MinSig;
#[cfg(feature = "snapshot-integration")]
use outbe_consensus::hybrid::{HybridScheme, HybridSchemeProvider};
#[cfg(any(test, feature = "snapshot-integration"))]
use outbe_consensus::{config, digest::Digest};
#[cfg(any(test, feature = "snapshot-integration"))]
use recovery::forkchoice::{read_reth_recovery_forkchoice, recover_application_finalized_round};
#[cfg(feature = "snapshot-integration")]
use std::{sync::Arc, time::Duration};

#[cfg(any(test, feature = "snapshot-integration"))]
use commonware_cryptography::Signer as _;

#[cfg(any(test, feature = "snapshot-integration"))]
use commonware_utils::TryCollect as _;
