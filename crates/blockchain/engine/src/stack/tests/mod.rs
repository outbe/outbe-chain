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
    durable_recovery_anchor_height, reconcile_authenticated_execution_head,
    reconcile_recovered_execution_head, unfinalized_head_lead_is_recoverable,
    CanonicalHeaderRecord, RecoveryCommitteeConfig, StaticCanonicalHistory,
};
use outbe_consensus::stack::recovery::forkchoice::confirm_recovered_forkchoice;
use outbe_consensus::stack::recovery::forkchoice::{
    classify_recovered_fcu_attempt, recover_application_finalized_round,
    recover_authenticated_application_finalization, RecoveredFcuAction, RecoveredRethForkchoice,
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
mod marshal_restart_seam;
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

pub(super) struct AuthenticatedRecoveryCommittee {
    pub(super) epoch: u64,
    pub(super) participants: Set<bls12381::PublicKey>,
    pub(super) signers: Vec<HybridScheme<MinSig>>,
    pub(super) verifier: HybridScheme<MinSig>,
    pub(super) snapshot: outbe_consensus::proof::CommitteeSnapshot,
    pub(super) boundary_extra_data: Bytes,
    pub(super) preannounce_extra_data: Bytes,
}

impl AuthenticatedRecoveryCommittee {
    pub(super) fn new(
        epoch: u64,
        seed_base: u64,
        freeze_height: u64,
        planned_activation_height: u64,
    ) -> Self {
        use commonware_cryptography::bls12381::dkg::feldman_desmedt::{Dealer, Info, Logs, Player};
        use commonware_cryptography::bls12381::primitives::sharing::Mode;
        use commonware_utils::N3f1;

        let mut keys: Vec<bls12381::PrivateKey> = (0u64..3)
            .map(|i| bls12381::PrivateKey::from_seed(seed_base + i + 1))
            .collect();
        keys.sort_by(|a, b| {
            commonware_codec::Encode::encode(&a.public_key())
                .cmp(&commonware_codec::Encode::encode(&b.public_key()))
        });
        let participants: Set<bls12381::PublicKey> =
            keys.iter().map(|k| k.public_key()).try_collect().unwrap();

        let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
            b"outbe-recovery-test",
            epoch,
            None,
            Mode::NonZeroCounter,
            commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
            participants.clone(),
            participants.clone(),
        )
        .unwrap();

        let mut dealers = Vec::new();
        let mut pub_msgs = Vec::new();
        let mut all_priv_msgs = Vec::new();
        for key in &keys {
            let (dealer, pub_msg, priv_msgs) =
                Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
                    rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
                    info.clone(),
                    key.clone(),
                    None,
                )
                .unwrap();
            dealers.push(dealer);
            pub_msgs.push(pub_msg);
            all_priv_msgs.push(priv_msgs);
        }
        let mut players: Vec<Player<MinSig, bls12381::PrivateKey>> = keys
            .iter()
            .map(|k| Player::new(info.clone(), k.clone()).unwrap())
            .collect();
        outbe_consensus::test_harness::acknowledge_fixture_dealings(
            &keys,
            outbe_consensus::test_harness::FixtureDealings {
                public_messages: &pub_msgs,
                private_messages: &all_priv_msgs,
            },
            &mut dealers,
            &mut players,
        );
        let mut logs = BTreeMap::new();
        for dealer in dealers {
            let signed_log = dealer.finalize::<N3f1>();
            if let Some((pk, log)) = signed_log.check(&info) {
                logs.insert(pk, log);
            }
        }
        let mut dkg_logs = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(info);
        for (pk, log) in logs {
            dkg_logs.record(pk, log);
        }
        let mut output_opt = None;
        let mut shares = Vec::with_capacity(players.len());
        for player in players {
            let (output, share) = player
                .finalize::<N3f1>(
                    &mut rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
                    dkg_logs.clone(),
                    &Sequential,
                )
                .unwrap();
            output_opt = Some(output);
            shares.push(share);
        }
        let output = output_opt.expect("dkg output");
        let polynomial = output.public().clone();

        let signers: Vec<HybridScheme<MinSig>> = keys
            .iter()
            .zip(shares.iter())
            .map(|(key, share)| {
                HybridScheme::signer_with_vrf_provider(
                    &config::outbe_app_namespace(),
                    participants.clone(),
                    key.clone(),
                    VrfMaterialProvider::new(epoch, polynomial.clone(), Some(share.clone())),
                )
                .unwrap()
            })
            .collect();
        let verifier = HybridScheme::<MinSig>::verifier_with_vrf_provider(
            &config::outbe_app_namespace(),
            participants.clone(),
            VrfMaterialProvider::new(epoch, polynomial.clone(), None),
        )
        .unwrap();

        let ordered_pks: Vec<bls12381::PublicKey> = participants.iter().cloned().collect();
        let group_pk = verifier.identity().expect("verifier identity");
        let snapshot =
            outbe_consensus::test_harness::committee_snapshot(&ordered_pks, &group_pk, epoch);
        let addresses: Vec<Address> = snapshot.committee.iter().map(|e| e.address).collect();
        let validator_set = outbe_primitives::validators::ValidatorSet {
            public_keys: ordered_pks,
            addresses,
            p2p_addresses: Vec::new(),
        };
        let boundary = outbe_consensus::test_harness::boundary_artifact(
            epoch,
            &validator_set,
            &output,
            outbe_consensus::test_harness::BoundaryFixtureSettings {
                is_full_dkg: epoch == 0,
                freeze_height,
                planned_activation_height,
                is_validator_set_change: false,
            },
        );
        let boundary_extra_data = outbe_primitives::reshare_artifact::encode_outbe_block_artifacts(
            &outbe_primitives::reshare_artifact::OutbeBlockArtifacts {
                consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(
                    boundary.clone(),
                )),
                ..Default::default()
            },
        )
        .unwrap();
        let preannounce_extra_data =
            outbe_primitives::reshare_artifact::encode_outbe_block_artifacts(
                &outbe_primitives::reshare_artifact::OutbeBlockArtifacts {
                    consensus_header_artifact: Some(
                        ConsensusHeaderArtifact::CommitteePreAnnounce {
                            epoch,
                            outcome: boundary.outcome.clone(),
                        },
                    ),
                    ..Default::default()
                },
            )
            .unwrap();

        Self {
            epoch,
            participants,
            signers,
            verifier,
            snapshot,
            boundary_extra_data,
            preannounce_extra_data,
        }
    }

    pub(super) fn sign_finalization(
        &self,
        view: u64,
        parent_view: u64,
        digest: Digest,
    ) -> Finalization<HybridScheme<MinSig>, Digest> {
        let proposal = Proposal::new(
            Round::new(Epoch::new(self.epoch), View::new(view)),
            View::new(parent_view),
            digest,
        );
        certify_fixture_proposal(&self.verifier, &self.signers, proposal)
    }

    pub(super) fn sign_notarization(
        &self,
        view: u64,
        parent_view: u64,
        digest: Digest,
    ) -> commonware_consensus::simplex::types::Notarization<HybridScheme<MinSig>, Digest> {
        use commonware_consensus::simplex::types::{Notarization, Notarize};
        let proposal = Proposal::new(
            Round::new(Epoch::new(self.epoch), View::new(view)),
            View::new(parent_view),
            digest,
        );
        let votes: Vec<_> = self
            .signers
            .iter()
            .map(|signer| Notarize::sign(signer, proposal.clone()).unwrap())
            .collect();
        let attestations =
            commonware_utils::iter::NonEmpty::try_new(votes.into_iter().map(|v| v.attestation))
                .unwrap();
        let certificate = self.verifier.assemble(attestations, &Sequential).unwrap();
        Notarization {
            proposal,
            certificate,
        }
    }

    pub(super) fn block_with_extra_data(
        number: u64,
        parent_hash: B256,
        extra_data: Bytes,
    ) -> ConsensusBlock {
        let mut block = Block::default();
        block.header.number = number;
        block.header.parent_hash = parent_hash;
        block.header.extra_data = extra_data;
        let mut raw = block.map_header(OutbeHeader::new);
        raw.header.inner.transactions_root =
            alloy_consensus::proofs::calculate_transaction_root(&raw.body.transactions);
        ConsensusBlock::from_sealed(SealedBlock::seal_slow(raw))
    }

    pub(super) fn block_with_parent_witness(
        &self,
        parent: &ConsensusBlock,
        parent_view: u64,
        parent_parent_view: u64,
        proof_kind: outbe_primitives::consensus_metadata::ParentParticipationProof,
        extra_data: Bytes,
    ) -> ConsensusBlock {
        use outbe_primitives::consensus_metadata::ParentParticipationProof;
        use outbe_primitives::system_tx::{
            build_unsigned_system_tx, SystemTx, CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
        };

        let proof_bytes: Vec<u8> = match proof_kind {
            ParentParticipationProof::Finalization => self
                .sign_finalization(parent_view, parent_parent_view, parent.digest())
                .encode()
                .to_vec(),
            ParentParticipationProof::CertifiedNotarization => self
                .sign_notarization(parent_view, parent_parent_view, parent.digest())
                .encode()
                .to_vec(),
        };
        let metadata = outbe_consensus::test_harness::test_fully_signed_metadata(
            &outbe_consensus::test_harness::TestFinalizedParent {
                block_number: parent.number(),
                block_hash: parent.digest().0,
                epoch: self.epoch,
                view: parent_view,
                parent_view: parent_parent_view,
                vrf_material_version: self.epoch,
                proof_kind,
            },
            &self.snapshot,
            &proof_bytes,
        );
        let block_number = parent.number() + 1;
        let signer = outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes([9; 32]).unwrap();
        let mut block = Block::default();
        block.header.number = block_number;
        block.header.parent_hash = parent.digest().0;
        block.header.extra_data = extra_data;
        block.header.gas_limit =
            outbe_primitives::system_tx::protocol_block_gas_limit(block_number);
        let begin_input = SystemTx::Consensus {
            step_index: CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
            payload: metadata.encode().expect("encode phase1 metadata"),
        };
        let unsigned_begin = build_unsigned_system_tx(
            0,
            block_number,
            outbe_primitives::chain::CHAIN_ID,
            begin_input.encode().expect("encode system tx"),
        )
        .expect("build begin system tx");
        block
            .body
            .transactions
            .push(signer.sign_unsigned(unsigned_begin).expect("sign begin tx"));
        if outbe_primitives::block::BlockHeightRegime::for_block(block_number)
            .has_end_block_system_txs()
        {
            for step_index in 0..5 {
                let end_input = SystemTx::EndBlock { step_index };
                let unsigned_end = build_unsigned_system_tx(
                    end_input.nonce_ordinal().expect("end ordinal"),
                    block_number,
                    outbe_primitives::chain::CHAIN_ID,
                    end_input.encode().expect("encode end tx"),
                )
                .expect("build end tx");
                block
                    .body
                    .transactions
                    .push(signer.sign_unsigned(unsigned_end).expect("sign end tx"));
            }
        }
        let mut raw = block.map_header(OutbeHeader::new);
        raw.header.inner.transactions_root =
            alloy_consensus::proofs::calculate_transaction_root(&raw.body.transactions);
        ConsensusBlock::from_sealed(SealedBlock::seal_slow(raw))
    }

    pub(super) fn header_record(block: &ConsensusBlock) -> CanonicalHeaderRecord {
        CanonicalHeaderRecord {
            number: block.number(),
            hash: block.digest().0,
            parent_hash: block.parent_hash(),
            extra_data: block.header().inner.extra_data.clone(),
        }
    }
}

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
