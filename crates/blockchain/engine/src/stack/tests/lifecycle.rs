//! `UpstreamBootstrap -> Verifier -> Signer` lifecycle seams.
use super::*;
use crate::stack::epoch::marshal_recovery::validator_marshal_epocher;
use crate::stack::epoch::supervisor::future_epoch_hint_height;
use crate::stack::follower::{bootstrap_ready_from_facts, BootstrapFacts};
use commonware_consensus::types::Epocher as _;

const OFFER: B256 = B256::repeat_byte(0x0f);

fn ready_facts() -> BootstrapFacts {
    BootstrapFacts {
        finalized_height: 120,
        local_key_in_validator_set: true,
        finalized_boundary_present: true,
        on_chain_offer: OFFER,
        resident_offer: Some(OFFER),
        tee_lease_armed: true,
    }
}

fn rotation(length: u64, grace: u64) -> DkgRotationParams {
    DkgRotationParams {
        epoch_length_blocks: length,
        prepare_window_blocks: 10,
        activation_grace_blocks: grace,
    }
}

#[test]
fn bootstrap_ready_requires_every_fact_above_genesis() {
    assert!(bootstrap_ready_from_facts(ready_facts()));
    for broken in [
        BootstrapFacts {
            local_key_in_validator_set: false,
            ..ready_facts()
        },
        BootstrapFacts {
            finalized_boundary_present: false,
            ..ready_facts()
        },
        BootstrapFacts {
            on_chain_offer: B256::ZERO,
            resident_offer: Some(B256::ZERO),
            ..ready_facts()
        },
        BootstrapFacts {
            resident_offer: None,
            ..ready_facts()
        },
        BootstrapFacts {
            resident_offer: Some(B256::repeat_byte(0x01)),
            ..ready_facts()
        },
        BootstrapFacts {
            tee_lease_armed: false,
            ..ready_facts()
        },
    ] {
        assert!(!bootstrap_ready_from_facts(broken), "{broken:?}");
    }
}

#[test]
fn bootstrap_ready_at_genesis_requires_only_genesis_membership() {
    let genesis = BootstrapFacts {
        finalized_height: 0,
        local_key_in_validator_set: true,
        finalized_boundary_present: false,
        on_chain_offer: B256::ZERO,
        resident_offer: None,
        tee_lease_armed: false,
    };
    assert!(bootstrap_ready_from_facts(genesis));
    assert!(!bootstrap_ready_from_facts(BootstrapFacts {
        local_key_in_validator_set: false,
        ..genesis
    }));
}

#[test]
fn validator_marshal_epocher_recovers_from_the_latest_db_boundary() {
    // Epoch 3 activated at carrier 181 (L = 60, no grace delay).
    let epocher = validator_marshal_epocher(Some((181, 3)), rotation(60, 5));
    let epoch_of = |height: u64| {
        epocher
            .containing(Height::new(height))
            .map(|info| info.epoch().get())
    };
    assert_eq!(
        epocher.activation_height(Epoch::new(3)),
        Some(Height::new(181))
    );
    assert_eq!(epoch_of(181), Some(3));
    assert_eq!(epoch_of(246), Some(3), "open epoch extends to L + grace");
    // Heights below the anchor stay unknown (recorded decision).
    assert_eq!(epoch_of(180), None);

    // A grace-delayed boundary observed on activation closes epoch 3 exactly.
    epocher
        .observe_boundary(Epoch::new(4), Height::new(243))
        .unwrap();
    assert_eq!(epoch_of(242), Some(3));
    assert_eq!(epoch_of(243), Some(4));
    // The finalization path and the supervisor may both record it.
    epocher
        .observe_boundary(Epoch::new(4), Height::new(243))
        .unwrap();
    assert!(epocher
        .observe_boundary(Epoch::new(4), Height::new(244))
        .is_err());
}

#[test]
fn validator_marshal_epocher_starts_at_genesis_for_epoch_zero() {
    for boundary in [None, Some((1, 0))] {
        let epocher = validator_marshal_epocher(boundary, rotation(60, 0));
        assert_eq!(
            epocher.containing(Height::new(60)).unwrap().epoch(),
            Epoch::new(0),
            "block 60 is the last block of epoch 0"
        );
        epocher
            .observe_boundary(Epoch::new(1), Height::new(61))
            .unwrap();
        assert_eq!(
            epocher.containing(Height::new(61)).unwrap().epoch(),
            Epoch::new(1)
        );
    }
}

