use super::*;
use alloy_primitives::{Address, Bytes, B256};
use commonware_actor::Feedback;
use commonware_codec::Encode as _;
use commonware_consensus::{
    marshal::{self, core::Buffer, resolver::handler, Start, Update},
    simplex::{
        elector::{Config as _, Elector as _},
        types::{Activity, Finalization, Finalize, Proposal, Subject},
    },
    types::{Epoch, FixedEpocher, Height, Round, View, ViewDelta},
    Reporter,
};
use commonware_cryptography::certificate::{Provider as _, Scheme as _};
use commonware_cryptography::sha256::Digest as Sha256Digest;
use commonware_cryptography::Hasher as _;
use commonware_math::algebra::Random;
use commonware_p2p::Recipients;
use commonware_parallel::Sequential;
use commonware_resolver::Resolver;
use commonware_resolver::TargetedResolver;
use commonware_runtime::{buffer::paged::CacheRef, Runner as _, Supervisor as _};
use commonware_storage::archive::immutable;
use commonware_utils::{
    acknowledgement::Acknowledgement, channel::oneshot, ordered::Quorum as _, vec::NonEmptyVec,
};
use outbe_consensus::{
    block::ConsensusBlock,
    bls::bootstrap_dkg,
    committee_provider::CommitteeProvider,
    hybrid::{HybridScheme, HybridSchemeProvider, VrfMaterialProvider},
    reporter::ReporterContinuity,
};
use outbe_primitives::OutbeHeader;
use reth_ethereum::{primitives::SealedBlock, Block};
use reth_provider::ProviderResult;
use std::{
    collections::BTreeMap,
    num::{NonZeroU16, NonZeroU64, NonZeroUsize},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, SystemTime},
};

static STACK_MARSHAL_TEST_ID: AtomicU64 = AtomicU64::new(0);

#[test]
fn testnet_clock_offset_is_rejected_for_unregistered_networks() {
    let unknown_production_chain = 1_000_000_001;
    let error = validate_testnet_only_flags(false, false, Some(1), unknown_production_chain)
        .unwrap_err()
        .to_string();
    assert!(error.contains("--testnet.unix-time-offset-secs"));
}

#[test]
fn testnet_clock_offset_is_allowed_only_on_explicit_test_networks() {
    for chain_id in [
        outbe_primitives::chain::DEVNET_CHAIN_ID,
        outbe_primitives::chain::TESTNET_CHAIN_ID,
    ] {
        validate_testnet_only_flags(false, false, Some(-60), chain_id).unwrap();
    }
}

/// Run a minimal 3-node DKG to get a valid (Output, Share) for testing.
#[allow(clippy::type_complexity)]
fn run_test_dkg_complete() -> (
    Vec<bls12381::PrivateKey>,
    commonware_utils::ordered::Set<bls12381::PublicKey>,
    Output<MinSig, bls12381::PublicKey>,
    Share,
    Sharing<MinSig>,
) {
    use commonware_cryptography::bls12381::dkg::feldman_desmedt::{Dealer, Info, Player};
    use commonware_cryptography::bls12381::primitives::sharing::Mode;
    use commonware_parallel::Sequential;
    use commonware_utils::N3f1;

    let mut keys: Vec<bls12381::PrivateKey> = (0..3)
        .map(|_| bls12381::PrivateKey::random(rand_core::OsRng))
        .collect();
    keys.sort_by(|a, b| {
        commonware_codec::Encode::encode(&a.public_key())
            .cmp(&commonware_codec::Encode::encode(&b.public_key()))
    });

    let participants: commonware_utils::ordered::Set<bls12381::PublicKey> =
        keys.iter().map(|k| k.public_key()).try_collect().unwrap();

    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        b"test",
        0,
        None,
        Mode::NonZeroCounter,
        participants.clone(),
        participants.clone(),
    )
    .unwrap();

    // Each validator deals and acks.
    let mut dealers = Vec::new();
    let mut pub_msgs = Vec::new();
    let mut all_priv_msgs = Vec::new();

    for key in &keys {
        let (dealer, pub_msg, priv_msgs) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
            rand_core::OsRng,
            info.clone(),
            key.clone(),
            None,
        )
        .unwrap();
        dealers.push(dealer);
        pub_msgs.push(pub_msg);
        all_priv_msgs.push(priv_msgs);
    }

    // Each player receives from all dealers.
    let mut players: Vec<Player<MinSig, bls12381::PrivateKey>> = keys
        .iter()
        .map(|k| Player::new(info.clone(), k.clone()).unwrap())
        .collect();

    for (dealer_idx, (pub_msg, priv_msgs)) in pub_msgs.iter().zip(all_priv_msgs.iter()).enumerate()
    {
        let dealer_pk = keys[dealer_idx].public_key();
        for (player_pk, priv_msg) in priv_msgs {
            let player_idx = keys
                .iter()
                .position(|k| &k.public_key() == player_pk)
                .unwrap();
            if let Some(ack) = players[player_idx].dealer_message::<N3f1>(
                dealer_pk.clone(),
                pub_msg.clone(),
                priv_msg.clone(),
            ) {
                dealers[dealer_idx]
                    .receive_player_ack(player_pk.clone(), ack)
                    .unwrap();
            }
        }
    }

    // Finalize all dealers.
    let mut logs = std::collections::BTreeMap::new();
    for dealer in dealers {
        let signed_log = dealer.finalize::<N3f1>();
        if let Some((pk, log)) = signed_log.check(&info) {
            logs.insert(pk, log);
        }
    }

    // Player 0 finalizes.
    let mut dkg_logs = commonware_cryptography::bls12381::dkg::feldman_desmedt::Logs::<
        MinSig,
        bls12381::PublicKey,
        N3f1,
    >::new(info.clone());
    for (dealer_pk, log) in logs {
        dkg_logs.record(dealer_pk, log);
    }
    let (output, share) = players
        .remove(0)
        .finalize::<N3f1, commonware_cryptography::bls12381::Batch>(
            &mut rand_core::OsRng,
            dkg_logs,
            &Sequential,
        )
        .unwrap();
    let polynomial = output.public().clone();

    (keys, participants, output, share, polynomial)
}

fn run_test_dkg() -> (
    Vec<bls12381::PrivateKey>,
    commonware_utils::ordered::Set<bls12381::PublicKey>,
    Output<MinSig, bls12381::PublicKey>,
    Sharing<MinSig>,
) {
    let (keys, participants, _output, _share, polynomial) = run_test_dkg_complete();
    (keys, participants, _output, polynomial)
}

fn sample_certificate() -> outbe_consensus::hybrid::HybridCertificate<MinSig> {
    let mut keys: Vec<bls12381::PrivateKey> = (0..3)
        .map(|i| bls12381::PrivateKey::from_seed((i + 1) as u64))
        .collect();
    keys.sort_by_key(|a| a.public_key().encode());

    let participants: commonware_utils::ordered::Set<bls12381::PublicKey> =
        keys.iter().map(|k| k.public_key()).try_collect().unwrap();
    let dkg = bootstrap_dkg(3).unwrap();

    let schemes: Vec<HybridScheme<MinSig>> = keys
        .iter()
        .map(|key| {
            let pk = key.public_key();
            let idx = participants.index(&pk).unwrap();
            HybridScheme::signer(
                &config::outbe_app_namespace(),
                participants.clone(),
                key.clone(),
                dkg.polynomial.clone(),
                dkg.shares[idx.get() as usize].clone(),
            )
            .unwrap()
        })
        .collect();

    let proposal = commonware_consensus::simplex::types::Proposal::new(
        Round::new(Epoch::new(0), View::new(2)),
        View::new(1),
        commonware_cryptography::Sha256::hash(b"stack-test"),
    );
    let subject = Subject::Notarize {
        proposal: &proposal,
    };
    let attestations: Vec<_> = schemes
        .iter()
        .map(|scheme| scheme.sign::<Sha256Digest>(subject).unwrap())
        .collect();

    schemes[0]
        .assemble::<_, commonware_utils::N3f1>(attestations, &Sequential)
        .unwrap()
}

#[derive(Default)]
struct MockBlockHashProvider {
    hashes: BTreeMap<u64, B256>,
}

impl BlockHashReader for MockBlockHashProvider {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        Ok(self.hashes.get(&number).copied())
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        Ok((start..end)
            .filter_map(|height| self.hashes.get(&height).copied())
            .collect())
    }
}

#[test]
fn provider_matches_consensus_tip_checks_height_and_hash() {
    let digest = outbe_consensus::digest::Digest(B256::repeat_byte(0x11));
    let tip = crate::marshal_update_reporter::ConsensusTip {
        round: Round::new(Epoch::new(0), View::new(7)),
        height: Height::new(42),
        digest,
    };

    let mut provider = MockBlockHashProvider::default();
    provider.hashes.insert(42, digest.0);

    assert!(provider_matches_consensus_tip(&provider, tip, 41).unwrap());
    assert!(provider_matches_consensus_tip(&provider, tip, 42).unwrap());
    assert!(!provider_matches_consensus_tip(&provider, tip, 43).unwrap());

    provider.hashes.insert(42, B256::repeat_byte(0x22));
    assert!(!provider_matches_consensus_tip(&provider, tip, 42).unwrap());

    provider.hashes.clear();
    assert!(!provider_matches_consensus_tip(&provider, tip, 42).unwrap());
}

#[test]
fn execution_watchdog_decision_covers_core_states() {
    let started = SystemTime::UNIX_EPOCH;
    let after_startup = started
        + Duration::from_secs(config::EXECUTION_WATCHDOG_STARTUP_GRACE_SEC)
        + Duration::from_secs(1);
    let after_fatal_grace = after_startup + config::EXECUTION_WATCHDOG_GRACE;

    let (decision, next_unhealthy_since) = execution_watchdog_decision(
        ExecutionWatchdogObservation::ProviderState {
            consensus_tip_height: 100,
            reth_head_height: 100,
            hash_match: true,
        },
        after_startup,
        started,
        Some(started),
    );
    assert_eq!(decision, ExecutionWatchdogDecision::Healthy);
    assert_eq!(next_unhealthy_since, None);

    let (decision, next_unhealthy_since) = execution_watchdog_decision(
        ExecutionWatchdogObservation::ProviderState {
            consensus_tip_height: 100,
            reth_head_height: 0,
            hash_match: true,
        },
        after_startup,
        started,
        Some(started),
    );
    assert_eq!(decision, ExecutionWatchdogDecision::Healthy);
    assert_eq!(next_unhealthy_since, None);

    let (decision, next_unhealthy_since) = execution_watchdog_decision(
        ExecutionWatchdogObservation::ProviderState {
            consensus_tip_height: 100,
            reth_head_height: 0,
            hash_match: false,
        },
        started + Duration::from_secs(1),
        started,
        None,
    );
    assert_eq!(decision, ExecutionWatchdogDecision::StartupGrace);
    assert_eq!(next_unhealthy_since, None);

    let (decision, next_unhealthy_since) = execution_watchdog_decision(
        ExecutionWatchdogObservation::ProviderState {
            consensus_tip_height: config::EXECUTION_WATCHDOG_LAG_BLOCKS + 2,
            reth_head_height: 0,
            hash_match: false,
        },
        after_startup,
        started,
        None,
    );
    assert_eq!(
        decision,
        ExecutionWatchdogDecision::Unhealthy {
            unhealthy_for: Duration::ZERO,
        }
    );
    assert_eq!(next_unhealthy_since, Some(after_startup));

    let (decision, next_unhealthy_since) = execution_watchdog_decision(
        ExecutionWatchdogObservation::ProviderState {
            consensus_tip_height: 100,
            reth_head_height: 100,
            hash_match: false,
        },
        after_fatal_grace,
        started,
        Some(after_startup),
    );
    assert_eq!(
        decision,
        ExecutionWatchdogDecision::Fatal {
            unhealthy_for: config::EXECUTION_WATCHDOG_GRACE,
        }
    );
    assert_eq!(next_unhealthy_since, Some(after_startup));

    let (decision, next_unhealthy_since) = execution_watchdog_decision(
        ExecutionWatchdogObservation::ProviderReadError,
        after_fatal_grace,
        started,
        Some(after_startup),
    );
    assert_eq!(
        decision,
        ExecutionWatchdogDecision::Fatal {
            unhealthy_for: config::EXECUTION_WATCHDOG_GRACE,
        }
    );
    assert_eq!(next_unhealthy_since, Some(after_startup));
}

fn test_boundary_with_vrf_hash(vrf_group_public_key: B256, dkg_cycle: u64) -> DkgBoundaryArtifact {
    DkgBoundaryArtifact {
        epoch: dkg_cycle,
        dkg_cycle,
        freeze_height: 10,
        planned_activation_height: 20,
        target_set_hash: B256::with_last_byte(0xA1),
        vrf_material_version: dkg_cycle,
        vrf_group_public_key,
        vrf_group_public_key_bytes: Bytes::new(),
        committee_set_hash: B256::ZERO,
        is_validator_set_change: true,
        outcome: Bytes::new(),
        is_full_dkg: false,
        tee_recipient_pubkeys: Vec::new(),
        tee_reshare_registrations: Vec::new(),
        endorsement_signature: alloy_primitives::Bytes::new(),
        reshare: outbe_primitives::consensus::ReshareResult {
            new_active_set: Vec::new(),
            active_set_hash: B256::with_last_byte(0xA2),
        },
    }
}

#[test]
fn startup_dkg_round_zero_is_only_for_empty_genesis_formation() {
    let empty_without_boundary = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: true,
    };
    assert_eq!(
        startup_dkg_mode(empty_without_boundary, true, false),
        StartupDkgMode::InitialGenesisDkg
    );

    assert_eq!(
        startup_dkg_mode(empty_without_boundary, false, false),
        StartupDkgMode::LiveJoinRequired,
        "a local key outside the current set must not start genesis DKG"
    );

    let nonzero_execution_history = StartupDkgContext {
        last_execution_height: 7,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: true,
    };
    assert_eq!(
        startup_dkg_mode(nonzero_execution_history, true, false),
        StartupDkgMode::LiveJoinRequired,
        "non-zero execution history must not start genesis DKG"
    );

    let recovered_boundary = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: Some(B256::with_last_byte(9)),
        recovered_dkg_output_hash: Some(B256::with_last_byte(10)),
        genesis_formation_proven: true,
    };
    assert_eq!(
        startup_dkg_mode(recovered_boundary, true, false),
        StartupDkgMode::LiveJoinRequired,
        "a recovered chain DKG boundary must force live-join semantics"
    );
}

