use crate::execution_backend::read_reth_recovery_forkchoice;
use alloy_primitives::{Address, Bytes, B256};
use commonware_actor::{Feedback, Unreliable};
use commonware_codec::Encode as _;
use commonware_consensus::{
    marshal::{resolver::handler, Update},
    simplex::{
        elector::{Config as _, Elector as _, RoundRobin},
        types::{Activity, Finalization, Finalize, Proposal, Subject},
        Config as SimplexConfig, Engine as SimplexEngine, Floor, ForwardPolicy,
    },
    types::{Epoch, Height, Round, View, ViewDelta},
    Reporter,
};
use commonware_cryptography::bls12381;
use commonware_cryptography::bls12381::{
    dkg::feldman_desmedt::Output,
    primitives::{group::Share, sharing::Sharing},
};
use commonware_cryptography::bls12381::{primitives::variant::MinSig, PrivateKey};
use commonware_cryptography::certificate::{Scheme as _, Verifier as _};
use commonware_cryptography::sha256::Digest as Sha256Digest;
use commonware_cryptography::Signer as _;
use commonware_cryptography::{Hasher as _, Sha256};
use commonware_math::algebra::Random;
use commonware_p2p::authenticated::lookup;
use commonware_p2p::{Blocker, CheckedSender, LimitedSender, Message, Receiver, Recipients};
use commonware_parallel::Sequential;
use commonware_runtime::{
    buffer::paged::CacheRef, tokio as commonware_tokio, IoBufs, Runner as _, Supervisor as _,
};
use commonware_runtime::{Clock, Quota, Spawner};
use commonware_storage::archive::immutable;
use commonware_utils::TryCollect as _;
use commonware_utils::NZU32;
use commonware_utils::{
    acknowledgement::Acknowledgement,
    ordered::{Quorum as _, Set},
    NZUsize,
};
use dkg::handoff::preannounce_matches_pending;
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
use dkg::persistence::{
    decode_boundary_output, persist_completed_dkg_before_activation,
    retire_activated_dkg_retry_state, DkgBoundaryContext, DkgStateStore,
    PendingDkgBoundarySnapshot,
};
use dkg::persistence::{
    decode_pending_dkg_boundary_snapshot, encode_pending_dkg_boundary_snapshot,
    load_pending_dkg_boundary, pending_boundary_is_finalized, save_pending_dkg_boundary,
    save_pending_dkg_state, DKG_DEALER_RETRY_FILE, DKG_PENDING_BOUNDARY_FILE,
    DKG_PENDING_BOUNDARY_TMP_FILE, DKG_PENDING_OUTPUT_FILE, DKG_PENDING_POLYNOMIAL_FILE,
    DKG_PENDING_SHARE_FILE, DKG_PLAYER_RETRY_FILE,
};
use dkg::promotion::{
    adopt_finalized_boundary_carrier, promote_committed_boundary, ActiveDkgMaterial,
    BoundaryPromotion, RetireScope,
};
use dkg::startup::{
    build_genesis_dkg_boundary_artifact, obtain_threshold_material,
    ordered_addresses_from_recovered_boundary, select_recovery_participants,
    should_coordinate_genesis_tee_bootstrap, startup_dkg_mode,
    validate_offer_key_before_threshold_work, validate_recovered_vrf_material,
    vrf_group_public_key_hash, StartupDkgMode, ThresholdMaterial,
};
use dkg::startup::{
    genesis_formation_gate_decision, genesis_formation_required_remote_peers,
    vrf_material_matches_recovered_boundary, GenesisFormationGate, RethGenesisPeerEvidence,
    RethGenesisPeerStatus, StartupDkgContext,
};
use epoch::signer::{
    radicle_signer_enabled, register_epoch_validation_providers, validate_validator_evm_signer,
    wait_for_radicle_role_change, EpochValidationCommittee, EpochValidationProviders,
    ValidatorEvmIdentity,
};
use epoch::simplex::epoch_elector_config;
use epoch::transport::radicle_channel_config;
use epoch::watchdog::{
    execution_watchdog_decision, provider_matches_consensus_tip, ExecutionWatchdogDecision,
    ExecutionWatchdogObservation,
};
use eyre::Result;
use futures::FutureExt as _;
use outbe_consensus::args::ConsensusArgs;
use outbe_consensus::stack::dkg::persistence::migrate_dkg_keys_if_needed;
use outbe_consensus::stack::follower::spawn_finalization_drainer;
use outbe_consensus::stack::follower::{
    build_certified_follower_parent_record, follower_height_has_certified_finalization,
};
use outbe_consensus::stack::recovery::anchor::RecoveredApplicationFinalization;
use outbe_consensus::stack::recovery::anchor::MAX_UNFINALIZED_HEAD_LEAD;
use outbe_consensus::stack::recovery::anchor::{
    durable_recovery_anchor_height, reconcile_recovered_execution_head,
    unfinalized_head_lead_is_recoverable,
};
use outbe_consensus::stack::recovery::forkchoice::confirm_recovered_forkchoice;
use outbe_consensus::stack::recovery::forkchoice::{
    classify_recovered_fcu_attempt, recover_application_finalized_round, RecoveredFcuAction,
    RecoveredRethForkchoice,
};
use outbe_consensus::stack::shutdown::preserve_stack_result_after_drain;
use outbe_consensus::stack::shutdown::{
    supervise_epoch_loop_result, EpochLoopAction, EpochLoopOutcome,
};
use outbe_consensus::stack::{dkg, epoch, marshal_archive, startup};
use outbe_consensus::validators;
use outbe_consensus::{
    application::ApplicationEpochFence,
    bls, dkg_actor,
    dkg_manager::{self, Mailbox as DkgManagerMailbox},
    executor::actor::ExecutorActor,
    finalization::state::new_finalization_view,
    vrf_safety::VrfSafetyGate,
};
use outbe_consensus::{
    block::ConsensusBlock,
    bls::bootstrap_dkg,
    committee_provider::CommitteeProvider,
    hybrid::{HybridScheme, HybridSchemeProvider, VrfMaterialProvider},
    reporter::ReporterContinuity,
    test_harness::{mock_genesis, MockAutomaton, MockRelay, MockReporter},
};
use outbe_consensus::{config, digest::Digest};
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
use outbe_primitives::reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact};
use outbe_primitives::OutbeHeader;
use outbe_primitives::{
    consensus::{ConsensusExecutionBridge, DkgBoundaryArtifact},
    projection::ProjectionCheckpoint,
};
use outbe_radicle::integration::{RadicleStatusChannel, RadicleVotingGate, RadicleVotingGateError};
use reth_ethereum::provider::BlockHashReader;
use reth_ethereum::{
    primitives::{Header, SealedBlock, SealedHeader},
    Block,
};
use reth_node_builder::ConsensusEngineHandle;
use reth_provider::HeaderProvider;
use reth_provider::ProviderResult;
use startup::{build_peer_map, map_marshal_init_height};
use startup::{nonzero_u16, nonzero_usize, parse_consensus_peers, validate_testnet_only_flags};
use startup::{read_ms, require_genesis_hash, validate_timing};
use std::future::Future;
use std::{
    collections::BTreeMap,
    convert::Infallible,
    marker::PhantomData,
    num::NonZeroU16,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Barrier, Mutex as StdMutex,
    },
    time::{Duration, SystemTime},
};