#[test]
fn future_epoch_vote_hints_last_guaranteed_block_of_open_epoch() {
    let epocher = validator_marshal_epocher(Some((181, 3)), rotation(60, 5));
    assert_eq!(
        future_epoch_hint_height(&epocher, Epoch::new(3), Epoch::new(4)),
        Some(Height::new(240))
    );
    assert_eq!(
        epocher
            .containing(Height::new(240))
            .expect("open-epoch hint must be contained in the open epoch")
            .epoch(),
        Epoch::new(3)
    );
    assert_eq!(
        future_epoch_hint_height(&epocher, Epoch::new(3), Epoch::new(9)),
        Some(Height::new(240))
    );
    for stale in [Epoch::new(2), Epoch::new(3)] {
        assert_eq!(
            future_epoch_hint_height(&epocher, Epoch::new(3), stale),
            None
        );
    }
    // The epocher does not know an epoch below its anchor.
    assert_eq!(
        future_epoch_hint_height(&epocher, Epoch::new(2), Epoch::new(3)),
        None
    );

    // Once the boundary for epoch 4 is observed at 243, epoch 3 is closed at 242.
    epocher
        .observe_boundary(Epoch::new(4), Height::new(243))
        .unwrap();
    assert_eq!(
        future_epoch_hint_height(&epocher, Epoch::new(3), Epoch::new(4)),
        Some(Height::new(242))
    );

    // With grace_blocks = 0, next_boundary_window(3) is (241, 241), so the open-epoch
    // backup vote hint is 240 (earliest - 1).
    let zero_grace_epocher = validator_marshal_epocher(Some((181, 3)), rotation(60, 0));
    assert_eq!(
        future_epoch_hint_height(&zero_grace_epocher, Epoch::new(3), Epoch::new(4)),
        Some(Height::new(240))
    );
}

#[tokio::test]
async fn activated_epoch_boundary_carrier_is_always_activation_anchor_plus_one() {
    use outbe_consensus::dkg_manager::{
        AncestryReader, BlockLookupFuture, BoundaryArtifactInput, BoundaryRequirement,
    };
    use outbe_primitives::validators::ValidatorP2pAddress;

    #[derive(Clone, Copy)]
    struct ReadyAncestryReader;

    impl AncestryReader for ReadyAncestryReader {
        fn get_block_by_height<'a>(&'a self, _height: u64) -> BlockLookupFuture<'a> {
            Box::pin(async { None })
        }

        fn get_block_by_hash<'a>(&'a self, _hash: B256) -> BlockLookupFuture<'a> {
            Box::pin(async { None })
        }

        fn is_ready(&self) -> bool {
            true
        }
    }

    let (keys, _participants, output, _share, _polynomial) = run_test_dkg_complete();
    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: (0..keys.len())
            .map(|idx| Address::with_last_byte((idx + 1) as u8))
            .collect(),
        p2p_addresses: vec![ValidatorP2pAddress::Missing; keys.len()],
    };

    // Covers both on-time activation (anchor = 240) and grace-delayed activation (anchor = 243)
    // across player activation, dealer-only activation, and recovered pending handoff.
    for activation_anchor in [240_u64, 243_u64] {
        let previous_epoch = Epoch::new(3);
        let active_epoch = Epoch::new(4);
        let carrier_height = activation_anchor + 1;
        let boundary = dkg_manager::build_boundary_artifact(BoundaryArtifactInput {
            epoch: active_epoch,
            validator_set: &validator_set,
            output: &output,
            is_full_dkg: true,
            dkg_cycle: 4,
            freeze_height: 230,
            planned_activation_height: 240,
            vrf_material_version: 4,
            is_validator_set_change: true,
            tee_expired_target_exclusions: Vec::new(),
        })
        .unwrap();

        let manager = DkgManagerMailbox::new();
        manager.note_ceremony_completed(boundary.clone());
        let ancestry = ReadyAncestryReader;

        // While previous_epoch (3) is active up to activation_anchor, the proposer
        // emits CommitteePreAnnounce for epoch 4, never BoundaryOutcome(epoch 4).
        let pre_activation_plan = manager
            .plan_header_artifact(None, previous_epoch, activation_anchor, &ancestry)
            .await
            .unwrap();
        assert_eq!(
            pre_activation_plan.requirement,
            BoundaryRequirement::NoPending
        );
        assert_eq!(
            pre_activation_plan.artifact,
            Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
                epoch: active_epoch.get(),
                outcome: boundary.outcome.clone(),
            })
        );
        assert!(manager
            .admit_header_artifact(
                None,
                previous_epoch,
                Some(&ConsensusHeaderArtifact::BoundaryOutcome(boundary.clone())),
                &ancestry,
            )
            .await
            .is_err());

        // Activation / pending-recovery arms the fence at activation_anchor, advances
        // to active_epoch (4), and records carrier_height = activation_anchor + 1.
        let fence = ApplicationEpochFence::new(previous_epoch);
        fence.arm_activation_boundary(previous_epoch, activation_anchor);
        fence.advance_epoch(active_epoch);

        let epocher = validator_marshal_epocher(Some((181, 3)), rotation(60, 5));
        let observed_carrier = crate::stack::epoch::activation::observe_activated_epoch_boundary(
            &epocher,
            active_epoch,
            activation_anchor,
        )
        .unwrap();
        assert_eq!(observed_carrier, carrier_height);

        // The first block of active_epoch (4) is proposed and admitted at carrier_height
        // and MUST carry BoundaryOutcome(epoch 4).
        let carrier_plan = manager
            .plan_header_artifact(None, active_epoch, carrier_height, &ancestry)
            .await
            .unwrap();
        assert_eq!(carrier_plan.requirement, BoundaryRequirement::MustEmit);
        assert_eq!(
            carrier_plan.artifact,
            Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary.clone()))
        );
        assert_eq!(
            manager
                .admit_header_artifact(
                    None,
                    active_epoch,
                    carrier_plan.artifact.as_ref(),
                    &ancestry,
                )
                .await,
            Ok(BoundaryRequirement::MustEmit)
        );
        assert!(manager
            .admit_header_artifact(None, active_epoch, None, &ancestry)
            .await
            .is_err());

        assert_eq!(
            epocher.activation_height(active_epoch),
            Some(Height::new(carrier_height))
        );
        assert_eq!(
            epocher
                .containing(Height::new(activation_anchor))
                .unwrap()
                .epoch(),
            previous_epoch
        );
        assert_eq!(
            epocher
                .containing(Height::new(carrier_height))
                .unwrap()
                .epoch(),
            active_epoch
        );
        // FinalizationActor observing the finalized carrier block is idempotent.
        epocher
            .observe_boundary(active_epoch, Height::new(carrier_height))
            .unwrap();
    }
}