#[test]
fn startup_dkg_round_zero_requires_genesis_formation_proof() {
    let unproven = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: false,
    };
    assert_eq!(
        startup_dkg_mode(unproven, true, false),
        StartupDkgMode::LiveJoinRequired,
        "local execution height 0 alone must not start DKG round 0"
    );

    let consensus_already_finalized = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 3,
        recovered_boundary_finalized: true,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: true,
    };
    assert_eq!(
        startup_dkg_mode(consensus_already_finalized, true, false),
        StartupDkgMode::LiveJoinRequired,
        "marshal finalized height > 0 must block genesis DKG"
    );
}

#[test]
fn force_dkg_overrides_execution_height_check() {
    let existing_chain = StartupDkgContext {
        last_execution_height: 780596,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: Some(B256::with_last_byte(9)),
        recovered_dkg_output_hash: Some(B256::with_last_byte(10)),
        genesis_formation_proven: false,
    };
    assert_eq!(
        startup_dkg_mode(existing_chain, true, false),
        StartupDkgMode::LiveJoinRequired,
        "without force_dkg, existing chain data must use live-join"
    );
    assert_eq!(
        startup_dkg_mode(existing_chain, true, true),
        StartupDkgMode::InitialGenesisDkg,
        "force_dkg must override all checks and force initial DKG"
    );
    assert_eq!(
        startup_dkg_mode(existing_chain, false, true),
        StartupDkgMode::LiveJoinRequired,
        "force_dkg must not override local-key-not-in-set check"
    );
}

#[test]
fn force_dkg_recovery_boundary_targets_next_epoch_at_head_plus_one() {
    let (keys, _participants, output, _share, _polynomial) = run_test_dkg_complete();
    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(1),
            Address::with_last_byte(2),
            Address::with_last_byte(3),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let mut previous = test_boundary_with_vrf_hash(B256::with_last_byte(0x55), 12);
    previous.epoch = 7;
    previous.vrf_material_version = 4;

    let (activation_height, boundary) =
        build_force_dkg_recovery_boundary(&validator_set, &output, &previous, 780_596).unwrap();

    assert_eq!(activation_height, 780_597);
    assert_eq!(boundary.epoch, 8);
    assert_eq!(boundary.dkg_cycle, 13);
    assert_eq!(boundary.freeze_height, 780_596);
    assert_eq!(boundary.planned_activation_height, 780_597);
    assert_eq!(boundary.vrf_material_version, 5);
    assert!(boundary.is_full_dkg);
    assert!(!boundary.is_validator_set_change);
    assert_eq!(decode_boundary_output(&boundary).unwrap(), output);
}

#[test]
fn force_dkg_recovery_boundary_rejects_empty_chain() {
    let (keys, _participants, output, _share, _polynomial) = run_test_dkg_complete();
    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(1),
            Address::with_last_byte(2),
            Address::with_last_byte(3),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let previous = test_boundary_with_vrf_hash(B256::with_last_byte(0x55), 12);

    let error = build_force_dkg_recovery_boundary(&validator_set, &output, &previous, 0)
        .unwrap_err()
        .to_string();
    assert!(error.contains("only valid for an existing chain"));
}

#[test]
fn genesis_formation_gate_waits_without_expected_peers() {
    let genesis = B256::with_last_byte(1);
    let context = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: false,
    };
    let evidence = RethGenesisPeerEvidence {
        connected_peers: 1,
        is_syncing: false,
        is_initially_syncing: false,
        peer_query_failed: false,
        peers: vec![RethGenesisPeerStatus {
            genesis,
            blockhash: genesis,
            latest_block: Some(0),
        }],
    };
    assert_eq!(
        genesis_formation_gate_decision(context, genesis, 3, &evidence),
        GenesisFormationGate::WaitForExecutionSync
    );
}

#[test]
fn genesis_formation_gate_proves_peers_are_at_genesis() {
    let genesis = B256::with_last_byte(1);
    let context = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: false,
    };
    let evidence = RethGenesisPeerEvidence {
        connected_peers: 2,
        is_syncing: true,
        is_initially_syncing: true,
        peer_query_failed: false,
        peers: vec![
            RethGenesisPeerStatus {
                genesis,
                blockhash: genesis,
                latest_block: Some(0),
            },
            RethGenesisPeerStatus {
                genesis,
                blockhash: genesis,
                latest_block: None,
            },
        ],
    };
    assert_eq!(
        genesis_formation_gate_decision(context, genesis, 2, &evidence),
        GenesisFormationGate::Proven
    );
}

#[test]
fn genesis_formation_gate_accepts_quorum_connected_non_mesh_topology() {
    let genesis = B256::with_last_byte(1);
    let context = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: false,
    };
    let peer = RethGenesisPeerStatus {
        genesis,
        blockhash: genesis,
        latest_block: Some(0),
    };
    let evidence = RethGenesisPeerEvidence {
        connected_peers: 2,
        is_syncing: true,
        is_initially_syncing: true,
        peer_query_failed: false,
        peers: vec![peer; 2],
    };

    // Four validators need a 3-of-4 BFT quorum, hence two matching remote
    // witnesses per node. Requiring all three remote validators creates a split
    // startup gate on a healthy non-fully-meshed gossip topology: nodes seeing
    // 3/3 start all-member DKG while nodes seeing 2/3 never enter it.
    assert_eq!(
        genesis_formation_gate_decision(
            context,
            genesis,
            genesis_formation_required_remote_peers(4),
            &evidence,
        ),
        GenesisFormationGate::Proven
    );
}

#[test]
fn genesis_formation_gate_rejects_remote_chain_progress() {
    let genesis = B256::with_last_byte(1);
    let context = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: false,
    };
    let evidence = RethGenesisPeerEvidence {
        connected_peers: 1,
        is_syncing: false,
        is_initially_syncing: false,
        peer_query_failed: false,
        peers: vec![RethGenesisPeerStatus {
            genesis,
            blockhash: B256::with_last_byte(2),
            latest_block: Some(11),
        }],
    };
    assert_eq!(
        genesis_formation_gate_decision(context, genesis, 1, &evidence),
        GenesisFormationGate::ExistingChainJoin
    );
}

#[test]
fn genesis_formation_gate_waits_while_reth_syncing_without_peer_quorum() {
    let genesis = B256::with_last_byte(1);
    let context = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: false,
    };
    let evidence = RethGenesisPeerEvidence {
        connected_peers: 1,
        is_syncing: true,
        is_initially_syncing: false,
        peer_query_failed: false,
        peers: vec![RethGenesisPeerStatus {
            genesis,
            blockhash: genesis,
            latest_block: Some(0),
        }],
    };
    assert_eq!(
        genesis_formation_gate_decision(context, genesis, 2, &evidence),
        GenesisFormationGate::WaitForExecutionSync
    );
}

#[test]
fn recovered_boundary_rejects_stale_threshold_material() {
    let (_, _, _output, _share, polynomial) = run_test_dkg_complete();
    let matching_hash = vrf_group_public_key_hash(&polynomial);

    assert!(vrf_material_matches_recovered_boundary(
        &polynomial,
        StartupDkgContext {
            last_execution_height: 100,
            last_consensus_finalized_height: 100,
            recovered_boundary_finalized: true,
            recovered_vrf_group_public_key: Some(matching_hash),
            recovered_dkg_output_hash: None,
            genesis_formation_proven: false,
        }
    ));
    assert!(vrf_material_matches_recovered_boundary(
        &polynomial,
        StartupDkgContext {
            last_execution_height: 0,
            last_consensus_finalized_height: 0,
            recovered_boundary_finalized: false,
            recovered_vrf_group_public_key: None,
            recovered_dkg_output_hash: None,
            genesis_formation_proven: true,
        }
    ));
    assert!(
        !vrf_material_matches_recovered_boundary(
            &polynomial,
            StartupDkgContext {
                last_execution_height: 100,
                last_consensus_finalized_height: 100,
                recovered_boundary_finalized: true,
                recovered_vrf_group_public_key: Some(B256::ZERO),
                recovered_dkg_output_hash: None,
                genesis_formation_proven: false,
            }
        ),
        "saved or CLI material from an older DKG boundary must not build a signer"
    );
}

#[test]
fn startup_live_join_uses_next_cycle_after_recovered_boundary() {
    let boundary = test_boundary_with_vrf_hash(B256::with_last_byte(0x55), 244);
    assert_eq!(next_live_reshare_round(&boundary), 245);
}

#[derive(Clone, Default)]
struct EmptyMarshalBuffer {
    pending_digest_subscribers: Arc<StdMutex<Vec<oneshot::Sender<ConsensusBlock>>>>,
    pending_commitment_subscribers: Arc<StdMutex<Vec<oneshot::Sender<ConsensusBlock>>>>,
}

impl Buffer<outbe_consensus::marshal_types::Variant> for EmptyMarshalBuffer {
    // commonware 2026.5.0 dropped `type CachedBlock` (the block type is now
    // `V::Block`) and added `type PublicKey`.
    type PublicKey = commonware_cryptography::bls12381::PublicKey;

    async fn find_by_digest(
        &self,
        _digest: outbe_consensus::digest::Digest,
    ) -> Option<ConsensusBlock> {
        None
    }

    async fn find_by_commitment(
        &self,
        _commitment: outbe_consensus::digest::Digest,
    ) -> Option<ConsensusBlock> {
        None
    }

    // `subscribe_by_*` are now SYNC and return `Option<oneshot::Receiver<..>>`.
    // We retain the pending sender (so the receiver never resolves) and hand
    // back `Some(rx)`, preserving the "block is never available" semantics this
    // empty buffer represents.
    fn subscribe_by_digest(
        &self,
        _digest: outbe_consensus::digest::Digest,
    ) -> Option<oneshot::Receiver<ConsensusBlock>> {
        let (tx, rx) = oneshot::channel();
        self.pending_digest_subscribers.lock().unwrap().push(tx);
        Some(rx)
    }

    fn subscribe_by_commitment(
        &self,
        _commitment: outbe_consensus::digest::Digest,
    ) -> Option<oneshot::Receiver<ConsensusBlock>> {
        let (tx, rx) = oneshot::channel();
        self.pending_commitment_subscribers.lock().unwrap().push(tx);
        Some(rx)
    }

    // `finalized` is now SYNC; `proposed` was removed and replaced by `send`.
    fn finalized(&self, _commitment: outbe_consensus::digest::Digest) {}

    fn send(
        &self,
        _round: Round,
        _block: ConsensusBlock,
        _recipients: Recipients<Self::PublicKey>,
    ) {
    }
}

#[derive(Clone, Default)]
struct AckingMarshalReporter;

impl Reporter for AckingMarshalReporter {
    type Activity = Update<ConsensusBlock, commonware_utils::acknowledgement::Exact>;

    // `report` is now SYNC and returns `Feedback` (commonware 2026.5.0). The
    // body is unchanged work (acknowledge delivered blocks); we always return
    // `Feedback::Ok` because this test reporter has no downstream mailbox that
    // can close.
    fn report(&mut self, activity: Self::Activity) -> Feedback {
        if let Update::Block(_, ack) = activity {
            ack.acknowledge();
        }
        Feedback::Ok
    }
}

#[derive(Clone, Default)]
struct NoopMarshalResolver;

// commonware 2026.5.0 split the resolver surface: the base `Resolver` keeps
// `fetch`/`fetch_all`/`retain` (now SYNC, returning `Feedback`, generic over
// `Into<Fetch<Key, Subscriber>>`) and gained `type Subscriber`; `cancel`/`clear`
// were removed; the targeted methods moved to `TargetedResolver`. The marshal
// actor requires `Key = handler::Key<Commitment>` and `Subscriber =
// handler::Annotation`.
impl Resolver for NoopMarshalResolver {
    type Key = handler::Key<outbe_consensus::digest::Digest>;
    type Subscriber = handler::Annotation;

    fn fetch<F>(&mut self, _key: F) -> Feedback
    where
        F: Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        Feedback::Ok
    }

    fn fetch_all<F>(&mut self, _keys: Vec<F>) -> Feedback
    where
        F: Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        Feedback::Ok
    }

    fn retain(
        &mut self,
        _predicate: impl Fn(&Self::Key, &Self::Subscriber) -> bool + Send + 'static,
    ) -> Feedback {
        Feedback::Ok
    }
}

impl TargetedResolver for NoopMarshalResolver {
    type PublicKey = bls12381::PublicKey;

    fn fetch_targeted(
        &mut self,
        _fetch: impl Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
        _targets: NonEmptyVec<Self::PublicKey>,
    ) -> Feedback {
        Feedback::Ok
    }

    fn fetch_all_targeted<F>(&mut self, _keys: Vec<(F, NonEmptyVec<Self::PublicKey>)>) -> Feedback
    where
        F: Into<commonware_resolver::Fetch<Self::Key, Self::Subscriber>> + Send,
    {
        Feedback::Ok
    }
}