mod boundary_promotion;
mod configuration;
mod continuity;
mod dkg_persistence;
mod epoch_handoff;
mod epoch_transition_harness;
mod fixtures;
mod follower;
mod genesis_formation;
pub(in crate::stack) mod harness;
mod lifecycle;
mod local_anchor;
mod marshal_reporter;
#[cfg(test)]
mod muxer_contract;
mod recovery;
/// Restart recovery: distinguishing a benign "execution head leads the marshal
/// finalized tip" restart (an unfinalized in-flight head) from genuine archive
/// corruption. See `unfinalized_head_lead_is_recoverable` + the recover match arm.
#[cfg(test)]
mod restart_recovery;
mod shutdown;
mod signer_replacement;
mod startup_material;

use fixtures::{
    bootstrap_fixture_signers, certify_fixture_proposal, recovery_block,
    recovery_finalization_fixture, run_test_dkg, run_test_dkg_complete,
    test_boundary_with_vrf_hash, MockFinalizedHeaderProvider,
};

use harness::{start_recovery_marshal, start_recovery_marshal_with_reporter};

#[test]
fn execution_bridge_accepts_launched_node_ports() {
    fn require_backend<T>()
    where
        T: outbe_consensus::execution_port::ExecutionNodeBackend
            + outbe_consensus::execution_port::RecoveryForkchoiceReader
            + outbe_consensus::stack::StackNodeAccess
            + outbe_consensus::follow::LocalBlockSource
            + From<outbe_execution::OutbeFullNode>,
    {
    }
    require_backend::<crate::RethExecutionBackend>();
    let _run: fn(
        commonware_tokio::Context,
        ConsensusArgs,
        outbe_execution::OutbeFullNode,
        ConsensusExecutionBridge,
        outbe_consensus::stack::ConsensusStackServices,
    ) -> _ = crate::run_consensus_with_execution;
}