#[test]
fn operating_mode_tracks_effective_radicle_signer_authority() {
    use crate::stack::epoch::signer::radicle_signer_enabled;
    use outbe_primitives::consensus::ConsensusOperatingMode;

    for (gate, has_share, expected_mode) in [
        (
            RadicleVotingGate::SignerAllowed,
            true,
            ConsensusOperatingMode::Signer,
        ),
        (
            RadicleVotingGate::Verifier,
            true,
            ConsensusOperatingMode::Verifier,
        ),
        (
            RadicleVotingGate::SignerAllowed,
            false,
            ConsensusOperatingMode::Verifier,
        ),
        (
            RadicleVotingGate::Verifier,
            false,
            ConsensusOperatingMode::Verifier,
        ),
    ] {
        let radicle_signer = radicle_signer_enabled(gate, has_share).unwrap();
        let bridge = ConsensusExecutionBridge::new();
        bridge.set_operating_mode(
            crate::stack::epoch::simplex::effective_validator_operating_mode(radicle_signer),
        );
        assert_eq!(bridge.operating_mode(), expected_mode);
    }
}

#[tokio::test]
async fn wait_for_bootstrap_tee_lease_parks_when_sender_is_dropped() {
    use crate::stack::follower::{wait_for_bootstrap_tee_lease, ValidatorBootstrap};

    let (tx, rx) = tokio::sync::watch::channel(false);
    drop(tx);
    let mut readiness = ValidatorBootstrap {
        local_consensus_key: bls12381::PrivateKey::from_seed(1).public_key(),
        local_key_in_genesis_validator_set: false,
        dkg_rotation_params: rotation(60, 5),
        tee_lease_armed_rx: rx,
    };
    assert!(
        wait_for_bootstrap_tee_lease(Some(&mut readiness))
            .now_or_never()
            .is_none(),
        "dropped tee_lease_armed watch sender must park rather than busy-spin FinalityObserver"
    );
}