async fn start_recovery_marshal(
    context: commonware_runtime::tokio::Context,
    provider: HybridSchemeProvider<MinSig>,
) -> (
    outbe_consensus::marshal_types::MarshalMailbox,
    handler::Handler<outbe_consensus::digest::Digest>,
    commonware_runtime::Handle<()>,
) {
    let page_cache = CacheRef::from_pooler(
        &context,
        NonZeroU16::new(1024).unwrap(),
        NonZeroUsize::new(10).unwrap(),
    );
    let test_id = STACK_MARSHAL_TEST_ID.fetch_add(1, Ordering::SeqCst);
    let partition_prefix = format!("stack-finalized-round-recovery-{test_id}");
    let items_per_section = NonZeroU64::new(10).unwrap();
    let replay_buffer = NonZeroUsize::new(1024).unwrap();
    let write_buffer = NonZeroUsize::new(1024).unwrap();

    let finalizations_archive = immutable::Archive::init(
        context.child("recovery_finalizations"),
        immutable::Config {
            metadata_partition: format!("{partition_prefix}-finalizations-metadata"),
            freezer_table_partition: format!("{partition_prefix}-finalizations-freezer-table"),
            freezer_table_initial_size: config::FREEZER_TABLE_INITIAL_SIZE,
            freezer_table_resize_frequency: config::FREEZER_TABLE_RESIZE_FREQUENCY,
            freezer_table_resize_chunk_size: config::FREEZER_TABLE_RESIZE_CHUNK_SIZE,
            freezer_key_partition: format!("{partition_prefix}-finalizations-freezer-key"),
            freezer_key_page_cache: page_cache.clone(),
            freezer_value_partition: format!("{partition_prefix}-finalizations-freezer-value"),
            freezer_value_target_size: 1024,
            freezer_value_compression: None,
            ordinal_partition: format!("{partition_prefix}-finalizations-ordinal"),
            items_per_section,
            codec_config: HybridScheme::<MinSig>::certificate_codec_config_unbounded(),
            replay_buffer,
            freezer_key_write_buffer: write_buffer,
            freezer_value_write_buffer: write_buffer,
            ordinal_write_buffer: write_buffer,
        },
    )
    .await
    .unwrap();

    let blocks_archive = immutable::Archive::init(
        context.child("recovery_blocks"),
        immutable::Config {
            metadata_partition: format!("{partition_prefix}-blocks-metadata"),
            freezer_table_partition: format!("{partition_prefix}-blocks-freezer-table"),
            freezer_table_initial_size: config::FREEZER_TABLE_INITIAL_SIZE,
            freezer_table_resize_frequency: config::FREEZER_TABLE_RESIZE_FREQUENCY,
            freezer_table_resize_chunk_size: config::FREEZER_TABLE_RESIZE_CHUNK_SIZE,
            freezer_key_partition: format!("{partition_prefix}-blocks-freezer-key"),
            freezer_key_page_cache: page_cache.clone(),
            freezer_value_partition: format!("{partition_prefix}-blocks-freezer-value"),
            freezer_value_target_size: 1024,
            freezer_value_compression: None,
            ordinal_partition: format!("{partition_prefix}-blocks-ordinal"),
            items_per_section,
            codec_config: (),
            replay_buffer,
            freezer_key_write_buffer: write_buffer,
            freezer_value_write_buffer: write_buffer,
            ordinal_write_buffer: write_buffer,
        },
    )
    .await
    .unwrap();

    let (actor, mailbox, _) = marshal::core::Actor::init(
        context.child("recovery_marshal"),
        finalizations_archive,
        blocks_archive,
        marshal::Config {
            provider,
            epocher: FixedEpocher::new(NonZeroU64::new(10_000).unwrap()),
            // 2026.5.0: the floor/genesis anchor is now an explicit `Start`.
            // A fresh epoch starts from the height-0 genesis block (the actor
            // asserts the anchor height is zero).
            start: Start::Genesis(recovery_block(0)),
            partition_prefix,
            // `mailbox_size` is now `NonZeroUsize`.
            mailbox_size: NonZeroUsize::new(32).unwrap(),
            view_retention_timeout: ViewDelta::new(10_000),
            prunable_items_per_section: items_per_section,
            page_cache,
            replay_buffer,
            key_write_buffer: write_buffer,
            value_write_buffer: write_buffer,
            block_codec_config: (),
            max_repair: NonZeroUsize::new(16).unwrap(),
            max_pending_acks: NonZeroUsize::new(16).unwrap(),
            strategy: Sequential,
        },
    )
    .await;

    // 2026.5.0: the resolver handoff changed — the marshal actor takes
    // `(handler::Receiver<Commitment>, R)` where `R: TargetedResolver`. The
    // receiver/handler pair is produced by `handler::init`; the `Handler` is
    // returned as the keepalive (dropping it closes the receiver and shuts the
    // actor's run loop down). The old `mpsc::Sender<handler::Message>` type is
    // now private and cannot be named or constructed by tests.
    let (resolver_rx, resolver_handler) = handler::init::<outbe_consensus::digest::Digest>(
        context.child("resolver_handler"),
        NonZeroUsize::new(16).unwrap(),
    );
    let handle = actor.start(
        AckingMarshalReporter,
        EmptyMarshalBuffer::default(),
        (resolver_rx, NoopMarshalResolver),
    );
    (mailbox, resolver_handler, handle)
}

fn recovery_block(number: u64) -> ConsensusBlock {
    let mut block = Block::default();
    block.header.number = number;
    block.header.extra_data = Bytes::from(vec![number as u8]);
    let block = block.map_header(OutbeHeader::new);
    ConsensusBlock::from_sealed(SealedBlock::seal_slow(block))
}

fn recovery_finalization_fixture(
    block: &ConsensusBlock,
    round: Round,
) -> (
    HybridSchemeProvider<MinSig>,
    Finalization<HybridScheme<MinSig>, outbe_consensus::digest::Digest>,
) {
    let keys: Vec<bls12381::PrivateKey> = (1u64..=3).map(bls12381::PrivateKey::from_seed).collect();
    let participants: commonware_utils::ordered::Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let dkg = bootstrap_dkg(3).unwrap();
    let signers: Vec<HybridScheme<MinSig>> = keys
        .iter()
        .map(|key| {
            let pk = key.public_key();
            let idx = participants.index(&pk).unwrap();
            HybridScheme::signer(
                &config::outbe_app_namespace(),
                participants.clone(),
                key.clone(),
                dkg.polynomial.clone(),
                dkg.shares[idx.get() as usize].clone(),
            )
            .unwrap()
        })
        .collect();
    let verifier = HybridScheme::<MinSig>::verifier(
        &config::outbe_app_namespace(),
        participants,
        dkg.polynomial.clone(),
    )
    .unwrap();

    let proposal = Proposal::new(
        round,
        round.view().previous().unwrap_or(View::zero()),
        block.digest(),
    );
    let finalizes: Vec<_> = signers
        .iter()
        .map(|scheme| Finalize::sign(scheme, proposal.clone()).unwrap())
        .collect();
    let finalization = Finalization::from_finalizes(&verifier, &finalizes, &Sequential).unwrap();
    let provider = HybridSchemeProvider::new();
    let _ = provider.register(round.epoch(), verifier);
    (provider, finalization)
}

#[test]
fn recover_application_finalized_round_returns_none_at_genesis_height() {
    let recovered = commonware_runtime::tokio::Runner::default().start(|context| async move {
        let clock = context.child("recover_clock");
        let (marshal_mailbox, resolver_keepalive, actor_handle) =
            start_recovery_marshal(context, HybridSchemeProvider::new()).await;

        let recovered = recover_application_finalized_round(&clock, &marshal_mailbox, 0)
            .await
            .unwrap();

        drop(resolver_keepalive);
        actor_handle.abort();
        let _ = actor_handle.await;
        recovered
    });

    assert_eq!(recovered, None);
}

#[test]
fn recover_application_finalized_round_reads_round_from_marshal_archive() {
    let recovered = commonware_runtime::tokio::Runner::default().start(|context| async move {
        let round = Round::new(Epoch::new(0), View::new(1175));
        let block = recovery_block(5700);
        let expected_digest = block.digest();
        let (provider, finalization) = recovery_finalization_fixture(&block, round);
        let clock = context.child("recover_clock");
        let (mut marshal_mailbox, resolver_keepalive, actor_handle) =
            start_recovery_marshal(context, provider).await;

        let _ = marshal_mailbox.verified(round, block).await;
        // 2026.5.0: `Reporter::report` is SYNC and returns `Feedback`.
        let _ = marshal_mailbox.report(Activity::Finalization(finalization));

        let recovered = recover_application_finalized_round(&clock, &marshal_mailbox, 5700)
            .await
            .unwrap();

        drop(resolver_keepalive);
        actor_handle.abort();
        let _ = actor_handle.await;
        (recovered, expected_digest)
    });

    assert_eq!(
        recovered.0,
        Some(RecoveredApplicationFinalization {
            round: Round::new(Epoch::new(0), View::new(1175)),
            digest: recovered.1,
        })
    );
}

#[test]
fn exact_marshal_finalization_promotes_recovery_anchor_to_execution_head() {
    let hash = B256::repeat_byte(0x42);
    let round = Round::new(Epoch::new(3), View::new(17));
    let reconciled = reconcile_recovered_execution_head(
        91,
        hash,
        Some(RecoveredApplicationFinalization {
            round,
            digest: Digest(hash),
        }),
    )
    .unwrap();

    assert_eq!(reconciled, (91, hash, Some(round)));
}

#[test]
fn mismatched_marshal_finalization_digest_fails_closed() {
    let error = reconcile_recovered_execution_head(
        91,
        B256::repeat_byte(0x42),
        Some(RecoveredApplicationFinalization {
            round: Round::new(Epoch::new(3), View::new(17)),
            digest: Digest(B256::repeat_byte(0x24)),
        }),
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("marshal finalization digest mismatch at execution height 91"));
    assert!(error.contains("execution=0x4242"));
    assert!(error.contains("marshal=0x2424"));
}

#[test]
fn recover_application_finalized_round_fails_when_archive_is_missing_height() {
    let error = commonware_runtime::tokio::Runner::default().start(|context| async move {
        let clock = context.child("recover_clock");
        let (marshal_mailbox, resolver_keepalive, actor_handle) =
            start_recovery_marshal(context, HybridSchemeProvider::new()).await;

        let error = recover_application_finalized_round(&clock, &marshal_mailbox, 5700)
            .await
            .unwrap_err()
            .to_string();

        drop(resolver_keepalive);
        actor_handle.abort();
        let _ = actor_handle.await;
        error
    });

    assert!(error.contains("marshal finalization missing for finalized execution height 5700"));
    assert!(error.contains("resync/rebuild consensus storage"));
}

#[test]
fn test_build_boundary_artifact_maps_addresses() {
    let (keys, _participants, output, _polynomial) = run_test_dkg();

    let addresses = vec![
        Address::with_last_byte(0x11),
        Address::with_last_byte(0x22),
        Address::with_last_byte(0x33),
    ];

    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|k| k.public_key()).collect(),
        addresses: addresses.clone(),
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };

    let result = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(1),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 1,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 1,
        is_validator_set_change: true,
        tee_reshare_registrations: Vec::new(),
    })
    .unwrap();

    // All 3 addresses should be in the result.
    assert_eq!(result.reshare.new_active_set.len(), 3);
    assert!(result.reshare.new_active_set.contains(&addresses[0]));
    assert!(result.reshare.new_active_set.contains(&addresses[1]));
    assert!(result.reshare.new_active_set.contains(&addresses[2]));

    // Group public key should be a non-zero hash.
    assert_ne!(result.vrf_group_public_key, B256::ZERO);
    assert_ne!(result.reshare.active_set_hash, B256::ZERO);
}

#[test]
fn test_build_boundary_artifact_deterministic() {
    let (_keys, _participants, output, _polynomial) = run_test_dkg();

    let validator_set = validators::ValidatorSet {
        public_keys: _keys.iter().map(|k| k.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0xAA),
            Address::with_last_byte(0xBB),
            Address::with_last_byte(0xCC),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };

    // Same inputs → same output.
    let r1 = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(1),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: true,
        dkg_cycle: 1,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 1,
        is_validator_set_change: true,
        tee_reshare_registrations: Vec::new(),
    })
    .unwrap();
    let r2 = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(1),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: true,
        dkg_cycle: 1,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 1,
        is_validator_set_change: true,
        tee_reshare_registrations: Vec::new(),
    })
    .unwrap();
    assert_eq!(r1.vrf_group_public_key, r2.vrf_group_public_key);
    assert_eq!(r1.reshare.active_set_hash, r2.reshare.active_set_hash);
    assert_eq!(r1.reshare.new_active_set, r2.reshare.new_active_set);
    assert_eq!(r1.outcome, r2.outcome);
}

#[test]
fn test_build_boundary_artifact_allows_extra_validator_not_in_threshold_output() {
    let (keys, _participants, output, _polynomial) = run_test_dkg();
    let mut all_pks: Vec<_> = keys.iter().map(|k| k.public_key()).collect();
    let extra_key = bls12381::PrivateKey::random(rand_core::OsRng);
    all_pks.push(extra_key.public_key());

    let refreshed_set = validators::ValidatorSet {
        public_keys: all_pks,
        addresses: vec![
            Address::with_last_byte(0x11),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
            Address::with_last_byte(0x44),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 4],
    };

    let result = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(1),
        validator_set: &refreshed_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 1,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 1,
        is_validator_set_change: true,
        tee_reshare_registrations: Vec::new(),
    })
    .unwrap();
    assert_eq!(result.reshare.new_active_set.len(), 3);
}

#[test]
fn test_build_boundary_artifact_rejects_removed_validator_in_output() {
    let (keys, _participants, output, _polynomial) = run_test_dkg();
    let partial_set = validators::ValidatorSet {
        public_keys: keys.iter().take(2).map(|k| k.public_key()).collect(),
        addresses: vec![Address::with_last_byte(0x11), Address::with_last_byte(0x22)],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 2],
    };

    let error = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(1),
        validator_set: &partial_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 1,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 1,
        is_validator_set_change: true,
        tee_reshare_registrations: Vec::new(),
    })
    .unwrap_err()
    .to_string();
    assert!(error.contains("absent from the validator set"));
}

#[test]
fn test_decode_boundary_output_round_trips_full_output() {
    let (keys, _participants, output, _share, _polynomial) = run_test_dkg_complete();

    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x11),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };

    let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(1),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 1,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 1,
        is_validator_set_change: true,
        tee_reshare_registrations: Vec::new(),
    })
    .unwrap();

    let decoded = decode_boundary_output(&artifact).unwrap();
    assert_eq!(decoded, output);
}

#[test]
fn test_decode_boundary_output_rejects_corrupted_outcome() {
    let (keys, _participants, output, _share, _polynomial) = run_test_dkg_complete();

    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x11),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };

    let mut artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(1),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 1,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 1,
        is_validator_set_change: true,
        tee_reshare_registrations: Vec::new(),
    })
    .unwrap();

    let mut corrupted = artifact.outcome.to_vec();
    corrupted[0] = b'X';
    artifact.outcome = Bytes::from(corrupted);

    let error = decode_boundary_output(&artifact).unwrap_err().to_string();
    assert!(error.contains("invalid magic"));
}

#[test]
fn test_pending_dkg_boundary_snapshot_round_trips_and_rejects_corruption() {
    let (keys, _participants, output, _share, _polynomial) = run_test_dkg_complete();
    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x11),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(7),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 6,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 2,
        is_validator_set_change: true,
        tee_reshare_registrations: Vec::new(),
    })
    .unwrap();
    let snapshot = PendingDkgBoundarySnapshot {
        artifact,
        activated_at_height: 20,
    };

    let encoded = encode_pending_dkg_boundary_snapshot(&snapshot).unwrap();
    let decoded = decode_pending_dkg_boundary_snapshot(&encoded).unwrap();
    assert_eq!(decoded, snapshot);

    let mut corrupted = encoded;
    corrupted[0] = b'X';
    let error = decode_pending_dkg_boundary_snapshot(&corrupted)
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid pending DKG boundary snapshot magic"));
}

#[test]
fn test_save_load_and_clear_pending_dkg_boundary_snapshot() {
    let boundary = test_boundary_with_vrf_hash(B256::with_last_byte(0x55), 9);
    let snapshot = PendingDkgBoundarySnapshot {
        artifact: boundary,
        activated_at_height: 42,
    };
    let dir = tempfile::tempdir().unwrap();

    assert!(load_pending_dkg_boundary(dir.path()).unwrap().is_none());
    save_pending_dkg_boundary(dir.path(), &snapshot).unwrap();
    assert_eq!(
        load_pending_dkg_boundary(dir.path()).unwrap(),
        Some(snapshot)
    );
    clear_pending_dkg_boundary(dir.path());
    assert!(load_pending_dkg_boundary(dir.path()).unwrap().is_none());
}

#[test]
fn test_completed_dkg_is_durable_before_activation_boundary() {
    let (keys, participants, output, share, _polynomial) = run_test_dkg_complete();
    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x11),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let target = FrozenDkgTarget {
        dkg_cycle: 4,
        freeze_height: 90,
        planned_activation_height: 120,
        validator_set,
        participants: participants.clone(),
        is_validator_set_change: false,
    };
    let complete = dkg_actor::DkgComplete {
        output: output.clone(),
        share: Some(share),
        participants: participants.clone(),
    };
    let dir = tempfile::tempdir().unwrap();
    let backend = bls::KeyBackend::Plaintext;

    persist_completed_dkg_before_activation(
        dir.path(),
        &backend,
        Epoch::new(3),
        3,
        &participants,
        &target,
        &complete,
        104,
    )
    .unwrap();

    let (_, _, recovered_output) = load_pending_dkg_state(dir.path(), &backend)
        .unwrap()
        .expect("completed DKG material must survive a pre-activation crash");
    assert_eq!(recovered_output, output);
    let snapshot = load_pending_dkg_boundary(dir.path())
        .unwrap()
        .expect("completed DKG boundary must survive a pre-activation crash");
    assert_eq!(snapshot.activated_at_height, 120);
    assert_eq!(snapshot.artifact.epoch, 4);
    assert_eq!(snapshot.artifact.dkg_cycle, 4);
}

#[test]
fn test_pending_dkg_material_alone_does_not_restore_boundary() {
    let (_keys, _participants, output, share, polynomial) = run_test_dkg_complete();
    let dir = tempfile::tempdir().unwrap();
    let backend = bls::KeyBackend::Plaintext;

    // Crash cut point: pending DKG triplet reached disk, but the boundary
    // snapshot did not. Restart must not infer/activate a boundary from material
    // alone; the pending-boundary file remains absent and DkgManager has no
    // pending artifact to verify/drain.
    save_pending_dkg_state(dir.path(), &share, &polynomial, &output, &backend).unwrap();
    assert!(load_pending_dkg_state(dir.path(), &backend)
        .unwrap()
        .is_some());
    assert!(load_pending_dkg_boundary(dir.path()).unwrap().is_none());

    let manager = DkgManagerMailbox::new();
    assert!(commonware_runtime::tokio::Runner::default()
        .start(|_| async move { manager.pending_boundary_artifact(Epoch::new(7)).await })
        .is_none());
}

#[test]
fn test_pending_boundary_snapshot_restores_manager_before_commit() {
    let (keys, _participants, output, share, polynomial) = run_test_dkg_complete();
    let dir = tempfile::tempdir().unwrap();
    let backend = bls::KeyBackend::Plaintext;
    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x11),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(7),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 6,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 2,
        is_validator_set_change: true,
        tee_reshare_registrations: Vec::new(),
    })
    .unwrap();
    let snapshot = PendingDkgBoundarySnapshot {
        artifact: artifact.clone(),
        activated_at_height: 20,
    };

    // Crash cut point: pending material + pending boundary snapshot exist, but
    // process memory was lost before/around note_ceremony_completed. Restart can
    // load both durable pieces and restore the boundary into DkgManager without
    // creating a committed marker.
    save_pending_dkg_state(dir.path(), &share, &polynomial, &output, &backend).unwrap();
    save_pending_dkg_boundary(dir.path(), &snapshot).unwrap();
    let loaded_state = load_pending_dkg_state(dir.path(), &backend)
        .unwrap()
        .expect("pending DKG state must survive restart");
    assert_eq!(loaded_state.2, output);
    let loaded_snapshot = load_pending_dkg_boundary(dir.path())
        .unwrap()
        .expect("pending boundary snapshot must survive restart");
    assert_eq!(loaded_snapshot, snapshot);

    let manager = DkgManagerMailbox::new();
    manager.note_recovered_pending_boundary(loaded_snapshot.artifact.clone());
    commonware_runtime::tokio::Runner::default().start(|_| async move {
        assert_eq!(
            manager.pending_boundary_artifact(Epoch::new(7)).await,
            Some(artifact.clone())
        );
        manager
            .verify_pending_boundary_artifact(Epoch::new(7), &artifact)
            .await
            .unwrap();
        assert_eq!(manager.take_committed_boundary_artifact().await, None);
    });
}

#[test]
fn test_pending_boundary_commit_requires_matching_finalized_artifact_then_clears() {
    let (keys, _participants, output, _share, _polynomial) = run_test_dkg_complete();
    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x11),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(7),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 6,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 2,
        is_validator_set_change: true,
        tee_reshare_registrations: Vec::new(),
    })
    .unwrap();
    let mut different = artifact.clone();
    different.dkg_cycle = different.dkg_cycle.saturating_add(1);

    let manager = DkgManagerMailbox::new();
    manager.note_recovered_pending_boundary(artifact.clone());
    commonware_runtime::tokio::Runner::default().start(|_| async move {
        // Crash cut point: pending boundary exists before finalization. A different
        // finalized BoundaryOutcome must not drain/activate the pending artifact.
        manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::BoundaryOutcome(
            different,
        )));
        assert_eq!(manager.take_committed_boundary_artifact().await, None);
        assert_eq!(
            manager.pending_boundary_artifact(Epoch::new(7)).await,
            Some(artifact.clone())
        );

        // Once the matching boundary finalizes, activation drain returns it once
        // and clears pending state.
        manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::BoundaryOutcome(
            artifact.clone(),
        )));
        assert_eq!(
            manager.take_committed_boundary_artifact().await,
            Some(artifact.clone())
        );
        assert_eq!(manager.take_committed_boundary_artifact().await, None);
        assert!(manager
            .pending_boundary_artifact(Epoch::new(7))
            .await
            .is_none());
    });
}

#[test]
fn test_stale_pending_boundary_snapshot_predicate_covers_restart_cleanup() {
    let current = test_boundary_with_vrf_hash(B256::with_last_byte(0x55), 9);
    let snapshot = PendingDkgBoundarySnapshot {
        artifact: current.clone(),
        activated_at_height: 42,
    };
    assert!(!pending_boundary_is_finalized(&snapshot, None));
    assert!(pending_boundary_is_finalized(
        &snapshot,
        Some(&(41, current.clone()))
    ));
    assert!(pending_boundary_is_finalized(
        &snapshot,
        Some(&(42, current.clone()))
    ));

    let mut newer_cycle = current.clone();
    newer_cycle.dkg_cycle = current.dkg_cycle.saturating_add(1);
    assert!(pending_boundary_is_finalized(
        &snapshot,
        Some(&(42, newer_cycle.clone()))
    ));
    assert!(pending_boundary_is_finalized(
        &snapshot,
        Some(&(142, newer_cycle))
    ));

    let mut older_cycle = current;
    older_cycle.dkg_cycle = older_cycle.dkg_cycle.saturating_sub(1);
    assert!(!pending_boundary_is_finalized(
        &snapshot,
        Some(&(42, older_cycle))
    ));
}

#[test]
fn test_startup_live_join_scan_height_never_uses_unfinalized_execution_head() {
    assert_eq!(startup_live_join_scan_height(10, 7, false).unwrap(), 7);
    assert_eq!(startup_live_join_scan_height(5, 7, false).unwrap(), 5);
    assert_eq!(startup_live_join_scan_height(0, 0, false).unwrap(), 0);
    let error = startup_live_join_scan_height(5, 0, false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("refusing to recover DKG artifacts from unfinalized execution head"));
    assert_eq!(startup_live_join_scan_height(5, 0, true).unwrap(), 0);
}

#[test]
fn test_verifier_boundary_adoption_rejects_prior_cycle_boundary() {
    // The trigger fires at exactly the activation height, where the newest
    // committed BoundaryOutcome is still the PREVIOUS cycle's (the activated
    // cycle's artifact rides the first new-epoch block, strictly above the
    // activation height). Adopting it would keep the follower one rotation
    // stale -> stale reshare prev_output/round -> ACTIVE-but-voteless after a
    // later stake. Regression for the wrong-boundary adoption bug.
    let activation_height = 120;
    // Prior cycle's boundary: committed one epoch earlier, planned for the
    // previous activation. Must not be adopted.
    assert!(!verifier_should_adopt_followed_boundary(
        1,
        0,
        activation_height
    ));
    assert!(!verifier_should_adopt_followed_boundary(
        91,
        activation_height,
        activation_height
    ));
    // A boundary committed AT the activation height is still not the
    // activated cycle's (the old epoch is fenced at the boundary).
    assert!(!verifier_should_adopt_followed_boundary(
        activation_height,
        activation_height,
        activation_height
    ));
    // The activated cycle's boundary: first new-epoch block, planned for this
    // activation. Adopt.
    assert!(verifier_should_adopt_followed_boundary(
        121,
        activation_height,
        activation_height
    ));
}

#[test]
fn test_verifier_boundary_adoption_is_monotone_for_lagging_followers() {
    // A follower processing rotation R_k's activation while the chain has
    // already committed R_{k+1}'s boundary adopts the newest one (planned
    // above this activation): adoption is monotone, and the follow of
    // R_{k+1} re-adopts idempotently.
    let activation_height = 120;
    assert!(verifier_should_adopt_followed_boundary(
        241,
        240,
        activation_height
    ));
    // But a boundary planned BELOW this activation is a stale cycle's
    // artifact regardless of commit height.
    assert!(!verifier_should_adopt_followed_boundary(
        241,
        119,
        activation_height
    ));
}

#[test]
fn test_startup_live_join_round_follows_chain_dkg_cycle() {
    let (keys, _participants, output, _share, _polynomial) = run_test_dkg_complete();

    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x11),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };

    let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(42),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 41,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 41,
        is_validator_set_change: true,
        tee_reshare_registrations: Vec::new(),
    })
    .unwrap();

    assert_eq!(next_live_reshare_round(&artifact), 42);
}

#[test]
fn test_build_peer_map_from_bootnodes() {
    let key = bls12381::PrivateKey::random(rand_core::OsRng);
    let pk = key.public_key();
    let pk_bytes = commonware_codec::Encode::encode(&pk);

    let addr: std::net::SocketAddr = "127.0.0.1:30400".parse().unwrap();
    let mut bootnode_map = std::collections::BTreeMap::new();
    bootnode_map.insert(pk_bytes.to_vec(), addr);

    let validator_set = validators::ValidatorSet {
        public_keys: vec![pk.clone()],
        addresses: vec![Address::with_last_byte(0x01)],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing], // no static p2p_address
    };

    let peer_map = build_peer_map(&validator_set, &bootnode_map);
    assert_eq!(peer_map.len(), 1);
}

#[test]
fn test_parse_consensus_peers_rejects_invalid_entries() {
    let err = parse_consensus_peers(&["not-a-peer".to_string()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("expected <hex_bls_pubkey>@<host:port>"));

    let err = parse_consensus_peers(&["zz@127.0.0.1:30400".to_string()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("public key is not hex"));

    let err = parse_consensus_peers(&["aa@not-a-socket".to_string()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("invalid socket address"));
}

#[test]
fn test_require_genesis_hash_rejects_missing_hash() {
    let err = require_genesis_hash(None).unwrap_err().to_string();
    assert!(err.contains("missing genesis block hash"));
}

#[test]
fn test_ordered_validator_addresses_rejects_missing_participant_key() {
    let key_a = bls12381::PrivateKey::random(rand_core::OsRng);
    let key_b = bls12381::PrivateKey::random(rand_core::OsRng);
    let participants: commonware_utils::ordered::Set<bls12381::PublicKey> =
        vec![key_a.public_key(), key_b.public_key()]
            .into_iter()
            .try_collect()
            .unwrap();
    let validator_set = validators::ValidatorSet {
        public_keys: vec![key_a.public_key()],
        addresses: vec![Address::with_last_byte(0x01)],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing],
    };

    let err = ordered_validator_addresses(&participants, &validator_set)
        .unwrap_err()
        .to_string();
    assert!(err.contains("participant public key is missing"));
}

#[test]
fn test_recovered_boundary_addresses_survive_latest_state_removal() {
    let (keys, participants, output, _polynomial) = run_test_dkg();
    let boundary_addresses = vec![
        Address::with_last_byte(0x11),
        Address::with_last_byte(0x22),
        Address::with_last_byte(0x33),
    ];
    let boundary_validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|k| k.public_key()).collect(),
        addresses: boundary_addresses.clone(),
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let boundary = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(7),
        validator_set: &boundary_validator_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 6,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 2,
        is_validator_set_change: true,
        tee_reshare_registrations: Vec::new(),
    })
    .unwrap();

    let latest_after_unfinalized_removal = validators::ValidatorSet {
        public_keys: keys.iter().skip(1).map(|k| k.public_key()).collect(),
        addresses: boundary_addresses.iter().skip(1).copied().collect(),
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 2],
    };
    assert!(
        ordered_validator_addresses(&participants, &latest_after_unfinalized_removal).is_err(),
        "provider-latest mapping should fail after an unfinalized removal of an old participant"
    );

    let recovered = ordered_addresses_from_recovered_boundary(&participants, &boundary).unwrap();
    assert_eq!(recovered, boundary_addresses);
}

#[test]
fn test_recovered_boundary_evm_signer_authorization_survives_latest_state_removal() {
    use crate::args::ConsensusArgs;
    use commonware_cryptography::Signer as _;
    use std::net::SocketAddr;

    let temp = tempfile::tempdir().unwrap();
    let evm_key_path = temp.path().join("evm-key.hex");
    let evm_secret = [0x42u8; 32];
    std::fs::write(&evm_key_path, hex::encode(evm_secret)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&evm_key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let evm_signer =
        outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes(evm_secret).unwrap();

    let (keys, participants, output, _polynomial) = run_test_dkg();
    let local_key = &keys[0];
    let boundary_addresses = vec![
        evm_signer.address(),
        Address::with_last_byte(0x22),
        Address::with_last_byte(0x33),
    ];
    let boundary_validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|k| k.public_key()).collect(),
        addresses: boundary_addresses.clone(),
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let boundary = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(7),
        validator_set: &boundary_validator_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 6,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 2,
        is_validator_set_change: true,
        tee_reshare_registrations: Vec::new(),
    })
    .unwrap();

    let latest_after_unfinalized_removal = validators::ValidatorSet {
        public_keys: keys.iter().skip(1).map(|k| k.public_key()).collect(),
        addresses: boundary_addresses.iter().skip(1).copied().collect(),
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 2],
    };
    let args = ConsensusArgs {
        is_validator: true,
        signing_key: Some(temp.path().join("signing-key.hex")),
        validator_evm_key: Some(evm_key_path.clone()),
        signing_share: None,
        public_polynomial: None,
        dkg_output: None,
        listen_address: "127.0.0.1:30400".parse::<SocketAddr>().unwrap(),
        storage_dir: None,
        keys_dir: None,
        trust_el_head: false,
        force_dkg: false,
        testnet_unix_time_offset_secs: None,
        consensus_peers: Vec::new(),
        use_local_defaults: true,
        payload_resolve_time_ms: 200,
        payload_return_time_ms: 450,
        worker_threads: 1,
        bls_key_backend: "plaintext".to_string(),
        bls_passphrase: None,
        tee_enclave_socket: None,
        tee_bootstrap_timeout_secs: 60,
        upstream: None,
        upstream_nocertify: false,
        projection_mongodb_uri: Some("mongodb://localhost:27017".to_owned()),
        projection_mongodb_database: Some("outbe_projection".to_owned()),
        projection_start_block: 1,
    };

    let address = validate_validator_evm_signer(
        &args,
        local_key,
        &latest_after_unfinalized_removal,
        &latest_after_unfinalized_removal,
        Some((&participants, &boundary)),
        false,
    )
    .unwrap();
    assert_eq!(address, Some(evm_signer.address()));

    let wrong_key_path = temp.path().join("wrong-evm-key.hex");
    let wrong_secret = [0x43u8; 32];
    std::fs::write(&wrong_key_path, hex::encode(wrong_secret)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrong_key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let wrong_args = ConsensusArgs {
        validator_evm_key: Some(wrong_key_path),
        ..args
    };
    let err = validate_validator_evm_signer(
        &wrong_args,
        local_key,
        &latest_after_unfinalized_removal,
        &latest_after_unfinalized_removal,
        Some((&participants, &boundary)),
        false,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("does not match recovered DKG boundary address"));
}

#[test]
fn test_register_epoch_validation_providers_is_available_and_first_wins() {
    let (keys, participants, _output, polynomial) = run_test_dkg();
    let vrf_materials = VrfMaterialProvider::new(0, polynomial, None);
    let epoch = Epoch::new(9);
    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x01),
            Address::with_last_byte(0x02),
            Address::with_last_byte(0x03),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let expected_committee = ordered_validator_addresses(&participants, &validator_set).unwrap();
    let scheme_provider = HybridSchemeProvider::new();
    let committee_provider = CommitteeProvider::new();

    register_epoch_validation_providers(
        epoch,
        &participants,
        &validator_set,
        None,
        &vrf_materials,
        &scheme_provider,
        &committee_provider,
    )
    .unwrap();

    assert!(scheme_provider.scoped(epoch).is_some());
    assert_eq!(
        committee_provider
            .ordered_committee(epoch)
            .expect("committee should be registered")
            .as_ref(),
        &expected_committee
    );

    let replacement_set = validators::ValidatorSet {
        public_keys: validator_set.public_keys.clone(),
        addresses: vec![
            Address::with_last_byte(0xAA),
            Address::with_last_byte(0xBB),
            Address::with_last_byte(0xCC),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    register_epoch_validation_providers(
        epoch,
        &participants,
        &replacement_set,
        None,
        &vrf_materials,
        &scheme_provider,
        &committee_provider,
    )
    .unwrap();

    assert_eq!(
        committee_provider
            .ordered_committee(epoch)
            .expect("committee should remain registered")
            .as_ref(),
        &expected_committee
    );
}

#[test]
fn test_build_peer_map_prefers_static_address() {
    let key = bls12381::PrivateKey::random(rand_core::OsRng);
    let pk = key.public_key();
    let pk_bytes = commonware_codec::Encode::encode(&pk);

    let static_addr: std::net::SocketAddr = "10.0.0.1:30400".parse().unwrap();
    let bootnode_addr: std::net::SocketAddr = "192.168.1.1:30400".parse().unwrap();
    let mut bootnode_map = std::collections::BTreeMap::new();
    bootnode_map.insert(pk_bytes.to_vec(), bootnode_addr);

    let validator_set = validators::ValidatorSet {
        public_keys: vec![pk.clone()],
        addresses: vec![Address::with_last_byte(0x01)],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Known(
            commonware_p2p::Address::Symmetric(static_addr),
        )],
    };

    let peer_map = build_peer_map(&validator_set, &bootnode_map);
    assert_eq!(peer_map.len(), 1);
    assert_eq!(
        peer_map.get_value(&pk),
        Some(&commonware_p2p::Address::Symmetric(static_addr))
    );
}

#[test]
fn test_build_peer_map_excludes_invalid_registry_without_bootnode_fallback() {
    let key = bls12381::PrivateKey::random(rand_core::OsRng);
    let pk = key.public_key();
    let pk_bytes = commonware_codec::Encode::encode(&pk);

    let bootnode_addr: std::net::SocketAddr = "192.168.1.1:30400".parse().unwrap();
    let mut bootnode_map = std::collections::BTreeMap::new();
    bootnode_map.insert(pk_bytes.to_vec(), bootnode_addr);

    let validator_set = validators::ValidatorSet {
        public_keys: vec![pk],
        addresses: vec![Address::with_last_byte(0x01)],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Invalid],
    };

    let peer_map = build_peer_map(&validator_set, &bootnode_map);
    assert_eq!(peer_map.len(), 0);
}

#[test]
fn test_build_peer_map_supports_asymmetric_registry_address() {
    let key = bls12381::PrivateKey::random(rand_core::OsRng);
    let pk = key.public_key();
    let ingress: std::net::SocketAddr = "10.0.0.1:30400".parse().unwrap();
    let egress: std::net::SocketAddr = "10.0.0.2:30401".parse().unwrap();
    let address = commonware_p2p::Address::Asymmetric {
        ingress: commonware_p2p::Ingress::Socket(ingress),
        egress,
    };

    let validator_set = validators::ValidatorSet {
        public_keys: vec![pk.clone()],
        addresses: vec![Address::with_last_byte(0x01)],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Known(address.clone())],
    };

    let peer_map = build_peer_map(&validator_set, &std::collections::BTreeMap::new());
    assert_eq!(peer_map.get_value(&pk), Some(&address));
}

#[test]
fn test_build_peer_map_excludes_unreachable() {
    let key = bls12381::PrivateKey::random(rand_core::OsRng);
    let pk = key.public_key();

    // No p2p_address and no bootnode entry → excluded.
    let bootnode_map = std::collections::BTreeMap::new();

    let validator_set = validators::ValidatorSet {
        public_keys: vec![pk],
        addresses: vec![Address::with_last_byte(0x01)],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing],
    };

    let peer_map = build_peer_map(&validator_set, &bootnode_map);
    assert_eq!(peer_map.len(), 0);
}

#[test]
fn test_pending_dkg_activation_blocks_duplicate_rotation_start() {
    assert!(
        !should_start_dkg_rotation(false, true, 99, 90),
        "pending DKG activation for a planned boundary must block duplicate rotation starts"
    );
}

#[test]
fn test_pending_dkg_activation_triggers_at_planned_height() {
    assert_eq!(
        pending_dkg_activation_decision(239, 240, 30),
        PendingDkgActivationDecision::Wait
    );
    assert_eq!(
        pending_dkg_activation_decision(240, 240, 30),
        PendingDkgActivationDecision::Activate
    );
    assert_eq!(
        pending_dkg_activation_decision(270, 240, 30),
        PendingDkgActivationDecision::Activate
    );
    assert_eq!(
        pending_dkg_activation_decision(271, 240, 30),
        PendingDkgActivationDecision::Expired { deadline: 270 }
    );
}

#[test]
fn frozen_dkg_target_expires_at_the_last_proposable_height() {
    assert!(!frozen_dkg_target_expired(269, 240, 30));
    assert!(
        frozen_dkg_target_expired(270, 240, 30),
        "the application refuses block 271, so the supervisor must fail closed at height 270"
    );
    assert!(frozen_dkg_target_expired(271, 240, 30));
}

#[test]
fn local_reshare_role_classifies_old_new_removed_and_outsider() {
    let (old_keys, old_participants, previous_output, _share, _polynomial) =
        run_test_dkg_complete();
    let old_pk = old_keys[0].public_key();

    let new_key = bls12381::PrivateKey::from_seed(10_000);
    let new_pk = new_key.public_key();
    let mut target_with_new: Vec<bls12381::PublicKey> = old_participants.iter().cloned().collect();
    target_with_new.push(new_pk.clone());
    let target_with_new: commonware_utils::ordered::Set<bls12381::PublicKey> =
        target_with_new.into_iter().try_collect().unwrap();

    assert_eq!(
        classify_local_reshare_role(&old_pk, Some(&previous_output), &target_with_new),
        LocalDkgRole::DealerAndPlayer
    );
    assert_eq!(
        classify_local_reshare_role(&new_pk, Some(&previous_output), &target_with_new),
        LocalDkgRole::PlayerOnly
    );

    let target_without_old: commonware_utils::ordered::Set<bls12381::PublicKey> = old_participants
        .iter()
        .filter(|pk| *pk != &old_pk)
        .cloned()
        .try_collect()
        .unwrap();
    assert_eq!(
        classify_local_reshare_role(&old_pk, Some(&previous_output), &target_without_old),
        LocalDkgRole::DealerOnly
    );

    let outsider = bls12381::PrivateKey::from_seed(20_000).public_key();
    assert_eq!(
        classify_local_reshare_role(&outsider, Some(&previous_output), &target_without_old),
        LocalDkgRole::NotParticipant
    );
}

#[test]
fn test_dkg_activation_always_advances_consensus_epoch() {
    assert_eq!(
        next_consensus_epoch_after_dkg_activation(Epoch::new(0)),
        Epoch::new(1)
    );
    assert_eq!(
        next_consensus_epoch_after_dkg_activation(Epoch::new(41)),
        Epoch::new(42)
    );
}

#[test]
fn verifier_rotation_discovered_at_or_after_activation_replays_current_height() {
    assert!(!verifier_activation_needs_immediate_replay(119, 120));
    assert!(verifier_activation_needs_immediate_replay(120, 120));
    assert!(verifier_activation_needs_immediate_replay(121, 120));
}

#[test]
fn test_missing_freeze_block_hash_retries_only_before_planned_activation() {
    assert_eq!(
        pending_freeze_block_hash_decision(119, 120),
        PendingFreezeBlockHashDecision::Retry
    );
    assert_eq!(
        pending_freeze_block_hash_decision(120, 120),
        PendingFreezeBlockHashDecision::Expired
    );
    assert_eq!(
        pending_freeze_block_hash_decision(121, 120),
        PendingFreezeBlockHashDecision::Expired
    );
}

#[test]
fn test_epoch_elector_config_allows_genesis_without_continuity() {
    let (_, participants, _, _) = run_test_dkg();
    let dkg = bootstrap_dkg(3).unwrap();
    let vrf_materials = VrfMaterialProvider::new(0, dkg.polynomial, None);
    let config =
        epoch_elector_config(Epoch::new(0), &ReporterContinuity::default(), vrf_materials).unwrap();
    let elector: outbe_consensus::hybrid::election::HybridRandomElector<MinSig> =
        config.build(&participants);
    let leader = elector.elect(Round::new(Epoch::new(0), View::new(1)), None);
    assert!(leader.get() < participants.len() as u32);
}

#[test]
fn test_epoch_elector_config_allows_recovered_epoch_without_continuity() {
    let dkg = bootstrap_dkg(3).unwrap();
    let vrf_materials = VrfMaterialProvider::new(0, dkg.polynomial, None);
    assert!(
        epoch_elector_config(Epoch::new(1), &ReporterContinuity::default(), vrf_materials).is_ok()
    );
}

#[test]
fn test_epoch_elector_config_uses_previous_certificate_for_view_one() {
    let certificate = sample_certificate();
    let continuity = ReporterContinuity::default();
    let seed = certificate.raw_vrf_seed_bytes().unwrap();
    continuity.update(9, Some(certificate.clone()), Some(seed.clone()));

    let (_, participants, _, _) = run_test_dkg();
    let dkg = bootstrap_dkg(3).unwrap();
    let vrf_materials = VrfMaterialProvider::new(0, dkg.polynomial, None);
    let config = epoch_elector_config(Epoch::new(1), &continuity, vrf_materials).unwrap();
    let elector: outbe_consensus::hybrid::election::HybridRandomElector<MinSig> =
        config.build(&participants);

    let leader = elector.elect(Round::new(Epoch::new(1), View::new(1)), None);
    let expected = commonware_utils::Participant::new(commonware_utils::modulo(
        seed.as_ref(),
        participants.len() as u64,
    ) as u32);

    assert_eq!(leader, expected);
}

#[test]
fn test_save_and_load_dkg_state_preserves_output() {
    let (_keys, _participants, output, share, polynomial) = run_test_dkg_complete();
    let dir = tempfile::tempdir().unwrap();
    let backend = bls::KeyBackend::Plaintext;

    save_dkg_state(dir.path(), &share, &polynomial, &output, &backend).unwrap();

    let (loaded_share, loaded_polynomial, loaded_output) =
        load_saved_dkg_state(dir.path(), &backend).unwrap().unwrap();

    assert_eq!(loaded_share.index, share.index);
    assert_eq!(loaded_polynomial.encode(), polynomial.encode());
    assert_eq!(loaded_output, output);
}

#[test]
fn test_load_saved_dkg_state_rejects_incomplete_files() {
    let (_keys, _participants, _output, share, polynomial) = run_test_dkg_complete();
    let dir = tempfile::tempdir().unwrap();
    let backend = bls::KeyBackend::Plaintext;

    bls::save_signing_share(&dir.path().join(DKG_SHARE_FILE), &share, &backend).unwrap();
    bls::save_public_polynomial(&dir.path().join(DKG_POLYNOMIAL_FILE), &polynomial, &backend)
        .unwrap();

    let error = load_saved_dkg_state(dir.path(), &backend).unwrap_err();
    assert!(error.to_string().contains("saved DKG state is incomplete"));
}

// =============================================================================
// T3 — ordered::Set index shift on prefix-sort join (must pass).
//
// Prepending a BLS pubkey that sorts before all existing keys to an ordered::Set
// shifts the indices of every original key by +1. Production code that builds
// `participants` from a live 4-key set after a 3-key DKG would therefore observe
// participant indices that no longer match the share.index baked into the
// saved DKG output (hybrid.rs:472-481, invariant).
//
// This is a structural assertion about ordered::Set, not a probabilistic one.
// =============================================================================
#[test]
fn ordered_set_index_shift_on_prefix_join() {
    use commonware_utils::ordered::Set;

    // Generate 3 BLS pubkeys deterministically.
    let mut keys: Vec<bls12381::PrivateKey> =
        (1u64..=3).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|k| commonware_codec::Encode::encode(&k.public_key()));

    let participants_3: Set<bls12381::PublicKey> =
        keys.iter().map(|k| k.public_key()).try_collect().unwrap();

    // Capture each original participant's index in the 3-key set.
    let original_indices: Vec<(bls12381::PublicKey, commonware_utils::Participant)> = keys
        .iter()
        .map(|k| {
            let pk = k.public_key();
            let idx = participants_3.index(&pk).unwrap();
            (pk, idx)
        })
        .collect();

    // Find a 4th BLS pubkey whose encoding sorts before all 3 originals.
    // BLS pubkeys are compressed G1 elements with byte values uniformly
    // distributed enough that a sort-before key is found within a small
    // seed window in practice.
    let smallest = commonware_codec::Encode::encode(&keys[0].public_key());
    let new_key = (4u64..1_000_000)
        .find_map(|seed| {
            let candidate = bls12381::PrivateKey::from_seed(seed);
            let bytes = commonware_codec::Encode::encode(&candidate.public_key());
            if bytes < smallest {
                Some(candidate)
            } else {
                None
            }
        })
        .expect("could not find a sort-before BLS pubkey within seed window");

    // Build a 4-key participants set including the new key + the 3 originals.
    let mut all_4: Vec<bls12381::PublicKey> = keys.iter().map(|k| k.public_key()).collect();
    all_4.push(new_key.public_key());
    let participants_4: Set<bls12381::PublicKey> = all_4.into_iter().try_collect().unwrap();

    // The new key sits at position 0; every original key shifts by +1.
    let mut shifted_count = 0usize;
    for (pk, original_idx) in &original_indices {
        let new_idx = participants_4.index(pk).unwrap();
        if new_idx != *original_idx {
            shifted_count += 1;
        }
    }

    assert_eq!(
        shifted_count,
        original_indices.len(),
        "expected every original participant's index to shift by +1 after prepending a sort-before key, but {} of {} shifted",
        shifted_count,
        original_indices.len()
    );

    // The new key occupies sorted position 0 in the 4-key set.
    let new_pk = new_key.public_key();
    assert_eq!(
        participants_4.index(&new_pk).unwrap().get(),
        0,
        "newly prepended key must occupy sorted position 0"
    );
}

// =============================================================================
// T0 — Commonware Muxer drop vs backup-capture contract.
//
// The Outbe consensus stack uses `Muxer::new(...)` (no backup) for vote / cert
// / resolver / dkg sub-channels and registers a fresh sub-channel for every
// new epoch (see stack.rs:513-549, 1009-1017). If a peer sends a message on
// epoch N's sub-channel before the receiver has registered that sub-channel
// on its end, the message is dropped — there is no replay path back into the
// late registrant.
//
// These two tests pin the Muxer contract for the pinned commonware-p2p tag
// (v2026.3.0) so that any future bump to a tag with different semantics fails
// loudly rather than silently changing the boundary-race surface.
// =============================================================================
#[cfg(test)]
mod muxer_contract {
    use commonware_cryptography::ed25519::{PrivateKey as Ed25519PrivateKey, PublicKey};
    use commonware_cryptography::Signer as _;
    use commonware_p2p::{
        simulated::{self, Link, Network, Oracle},
        utils::mux::{Builder as _, Muxer},
        Channel, Receiver as _, Recipients, Sender as _,
    };
    use commonware_runtime::{deterministic, Clock as _, IoBuf, Quota, Runner, Supervisor as _};
    use std::{num::NonZeroU32, time::Duration};

    const LINK: Link = Link {
        latency: Duration::from_millis(0),
        jitter: Duration::from_millis(0),
        success_rate: 1.0,
    };
    const CAPACITY: usize = 4;
    const TEST_QUOTA: Quota = Quota::per_second(NonZeroU32::MAX);
    /// p2p::Channel namespace for these tests. Type alias = `u64`.
    const PHYSICAL_CHANNEL: Channel = 0;
    /// Sub-channel id used in the test, modelling an epoch sub-channel id in
    /// production code.
    const EPOCH_SUBCHANNEL: Channel = 42;

    fn pk(seed: u64) -> PublicKey {
        Ed25519PrivateKey::from_seed(seed).public_key()
    }

    fn start_network(context: deterministic::Context) -> Oracle<PublicKey, deterministic::Context> {
        let (network, oracle) = Network::new(
            context.child("network"),
            simulated::Config {
                max_size: 1024 * 1024,
                disconnect_on_block: true,
                tracked_peer_sets: commonware_utils::NZUsize!(4),
            },
        );
        network.start();
        oracle
    }

    async fn link_bidirectional(
        oracle: &mut Oracle<PublicKey, deterministic::Context>,
        a: PublicKey,
        b: PublicKey,
    ) {
        oracle.add_link(a.clone(), b.clone(), LINK).await.unwrap();
        oracle.add_link(b, a, LINK).await.unwrap();
    }

    /// Without `.with_backup()`, a message sent to a sub-channel that the
    /// receiver has not yet registered is dropped. Even if the receiver
    /// registers later, it never observes the early message.
    #[test]
    fn mux_drops_messages_to_unregistered_subchannel() {
        let executor = deterministic::Runner::timed(Duration::from_secs(10));
        executor.start(|context| async move {
            // 2026.5.0: `deterministic::Context` is no longer `Clone`; pass a
            // child context to the network and keep `context` for the test body
            // (labels via `child` need `Supervisor` in scope).
            let mut oracle = start_network(context.child("network_owner"));

            let pk_sender = pk(0);
            let pk_receiver = pk(1);

            // Sender peer: register the physical channel + the epoch sub-channel.
            let (s_sender, s_receiver) = oracle
                .control(pk_sender.clone())
                .register(PHYSICAL_CHANNEL, TEST_QUOTA)
                .await
                .unwrap();
            let (s_mux, mut s_handle) =
                Muxer::new(context.child("sender_mux"), s_sender, s_receiver, CAPACITY);
            s_mux.start();

            // Receiver peer: register the physical channel only — sub-channel
            // is *not* registered yet.
            let (r_sender, r_receiver) = oracle
                .control(pk_receiver.clone())
                .register(PHYSICAL_CHANNEL, TEST_QUOTA)
                .await
                .unwrap();
            let (r_mux, mut r_handle) = Muxer::new(
                context.child("receiver_mux"),
                r_sender,
                r_receiver,
                CAPACITY,
            );
            r_mux.start();

            link_bidirectional(&mut oracle, pk_sender.clone(), pk_receiver.clone()).await;

            // Sender registers and sends a message on the epoch sub-channel
            // *before* the receiver has registered it.
            let (mut tx, _) = s_handle.register(EPOCH_SUBCHANNEL).await.unwrap();
            let payload = IoBuf::copy_from_slice(b"early-vote");
            // 2026.5.0: `Sender::send` is SYNC and returns `Vec<PublicKey>` (the
            // recipients we attempted to deliver to), not a future/Result.
            let _ = tx.send(Recipients::One(pk_receiver.clone()), payload.clone(), false);

            // Wait for the simulated network to drain the message into the
            // receiver muxer (which will drop it, since the sub-channel is
            // not registered there).
            context.sleep(Duration::from_millis(100)).await;

            // Now the receiver registers the sub-channel — too late.
            let (_, mut rx) = r_handle.register(EPOCH_SUBCHANNEL).await.unwrap();

            // Bound the wait. With LINK latency = 0 and SubReceiver mailbox
            // empty, recv() will block forever on the contract this test
            // pins; we treat any receipt within the bound as a contract break.
            let timed = context.sleep(Duration::from_millis(500));
            tokio::pin!(timed);
            tokio::select! {
                received = rx.recv() => {
                    let _ = received;
                    panic!(
                        "muxer contract violation: late registrant received a message that was \
                         sent before its sub-channel was registered"
                    );
                }
                _ = &mut timed => {
                    // Expected: timed out without receiving — message was dropped.
                }
            }
        });
    }

    /// With `.with_backup()`, the same early message is captured into the
    /// backup receiver as `(subchannel, (peer_pk, payload))`. The late-
    /// registrant of the sub-channel still does **not** see it — backup is
    /// a capture surface, not an auto-replay mechanism.
    #[test]
    fn mux_with_backup_captures_unrouted_message_but_does_not_replay() {
        let executor = deterministic::Runner::timed(Duration::from_secs(10));
        executor.start(|context| async move {
            // 2026.5.0: `deterministic::Context` is no longer `Clone`; pass a
            // child context to the network and keep `context` for the test body.
            let mut oracle = start_network(context.child("network_owner"));

            let pk_sender = pk(0);
            let pk_receiver = pk(1);

            let (s_sender, s_receiver) = oracle
                .control(pk_sender.clone())
                .register(PHYSICAL_CHANNEL, TEST_QUOTA)
                .await
                .unwrap();
            let (s_mux, mut s_handle) =
                Muxer::new(context.child("sender_mux"), s_sender, s_receiver, CAPACITY);
            s_mux.start();

            let (r_sender, r_receiver) = oracle
                .control(pk_receiver.clone())
                .register(PHYSICAL_CHANNEL, TEST_QUOTA)
                .await
                .unwrap();
            let (r_mux, mut r_handle, mut backup_rx) = Muxer::builder(
                context.child("receiver_mux"),
                r_sender,
                r_receiver,
                CAPACITY,
            )
            .with_backup()
            .build();
            r_mux.start();

            link_bidirectional(&mut oracle, pk_sender.clone(), pk_receiver.clone()).await;

            // commonware 2026.4.0: routing requires a tracked peer set, not just
            // a link. Track both peers so the Recipients::One send resolves.
            {
                use commonware_p2p::Manager as _;
                let peers = commonware_utils::ordered::Set::from_iter_dedup([
                    pk_sender.clone(),
                    pk_receiver.clone(),
                ]);
                // 2026.5.0: `Manager::track` is SYNC and returns `Feedback`.
                let _ = oracle.manager().track(0, peers);
            }

            let (mut tx, _) = s_handle.register(EPOCH_SUBCHANNEL).await.unwrap();
            let payload = IoBuf::copy_from_slice(b"early-vote");
            // 2026.5.0: `Sender::send` is SYNC and returns `Vec<PublicKey>`.
            let _ = tx.send(Recipients::One(pk_receiver.clone()), payload.clone(), false);

            // Drain into backup channel.
            let timed = context.sleep(Duration::from_secs(2));
            tokio::pin!(timed);
            let captured = tokio::select! {
                msg = backup_rx.recv() => msg.expect("backup recv must produce a message"),
                _ = &mut timed => {
                    panic!("muxer with backup did not capture the unrouted message");
                }
            };
            let (subchannel, (from, bytes)) = captured;
            assert_eq!(subchannel, EPOCH_SUBCHANNEL);
            assert_eq!(from, pk_sender);
            // The captured payload contains the muxer's framing prefix
            // (varint sub-channel id) followed by our raw payload. We assert
            // that our payload bytes appear at the tail so we don't depend on
            // the exact framing format.
            let captured_bytes: &[u8] = bytes.as_ref();
            let expected_bytes: &[u8] = payload.as_ref();
            assert!(
                captured_bytes.ends_with(expected_bytes),
                "backup-captured bytes did not contain the original payload as suffix"
            );

            // Now register the sub-channel on the receiver — assert that the
            // late registrant does **not** receive the message that was
            // already drained into backup.
            let (_, mut rx) = r_handle.register(EPOCH_SUBCHANNEL).await.unwrap();
            let timed_late = context.sleep(Duration::from_millis(500));
            tokio::pin!(timed_late);
            tokio::select! {
                received = rx.recv() => {
                    let _ = received;
                    panic!(
                        "muxer with backup auto-replayed into the late registrant; this is \
                         not the v2026.3.0 contract — production fix design must change"
                    );
                }
                _ = &mut timed_late => {
                    // Expected: backup captured, late registrant blank.
                }
            }
        });
    }
}

// =============================================================================
// T1 / T2a / T2b / T5 — multi-node simplex deterministic harness.
//
// These tests run the actual `simplex::Engine` over a deterministic
// simulated network with outbe-chain's `HybridScheme<MinSig>` and the
// shared `crate::epoch_subchannels::register_epoch_subchannels` /
// `take_or_register_current` helper that production also uses in
// `stack.rs`. Toggling `use_pre_registration` in the harness switches
// between the pre-fix lazy path and the post-fix pre-register path.
//
// Foundation tests T0 (`muxer_contract::*`) and T3
// (`ordered_set_index_shift_on_prefix_join`) above pin the underlying
// commonware-p2p Muxer contract and `ordered::Set` ordering invariant
// respectively.
// =============================================================================

#[test]
fn epoch_transition_finalizes_view_one() {
    use commonware_consensus::types::{Epoch, View};
    use commonware_runtime::{deterministic, Runner};
    use std::time::Duration;

    let runner = deterministic::Runner::timed(Duration::from_secs(30));
    runner.start(|ctx| async move {
        let mut harness = outbe_consensus::test_harness::Harness::new(&ctx, 3).await;
        // Epoch::new(2) → RoundRobin leader = (2+1) % 3 = 0; arbitrary
        // baseline cycle.
        let outcome = harness
            .run_cycle(
                Epoch::new(2),
                outbe_consensus::test_harness::CycleOptions {
                    use_pre_registration: true,
                    leader_timeout: Duration::from_millis(500),
                    run_for: Duration::from_millis(2_000),
                    ..Default::default()
                },
            )
            .await;
        assert!(
            outcome.all_finalized_view_one(),
            "T1 baseline: every node must finalize view 1; got {:?}",
            outcome.view_finalized_per_node
        );
        let _ = View::new(1);
    });
}

#[test]
fn cross_node_race_stalls_under_lazy_registration() {
    use commonware_consensus::types::Epoch;
    use commonware_runtime::{deterministic, Runner};
    use std::collections::HashMap;
    use std::time::Duration;

    let runner = deterministic::Runner::timed(Duration::from_secs(30));
    runner.start(|ctx| async move {
        let mut harness = outbe_consensus::test_harness::Harness::new(&ctx, 3).await;
        let epoch = Epoch::new(2); // leader index = (2+1) % 3 = 0
        let leader = harness.leader_for_view_one(epoch);

        // Identical timing to T2b. Only `use_pre_registration: false`
        // differs. In the lazy path, `dkg_completion_delay` is ignored
        // (no pre-register) so followers' Mux registers the new epoch
        // only at `activation_delay = 500ms`. Leader fires at 150ms;
        // 150-500ms window has no follower route → Mux drop → stall.
        let mut dkg_completion = HashMap::new();
        let mut activation = HashMap::new();
        for i in 0..3 {
            if i == leader {
                dkg_completion.insert(i, Duration::from_millis(0));
                activation.insert(i, Duration::from_millis(150));
            } else {
                dkg_completion.insert(i, Duration::from_millis(100));
                activation.insert(i, Duration::from_millis(500));
            }
        }

        let outcome = harness
            .run_cycle(
                epoch,
                outbe_consensus::test_harness::CycleOptions {
                    use_pre_registration: false,
                    dkg_completion_delay_per_node: dkg_completion,
                    activation_delay_per_node: activation,
                    leader_timeout: Duration::from_millis(500),
                    // Discriminating window: just past followers'
                    // activation, before view-1 nullification could
                    // recover into view 2.
                    run_for: Duration::from_millis(750),
                },
            )
            .await;

        // At least one follower's view-1 must NOT have finalized.
        let any_follower_stalled = outcome
            .followers()
            .any(|i| !outcome.view_finalized_per_node[i]);
        assert!(
            any_follower_stalled,
            "T2a: at least one follower must fail to finalize view 1 under lazy \
             registration; outcome={:?}",
            outcome.view_finalized_per_node
        );
    });
}

#[test]
fn pre_register_helper_avoids_cross_node_race() {
    use commonware_consensus::types::Epoch;
    use commonware_runtime::{deterministic, Runner};
    use std::collections::HashMap;
    use std::time::Duration;

    let runner = deterministic::Runner::timed(Duration::from_secs(30));
    runner.start(|ctx| async move {
        let mut harness = outbe_consensus::test_harness::Harness::new(&ctx, 3).await;
        let epoch = Epoch::new(2);
        let leader = harness.leader_for_view_one(epoch);

        // Same timing scenario as T2a: leader activates fast,
        // followers slow. The only difference is `use_pre_registration:
        // true`, which in the harness invokes
        // `register_epoch_subchannels` at modeled DKG completion —
        // exactly the function the production fix calls in
        // stack.rs:1124-1190.
        let mut dkg_completion = HashMap::new();
        let mut activation = HashMap::new();
        for i in 0..3 {
            if i == leader {
                dkg_completion.insert(i, Duration::from_millis(0));
                activation.insert(i, Duration::from_millis(150));
            } else {
                // Followers' DKG completion fires BEFORE the leader's
                // activation (modeling the production fix's
                // pre-register-at-DKG-completion guarantee). Their
                // activation lags.
                dkg_completion.insert(i, Duration::from_millis(100));
                activation.insert(i, Duration::from_millis(500));
            }
        }

        let outcome = harness
            .run_cycle(
                epoch,
                outbe_consensus::test_harness::CycleOptions {
                    use_pre_registration: true,
                    dkg_completion_delay_per_node: dkg_completion,
                    activation_delay_per_node: activation,
                    leader_timeout: Duration::from_millis(500),
                    run_for: Duration::from_millis(2_000),
                },
            )
            .await;

        assert!(
            outcome.all_finalized_view_one(),
            "T2b: every node must finalize view 1 once next-epoch \
             sub-channels are pre-registered; outcome={:?}",
            outcome.view_finalized_per_node
        );
    });
}

#[test]
fn repeated_dkg_cycles_no_stall() {
    use commonware_consensus::types::{Epoch, View};
    use commonware_runtime::{deterministic, Runner};
    use std::collections::HashMap;
    use std::time::Duration;

    let runner = deterministic::Runner::timed(Duration::from_secs(60));
    runner.start(|ctx| async move {
        let mut harness = outbe_consensus::test_harness::Harness::new(&ctx, 3).await;
        for raw_epoch in 2u64..=6 {
            let epoch = Epoch::new(raw_epoch);
            let leader = harness.leader_for_view_one(epoch);
            let mut dkg_completion = HashMap::new();
            let mut activation = HashMap::new();
            for i in 0..3 {
                if i == leader {
                    dkg_completion.insert(i, Duration::from_millis(0));
                    activation.insert(i, Duration::from_millis(80));
                } else {
                    dkg_completion.insert(i, Duration::from_millis(30));
                    activation.insert(i, Duration::from_millis(100));
                }
            }
            let outcome = harness
                .run_cycle(
                    epoch,
                    outbe_consensus::test_harness::CycleOptions {
                        use_pre_registration: true,
                        dkg_completion_delay_per_node: dkg_completion,
                        activation_delay_per_node: activation,
                        leader_timeout: Duration::from_millis(500),
                        run_for: Duration::from_millis(3_000),
                    },
                )
                .await;
            let finalized_view_three = outcome
                .finalized_view_per_node
                .iter()
                .all(|view| *view >= View::new(3));
            assert!(
                finalized_view_three,
                "T5 cycle {raw_epoch}: every node must finalize at least view 3; \
                 outcome={:?}",
                outcome.finalized_view_per_node
            );
        }
    });
}

#[test]
fn evm_signer_validation_allows_active_validator_waiting_for_live_join_share() {
    use crate::args::ConsensusArgs;
    use crate::validators::{ValidatorP2pAddress, ValidatorSet};
    use commonware_cryptography::Signer as _;
    use std::net::SocketAddr;

    let temp = tempfile::tempdir().unwrap();
    let evm_key_path = temp.path().join("evm-key.hex");
    let evm_secret = [0x11u8; 32];
    std::fs::write(&evm_key_path, hex::encode(evm_secret)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&evm_key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let evm_signer =
        outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes(evm_secret).unwrap();
    let bls_key = bls12381::PrivateKey::from_seed(7);
    let consensus_set = ValidatorSet {
        public_keys: Vec::new(),
        addresses: Vec::new(),
        p2p_addresses: Vec::new(),
    };
    let active_set = ValidatorSet {
        public_keys: vec![bls_key.public_key()],
        addresses: vec![evm_signer.address()],
        p2p_addresses: vec![ValidatorP2pAddress::Missing],
    };
    let args = ConsensusArgs {
        is_validator: true,
        signing_key: Some(temp.path().join("signing-key.hex")),
        validator_evm_key: Some(evm_key_path),
        signing_share: None,
        public_polynomial: None,
        dkg_output: None,
        listen_address: "127.0.0.1:30400".parse::<SocketAddr>().unwrap(),
        storage_dir: None,
        keys_dir: None,
        trust_el_head: false,
        force_dkg: false,
        testnet_unix_time_offset_secs: None,
        consensus_peers: Vec::new(),
        use_local_defaults: true,
        payload_resolve_time_ms: 200,
        payload_return_time_ms: 450,
        worker_threads: 1,
        bls_key_backend: "plaintext".to_string(),
        bls_passphrase: None,
        tee_enclave_socket: None,
        tee_bootstrap_timeout_secs: 60,
        upstream: None,
        upstream_nocertify: false,
        projection_mongodb_uri: Some("mongodb://localhost:27017".to_owned()),
        projection_mongodb_database: Some("outbe_projection".to_owned()),
        projection_start_block: 1,
    };

    let address = super::validate_validator_evm_signer(
        &args,
        &bls_key,
        &consensus_set,
        &active_set,
        None,
        false,
    )
    .unwrap();

    assert_eq!(address, Some(evm_signer.address()));

    // Verifier-join: an EVM signer NOT in either set must NOT bail when verifier_join
    // is true — it returns None (the node syncs as a verifier). The same signer with
    // verifier_join=false bails (the existing member-required contract).
    let empty = crate::validators::ValidatorSet {
        public_keys: Vec::new(),
        addresses: Vec::new(),
        p2p_addresses: Vec::new(),
    };
    assert!(
        super::validate_validator_evm_signer(&args, &bls_key, &empty, &empty, None, false).is_err(),
        "non-member must bail when not verifier-join"
    );
    assert_eq!(
        super::validate_validator_evm_signer(&args, &bls_key, &empty, &empty, None, true).unwrap(),
        None,
        "non-member must run as verifier (None) when verifier-join"
    );
}

// T-3 / behavioural counterpart of the removed source-grep test in
// `crates/blockchain/evm/tests/genesis.rs`. `validate_recovered_vrf_material`
// must reject when the locally-recovered VRF group public key disagrees with
// the finalized boundary artifact, and must accept when they match (or when
// no boundary is supplied — bootstrap path).
#[test]
fn validate_recovered_vrf_material_accepts_matching_boundary_rejects_mismatch() {
    let (_keys, _participants, _output, _share, polynomial) = run_test_dkg_complete();

    let local_group_pk =
        alloy_primitives::keccak256(commonware_codec::Encode::encode(polynomial.public()));

    // No boundary → bootstrap path is allowed.
    super::validate_recovered_vrf_material(&polynomial, None).expect("bootstrap path must accept");

    // Matching boundary → accept.
    let matching = test_boundary_with_vrf_hash(local_group_pk, 1);
    super::validate_recovered_vrf_material(&polynomial, Some(&matching))
        .expect("matching VRF group public key must accept");

    // Mismatching boundary → reject with the operator-facing error string.
    let mismatching = test_boundary_with_vrf_hash(B256::repeat_byte(0xEE), 1);
    let err = super::validate_recovered_vrf_material(&polynomial, Some(&mismatching))
        .expect_err("mismatched VRF group public key must reject");
    assert!(
        err.to_string()
            .contains("saved DKG material does not match finalized VRF group public key"),
        "operator-facing error string must surface in the rejection: got {err}"
    );
}

// =============================================================================
// T4 — recovery picks participants from the recovered DKG output's committee
//      (the share holders), NOT the latest on-chain set, and fails fast when
// the restored material does not match the recovered boundary.
//
// `select_recovery_participants` is the pure decision the recovery path now
// uses at stack.rs §7. The output's `players()` is already a sorted/deduped
// `commonware_utils::ordered::Set`, so participant indices derive from it
// canonically — the test asserts membership and the explicit drift error.
// =============================================================================

/// Build a `DkgBoundaryArtifact` whose `reshare.new_active_set` records `n`
/// distinct validator addresses — the committee the ceremony ran for.
fn test_boundary_with_active_set_len(n: usize) -> DkgBoundaryArtifact {
    let mut boundary = test_boundary_with_vrf_hash(B256::with_last_byte(0xC1), 7);
    boundary.reshare.new_active_set = (0..n).map(|i| Address::repeat_byte(i as u8 + 1)).collect();
    boundary
}

#[test]
fn recovery_uses_recovered_committee_not_latest() {
    // Recovered DKG output for a 3-validator committee. `players()` is the
    // sorted set of the three consensus pubkeys — the share holders.
    let recovered_players: commonware_utils::ordered::Set<bls12381::PublicKey> = (1u64..=3)
        .map(bls12381::PrivateKey::from_seed)
        .map(|key| key.public_key())
        .try_collect()
        .expect("3-key recovered participant set");

    // Subcase 1: the latest on-chain set has drifted to 4 keys, but the recovered
    // boundary recorded the 3-validator committee the material belongs to.
    // Recovery reconstructs against the recovered 3-key committee, ignoring latest.
    let boundary_ok = test_boundary_with_active_set_len(3);
    let resolved = super::select_recovery_participants(&recovered_players, &boundary_ok)
        .expect("matching committee size must reconstruct against the recovered committee");
    assert_eq!(
        resolved.len(),
        3,
        "must reconstruct against the recovered 3-key committee, not the drifted latest set"
    );
    assert_eq!(
        resolved, recovered_players,
        "resolved participants must be exactly the recovered DKG output's player set"
    );

    // Subcase 2: the recovered boundary records a 4-validator active set while the
    // restored DKG output has only 3 players — the consensus material does not
    // match the recovered chain boundary. Recovery must fail fast with an explicit
    // drift error rather than build the scheme against the wrong committee.
    let boundary_drift = test_boundary_with_active_set_len(4);
    let err = super::select_recovery_participants(&recovered_players, &boundary_drift)
        .expect_err("size mismatch between recovered material and boundary must fail fast");
    assert!(
        err.to_string()
            .contains("validator set has drifted from saved DKG"),
        "operator-facing drift error must surface in the rejection: got {err}"
    );
}

// ---------------------------------------------------------------------------
// BUG-B regression: telemetry label charset (real validator, not source scan).
// ---------------------------------------------------------------------------

/// commonware 2026.5.0's `validate_label` panics if a span/metric label is not
/// `[a-zA-Z][a-zA-Z0-9_]*`. The `with_label` -> `.child()` migration carried
/// dotted labels `dkg.live`/`dkg.retry`, which panicked at block ~90 during DKG
/// rotation — a rare path no short localnet hits. This feeds the labels the
/// engine passes to `Context::child(...)` through the REAL commonware validator
/// (the same function the runtime invokes), so an invalid label fails here
/// instead of in production. Asserts real label values via the real validator;
/// it does NOT scan source text.
///
/// Add new labels here when introducing a labeled child context. New labels are
/// additionally caught at runtime (commonware panics) by the localnet harness,
/// which spawns the `dkg_retry`/`dkg_live` contexts during epoch rotation.
const ENGINE_SPAWN_LABELS: &[&str] = &[
    "application",
    "broadcast",
    "cert_mux",
    "dkg_live",
    "dkg_mux",
    "dkg_retry",
    "engine",
    "executor",
    "finalization",
    "marshal",
    "marshal_blocks",
    "marshal_finalizations",
    "marshal_resolver",
    "network",
    "network_owner",
    "peer_manager",
    "receiver_mux",
    "recovery_blocks",
    "recovery_finalizations",
    "recovery_marshal",
    "res_mux",
    "resolver_handler",
    "sender_mux",
    "vote_mux",
];

#[test]
fn engine_spawn_labels_pass_commonware_validate_label() {
    for label in ENGINE_SPAWN_LABELS {
        commonware_runtime::telemetry::metrics::validate_label(label);
    }
}

/// Guard the guard: prove `validate_label` actually rejects the dotted form that
/// caused BUG-B, so the test above is meaningful (not a no-op validator).
#[test]
#[should_panic]
fn dotted_label_is_rejected_by_commonware_validate_label() {
    commonware_runtime::telemetry::metrics::validate_label("dkg.live");
}

// ---------------------------------------------------------------------------
// marshal-1 regression: restart-from-finalized monotonicity.
// ---------------------------------------------------------------------------

/// commonware 2026.5.0 `marshal::core::Actor::init` returns `Option<Height>`;
/// stack.rs maps `None` (no durable consensus finalization) -> `Height::zero()`
/// (fresh genesis) and `Some(N)` -> `N`. A mis-mapped `None` (e.g.
/// `unwrap_or(nonzero)`) would compile clean but reset a restarted node toward
/// genesis. This pins the mapping contract.
#[test]
fn marshal_init_option_height_maps_none_to_genesis_zero() {
    // Exercise the PRODUCTION mapping (super::map_marshal_init_height), not stdlib
    // Option::unwrap_or — so a regression in how Actor::init's Option<Height> is
    // mapped (e.g. mapping None to a non-zero height, or dropping Some(n)) fails here.
    assert_eq!(super::map_marshal_init_height(None).get(), 0);
    assert_eq!(
        super::map_marshal_init_height(Some(Height::new(7))).get(),
        7
    );
    assert_eq!(
        super::map_marshal_init_height(Some(Height::zero())).get(),
        0
    );
}

/// A node that has already finalized (`Some(N>0)`) — or whose execution layer
/// recovered after a crash with consensus still durable — must classify as an
/// existing-chain join: it must NOT re-run the initial genesis DKG and the
/// genesis-formation gate must NOT (re)form genesis. An inverted height check
/// would compile clean but re-run genesis DKG on a restarted validator.
#[test]
fn restarted_finalized_node_does_not_refresh_genesis_dkg() {
    let fresh = StartupDkgContext {
        last_execution_height: 0,
        last_consensus_finalized_height: 0,
        recovered_boundary_finalized: false,
        recovered_vrf_group_public_key: None,
        recovered_dkg_output_hash: None,
        genesis_formation_proven: true,
    };
    // Genuinely fresh node (local key in set, no force) runs the genesis DKG.
    assert_eq!(
        startup_dkg_mode(fresh, true, false),
        StartupDkgMode::InitialGenesisDkg
    );

    // Restarted after finalizing 42 blocks (durable Some(42)) -> live join,
    // never a fresh genesis DKG.
    let finalized = StartupDkgContext {
        last_consensus_finalized_height: 42,
        ..fresh
    };
    assert_eq!(
        startup_dkg_mode(finalized, true, false),
        StartupDkgMode::LiveJoinRequired,
        "a node that already finalized blocks must NOT re-run the initial genesis DKG"
    );

    // The genesis-formation gate short-circuits to existing-chain on any prior
    // progress, regardless of peer evidence.
    let genesis = B256::repeat_byte(0x11);
    let evidence = RethGenesisPeerEvidence {
        connected_peers: 0,
        is_syncing: false,
        is_initially_syncing: false,
        peer_query_failed: false,
        peers: Vec::new(),
    };
    assert_eq!(
        genesis_formation_gate_decision(finalized, genesis, 3, &evidence),
        GenesisFormationGate::ExistingChainJoin,
        "durable consensus finalization must classify as existing-chain join"
    );
    // Crash recovery: execution lost (height 0) but consensus durable -> still
    // existing-chain (must not reset to genesis formation).
    let crash_recovery = StartupDkgContext {
        last_execution_height: 10,
        last_consensus_finalized_height: 0,
        ..fresh
    };
    assert_eq!(
        genesis_formation_gate_decision(crash_recovery, genesis, 3, &evidence),
        GenesisFormationGate::ExistingChainJoin
    );
}

/// Restart recovery: distinguishing a benign "execution head leads the marshal
/// finalized tip" restart (an unfinalized in-flight head) from genuine archive
/// corruption. See `unfinalized_head_lead_is_recoverable` + the recover match arm.
#[cfg(test)]
mod restart_recovery {
    use super::*;

    #[test]
    fn benign_unfinalized_head_lead_is_recoverable() {
        // Steady state: head is exactly one block ahead of the finalized tip.
        assert!(unfinalized_head_lead_is_recoverable(70, 69));
        // A few blocks ahead during a finalization hiccup, up to the bound.
        assert!(unfinalized_head_lead_is_recoverable(
            69 + MAX_UNFINALIZED_HEAD_LEAD,
            69
        ));
    }

    #[test]
    fn recovery_anchor_never_promotes_an_execution_only_head_to_finalized() {
        assert_eq!(durable_recovery_anchor_height(70, 69), 69);
        assert_eq!(durable_recovery_anchor_height(69, 69), 69);
        assert_eq!(durable_recovery_anchor_height(68, 69), 68);
        assert_eq!(durable_recovery_anchor_height(0, 0), 0);
    }

    #[test]
    fn no_lead_is_not_a_recovery_case() {
        // head == finalized: recover(head) would have succeeded; not this arm.
        assert!(!unfinalized_head_lead_is_recoverable(69, 69));
        // head behind finalized (execution lags): saturating lead is 0.
        assert!(!unfinalized_head_lead_is_recoverable(68, 69));
    }

    #[test]
    fn zero_finalized_tip_is_not_recoverable() {
        // No durable finalized tip at all → fresh/corrupt, never the benign case.
        assert!(!unfinalized_head_lead_is_recoverable(5, 0));
    }

    #[test]
    fn lead_beyond_bound_stays_fatal() {
        // A head far ahead of the finalized tip is suspicious, not an in-flight
        // head — it must NOT be silently tolerated.
        assert!(!unfinalized_head_lead_is_recoverable(
            69 + MAX_UNFINALIZED_HEAD_LEAD + 1,
            69
        ));
    }

    #[test]
    fn bounded_head_lead_membership_drift_uses_recovered_boundary_committee() {
        use commonware_cryptography::Signer as _;
        use std::net::SocketAddr;

        let marshal_finalized_height = 100;
        let reth_head = marshal_finalized_height + MAX_UNFINALIZED_HEAD_LEAD;
        assert!(
            unfinalized_head_lead_is_recoverable(reth_head, marshal_finalized_height),
            "bounded Reth head lead should be treated as the benign restart window"
        );

        let temp = tempfile::tempdir().unwrap();
        let evm_key_path = temp.path().join("evm-key.hex");
        let evm_secret = [0x52u8; 32];
        std::fs::write(&evm_key_path, hex::encode(evm_secret)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&evm_key_path, std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let evm_signer =
            outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes(evm_secret).unwrap();

        let (keys, _participants, output, polynomial) = run_test_dkg();
        let local_key = &keys[0];
        let boundary_addresses = vec![
            evm_signer.address(),
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
        ];
        let boundary_validator_set = validators::ValidatorSet {
            public_keys: keys.iter().map(|key| key.public_key()).collect(),
            addresses: boundary_addresses.clone(),
            p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
        };
        let recovered_boundary =
            dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
                epoch: Epoch::new(7),
                validator_set: &boundary_validator_set,
                output: &output,
                is_full_dkg: false,
                dkg_cycle: 6,
                freeze_height: 10,
                planned_activation_height: 20,
                vrf_material_version: 2,
                is_validator_set_change: true,
                tee_reshare_registrations: Vec::new(),
            })
            .unwrap();
        let boundary_participants =
            select_recovery_participants(output.players(), &recovered_boundary).unwrap();
        assert_eq!(&boundary_participants, output.players());

        // Simulate provider-latest state after an unfinalized membership-changing
        // head: old participant A has been removed, and a new D is present.
        let replacement_key = bls12381::PrivateKey::from_seed(99);
        let latest_after_unfinalized_removal = validators::ValidatorSet {
            public_keys: vec![
                keys[1].public_key(),
                keys[2].public_key(),
                replacement_key.public_key(),
            ],
            addresses: vec![
                Address::with_last_byte(0x22),
                Address::with_last_byte(0x33),
                Address::with_last_byte(0x44),
            ],
            p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
        };
        assert!(
            ordered_validator_addresses(&boundary_participants, &latest_after_unfinalized_removal)
                .is_err(),
            "pre-fix provider-latest address mapping should fail when old A is absent"
        );

        let vrf_materials = VrfMaterialProvider::new(2, polynomial, None);
        let (_verifier_scheme, recovered_addresses) = epoch_validation_inputs(
            Epoch::new(7),
            &boundary_participants,
            &latest_after_unfinalized_removal,
            Some(&recovered_boundary),
            &vrf_materials,
        )
        .expect("bounded-head-lead recovery must use recovered boundary committee");
        assert_eq!(recovered_addresses, boundary_addresses);

        let args = crate::args::ConsensusArgs {
            is_validator: true,
            signing_key: Some(temp.path().join("signing-key.hex")),
            validator_evm_key: Some(evm_key_path),
            signing_share: None,
            public_polynomial: None,
            dkg_output: None,
            listen_address: "127.0.0.1:30400".parse::<SocketAddr>().unwrap(),
            storage_dir: None,
            keys_dir: None,
            trust_el_head: false,
            force_dkg: false,
            testnet_unix_time_offset_secs: None,
            consensus_peers: Vec::new(),
            use_local_defaults: true,
            payload_resolve_time_ms: 200,
            payload_return_time_ms: 450,
            worker_threads: 1,
            bls_key_backend: "plaintext".to_string(),
            bls_passphrase: None,
            tee_enclave_socket: None,
            tee_bootstrap_timeout_secs: 60,
            upstream: None,
            upstream_nocertify: false,
            projection_mongodb_uri: Some("mongodb://localhost:27017".to_owned()),
            projection_mongodb_database: Some("outbe_projection".to_owned()),
            projection_start_block: 1,
        };
        let signer_address = validate_validator_evm_signer(
            &args,
            local_key,
            &latest_after_unfinalized_removal,
            &latest_after_unfinalized_removal,
            Some((&boundary_participants, &recovered_boundary)),
            false,
        )
        .expect("old-epoch signer A should be authorized by recovered boundary, not latest state");
        assert_eq!(signer_address, Some(evm_signer.address()));
    }
}

// ---------------------------------------------------------------------------
// Block-timing genesis reader / validation (Phase 0/3 of min-block-time).
// ---------------------------------------------------------------------------

/// Test 8: absent genesis key falls back to the supplied default.
#[test]
fn read_ms_uses_default_when_absent() {
    assert_eq!(
        read_ms::<String>(None, "minBlockTimeMs", 2000).unwrap(),
        2000
    );
    assert_eq!(
        read_ms::<String>(None, "leaderTimeoutMs", 4000).unwrap(),
        4000
    );
    assert_eq!(
        read_ms::<String>(None, "certificationTimeoutMs", 8000).unwrap(),
        8000
    );
}

/// Test 9: a present value is returned verbatim (including 0 — the value is read
/// here; the `> 0` rule is enforced by `validate_timing`, see Test 11).
#[test]
fn read_ms_accepts_present_value() {
    assert_eq!(
        read_ms::<String>(Some(Ok(0)), "minBlockTimeMs", 2000).unwrap(),
        0
    );
    assert_eq!(
        read_ms::<String>(Some(Ok(1500)), "minBlockTimeMs", 2000).unwrap(),
        1500
    );
}

/// Test 10: a malformed value surfaces a structured error naming the key.
#[test]
fn read_ms_reports_malformed_value() {
    let err = read_ms(
        Some(Err("expected u64".to_string())),
        "minBlockTimeMs",
        2000,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("invalid genesis config minBlockTimeMs"),
        "error: {err}"
    );
}

/// Test 11: the startup invariants reject 0, min>=leader, and leader>cert.
#[test]
fn validate_timing_rejects_invalid_combinations() {
    let zero = validate_timing(0, 4000, 8000).unwrap_err().to_string();
    assert!(zero.contains("minBlockTimeMs"), "error: {zero}");
    assert!(validate_timing(4000, 4000, 8000).is_err()); // min == leader
    assert!(validate_timing(5000, 4000, 8000).is_err()); // min > leader
    assert!(validate_timing(2000, 9000, 8000).is_err()); // leader > cert
}

/// Test 12: the shipped defaults satisfy `0 < min < leader <= cert`.
#[test]
fn validate_timing_accepts_defaults() {
    assert!(validate_timing(2000, 4000, 8000).is_ok());
}
