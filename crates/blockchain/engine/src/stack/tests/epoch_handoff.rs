use super::*;

#[cfg(test)]
fn replay_finalized_dealer_logs_into_manager(
    provider: &impl HeaderProvider<Header = OutbeHeader>,
    next_scan_height: &mut u64,
    latest_height: u64,
    dkg_manager: &DkgManagerMailbox,
) -> Result<()> {
    while *next_scan_height <= latest_height {
        if let Some(header) = provider
            .sealed_header(*next_scan_height)
            .map_err(|error| eyre::eyre!("failed to read header {}: {error}", *next_scan_height))?
        {
            let artifacts = decode_outbe_block_artifacts(header.header().inner.extra_data.as_ref())
                .map_err(|error| {
                    eyre::eyre!(
                        "failed to decode header artifacts at {}: {error}",
                        *next_scan_height
                    )
                })?;
            if matches!(
                artifacts.consensus_header_artifact.as_ref(),
                Some(ConsensusHeaderArtifact::DealerLog(_))
            ) {
                dkg_manager.note_finalized_header_artifact_at(
                    *next_scan_height,
                    header.hash(),
                    artifacts.consensus_header_artifact.as_ref(),
                );
            }
        }
        *next_scan_height = next_scan_height.saturating_add(1);
    }
    Ok(())
}

#[allow(clippy::type_complexity)]
fn signed_dkg_logs(
    round: u64,
) -> (
    commonware_utils::ordered::Set<bls12381::PublicKey>,
    Vec<Bytes>,
) {
    use commonware_codec::Encode as _;
    use commonware_cryptography::bls12381::dkg::feldman_desmedt::{Dealer, Info, Player};
    use commonware_cryptography::bls12381::primitives::sharing::Mode;
    use commonware_utils::{N3f1, TryCollect as _};

    let mut keys: Vec<bls12381::PrivateKey> =
        (1..=4).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let participants: commonware_utils::ordered::Set<bls12381::PublicKey> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &config::outbe_app_namespace(),
        round,
        None,
        Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants.clone(),
    )
    .unwrap();

    let mut dealers = Vec::new();
    let mut public_messages = Vec::new();
    let mut private_messages = Vec::new();
    for key in &keys {
        let (dealer, public, private) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
            rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
            info.clone(),
            key.clone(),
            None,
        )
        .unwrap();
        dealers.push(dealer);
        public_messages.push(public);
        private_messages.push(private);
    }

    let mut players: Vec<Player<MinSig, bls12381::PrivateKey>> = keys
        .iter()
        .map(|key| Player::new(info.clone(), key.clone()).unwrap())
        .collect();
    for (dealer_index, (public, private)) in public_messages
        .iter()
        .zip(private_messages.iter())
        .enumerate()
    {
        let dealer = keys[dealer_index].public_key();
        for (player, share) in private {
            let player_index = keys
                .iter()
                .position(|key| key.public_key() == *player)
                .unwrap();
            if let Some(ack) = players[player_index]
                .dealer_message::<N3f1>(dealer.clone(), public.clone(), share.clone())
                .expect("fixture dealing must be valid")
            {
                dealers[dealer_index]
                    .receive_player_ack(player.clone(), ack)
                    .unwrap();
            }
        }
    }

    let mut encoded = BTreeMap::new();
    for dealer in dealers {
        let signed = dealer.finalize::<N3f1>();
        let (dealer, _) = signed.clone().check(&info).expect("valid dealer log");
        encoded.insert(dealer, Bytes::from(signed.encode()));
    }
    (participants, encoded.into_values().collect())
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
        commonware_cryptography::Sha256::hash(&[b"stack-test"]),
    );
    let subject = Subject::Notarize {
        proposal: &proposal,
    };
    let attestations: Vec<_> = schemes
        .iter()
        .map(|scheme| scheme.sign::<Sha256Digest>(subject).unwrap())
        .collect();

    schemes[0]
        .assemble(
            commonware_utils::iter::NonEmpty::try_new(attestations.into_iter()).unwrap(),
            &Sequential,
        )
        .unwrap()
}

#[derive(Clone, Default)]
struct MockFinalizedHeaderProvider {
    headers: BTreeMap<u64, SealedHeader<OutbeHeader>>,
    sealed_header_barrier: Option<(u64, Arc<Barrier>, Arc<Barrier>)>,
}

impl MockFinalizedHeaderProvider {
    fn insert(&mut self, number: u64, artifact: Option<ConsensusHeaderArtifact>) {
        let extra_data = outbe_primitives::reshare_artifact::encode_outbe_block_artifacts(
            &outbe_primitives::reshare_artifact::OutbeBlockArtifacts {
                consensus_header_artifact: artifact,
                ..Default::default()
            },
        )
        .unwrap();
        self.headers.insert(
            number,
            SealedHeader::seal_slow(OutbeHeader::new(Header {
                number,
                extra_data,
                ..Default::default()
            })),
        );
    }

    fn block_sealed_header_at(
        &mut self,
        number: u64,
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    ) {
        self.sealed_header_barrier = Some((number, entered, release));
    }

    fn without_sealed_header_barrier(&self) -> Self {
        Self {
            headers: self.headers.clone(),
            sealed_header_barrier: None,
        }
    }
}

impl BlockHashReader for MockFinalizedHeaderProvider {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        Ok(self.headers.get(&number).map(SealedHeader::hash))
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        Ok((start..end)
            .filter_map(|height| self.headers.get(&height).map(SealedHeader::hash))
            .collect())
    }
}

impl HeaderProvider for MockFinalizedHeaderProvider {
    type Header = OutbeHeader;

    fn header(&self, block_hash: B256) -> ProviderResult<Option<Self::Header>> {
        Ok(self
            .headers
            .values()
            .find(|header| header.hash() == block_hash)
            .map(|header| header.header().clone()))
    }

    fn header_by_number(&self, num: u64) -> ProviderResult<Option<Self::Header>> {
        Ok(self.headers.get(&num).map(|header| header.header().clone()))
    }

    fn headers_range(
        &self,
        _range: impl std::ops::RangeBounds<u64>,
    ) -> ProviderResult<Vec<Self::Header>> {
        Ok(Vec::new())
    }

    fn sealed_header(&self, number: u64) -> ProviderResult<Option<SealedHeader<Self::Header>>> {
        if let Some((blocked_number, entered, release)) = &self.sealed_header_barrier {
            if number == *blocked_number {
                entered.wait();
                release.wait();
            }
        }
        Ok(self.headers.get(&number).cloned())
    }

    fn sealed_headers_while(
        &self,
        _range: impl std::ops::RangeBounds<u64>,
        _predicate: impl FnMut(&SealedHeader<Self::Header>) -> bool,
    ) -> ProviderResult<Vec<SealedHeader<Self::Header>>> {
        Ok(Vec::new())
    }
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
fn test_pending_dkg_activation_blocks_duplicate_rotation_start() {
    assert!(
        !should_start_dkg_rotation(false, true, 99, 90),
        "pending DKG activation for a planned boundary must block duplicate rotation starts"
    );
}

#[test]
fn completed_dkg_waits_for_a_finalized_preannounce_carrier() {
    assert_eq!(
        pending_dkg_handoff_decision(250, 240, 30, None),
        PendingDkgHandoffDecision::Wait
    );
}

#[test]
fn pending_dkg_handoff_decision_covers_planned_height_and_deadline_edges() {
    assert_eq!(
        pending_dkg_handoff_decision(239, 240, 30, Some(230)),
        PendingDkgHandoffDecision::Wait
    );
    assert_eq!(
        pending_dkg_handoff_decision(240, 240, 30, Some(230)),
        PendingDkgHandoffDecision::Activate {
            activation_anchor: 240
        }
    );
    assert_eq!(
        pending_dkg_handoff_decision(270, 240, 30, Some(270)),
        PendingDkgHandoffDecision::Activate {
            activation_anchor: 270
        }
    );
    assert_eq!(
        pending_dkg_handoff_decision(270, 240, 30, None),
        PendingDkgHandoffDecision::Expired { deadline: 270 }
    );
    assert_eq!(
        pending_dkg_handoff_decision(271, 240, 30, Some(271)),
        PendingDkgHandoffDecision::Expired { deadline: 270 }
    );
}

#[test]
fn startup_pending_dkg_epoch_plan_keeps_future_epoch_separate_before_activation() {
    let current_epoch = Epoch::new(0);
    let pending_epoch = Epoch::new(1);

    assert_eq!(
        startup_pending_dkg_epoch_plan(current_epoch, pending_epoch, 299, 300, 30, Some(275))
            .unwrap(),
        StartupPendingDkgEpochPlan::Defer {
            active_epoch: current_epoch,
            preregister_after_current: pending_epoch,
        }
    );
}

#[test]
fn startup_pending_dkg_epoch_plan_restores_activated_epoch_before_boundary_commit() {
    let previous_epoch = Epoch::new(0);
    let pending_epoch = Epoch::new(1);

    assert_eq!(
        startup_pending_dkg_epoch_plan(previous_epoch, pending_epoch, 300, 300, 30, Some(275))
            .unwrap(),
        StartupPendingDkgEpochPlan::Activate {
            previous_epoch,
            active_epoch: pending_epoch,
            activation_anchor: 300,
        }
    );
}

#[test]
fn startup_pending_dkg_epoch_plan_fails_closed_on_invalid_or_expired_handoff() {
    let current_epoch = Epoch::new(4);

    let wrong_epoch =
        startup_pending_dkg_epoch_plan(current_epoch, Epoch::new(6), 500, 500, 30, Some(480))
            .unwrap_err()
            .to_string();
    assert!(wrong_epoch.contains("does not follow active epoch"));

    let expired = startup_pending_dkg_epoch_plan(current_epoch, Epoch::new(5), 530, 500, 30, None)
        .unwrap_err()
        .to_string();
    assert!(expired.contains("missed activation deadline 530"));
}

#[test]
fn deferred_startup_pending_dkg_reserves_the_following_cycle() {
    assert_eq!(next_dkg_cycle_after_restored_target(2, 2), 3);
    assert_eq!(next_dkg_cycle_after_restored_target(5, 2), 5);
    assert_eq!(
        next_dkg_cycle_after_restored_target(u64::MAX, u64::MAX),
        u64::MAX
    );
}

#[test]
fn restored_pending_output_survives_until_deferred_activation() {
    let (_keys, _participants, recovered, _share, _polynomial) = run_test_dkg_complete();
    assert_eq!(
        select_pending_canonical_output(None, Some(&recovered)),
        Some(recovered.clone()),
        "restart loses the process-local DKG manager ceremony, so the already validated durable output must remain available"
    );

    let (_keys, _participants, finalized, _share, _polynomial) = run_test_dkg_complete();
    assert_eq!(
        select_pending_canonical_output(Some(finalized.clone()), Some(&recovered)),
        Some(finalized),
        "a live finalized-log reconstruction remains authoritative when present"
    );
}

#[test]
fn preannounce_carrier_must_match_the_pending_epoch_and_outcome_exactly() {
    let pending = test_boundary_with_vrf_hash(B256::with_last_byte(0x55), 9);
    let exact = ConsensusHeaderArtifact::CommitteePreAnnounce {
        epoch: pending.epoch,
        outcome: pending.outcome.clone(),
    };
    let wrong_epoch = ConsensusHeaderArtifact::CommitteePreAnnounce {
        epoch: pending.epoch.saturating_add(1),
        outcome: pending.outcome.clone(),
    };
    let wrong_outcome = ConsensusHeaderArtifact::CommitteePreAnnounce {
        epoch: pending.epoch,
        outcome: Bytes::from_static(b"wrong"),
    };

    assert!(preannounce_matches_pending(&exact, &pending));
    assert!(!preannounce_matches_pending(&wrong_epoch, &pending));
    assert!(!preannounce_matches_pending(&wrong_outcome, &pending));
}

#[test]
fn finalized_preannounce_scan_uses_the_first_exact_canonical_carrier() {
    let pending = test_boundary_with_vrf_hash(B256::with_last_byte(0x55), 9);
    let mut wrong = pending.clone();
    wrong.outcome = Bytes::from_static(b"wrong");
    let mut provider = MockFinalizedHeaderProvider::default();
    provider.insert(
        10,
        Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
            epoch: wrong.epoch,
            outcome: wrong.outcome,
        }),
    );
    provider.insert(
        11,
        Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
            epoch: pending.epoch,
            outcome: pending.outcome.clone(),
        }),
    );
    provider.insert(
        12,
        Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
            epoch: pending.epoch,
            outcome: pending.outcome.clone(),
        }),
    );

    assert_eq!(
        find_exact_finalized_preannounce_carrier(&provider, &pending, 10, 30).unwrap(),
        None
    );
    assert_eq!(
        find_exact_finalized_preannounce_carrier(&provider, &pending, 12, 30).unwrap(),
        Some(11)
    );
}

#[test]
fn finalized_preannounce_scan_fails_closed_on_a_finalized_provider_gap() {
    let pending = test_boundary_with_vrf_hash(B256::with_last_byte(0x55), 9);
    let provider = MockFinalizedHeaderProvider::default();

    let error = find_exact_finalized_preannounce_carrier(&provider, &pending, 10, 30)
        .unwrap_err()
        .to_string();
    assert!(error.contains("missing block hash at height 10"));
}

#[test]
fn dkg_retry_replays_to_verified_tip_not_stale_scheduling_height() {
    let round = 17;
    let (participants, logs) = signed_dkg_logs(round);
    assert_eq!(logs.len(), 4);

    let mut provider = MockFinalizedHeaderProvider::default();
    for height in 40..=96 {
        provider.insert(height, None);
    }
    provider.insert(
        97,
        Some(ConsensusHeaderArtifact::DealerLog(logs[0].clone())),
    );
    provider.insert(
        98,
        Some(ConsensusHeaderArtifact::DealerLog(logs[1].clone())),
    );
    provider.insert(
        99,
        Some(ConsensusHeaderArtifact::DealerLog(logs[2].clone())),
    );
    let tip_hash = provider.block_hash(98).unwrap().unwrap();
    let verified_tip = crate::marshal_update_reporter::ConsensusTip {
        round: Round::new(Epoch::new(0), View::new(98)),
        height: Height::new(98),
        digest: outbe_consensus::digest::Digest(tip_hash),
    };

    let retry = DkgManagerMailbox::new();
    retry
        .note_ceremony_started(Epoch::new(0), round, None, participants.clone())
        .unwrap();
    for height in [97, 98] {
        let header = provider.sealed_header(height).unwrap().unwrap();
        let artifact = decode_outbe_block_artifacts(header.header().inner.extra_data.as_ref())
            .unwrap()
            .consensus_header_artifact;
        retry.note_finalized_header_artifact_at(height, header.hash(), artifact.as_ref());
    }
    assert!(retry.canonical_output(Epoch::new(0)).is_none());

    restart_dkg_manager_from_finalized_history(
        &provider,
        &retry,
        DkgCeremonyReplaySpec {
            epoch: Epoch::new(0),
            round,
            previous_output: None,
            participants: participants.clone(),
            finalized_dealer_log_tx: None,
        },
        40,
        41,
        || verified_tip,
    )
    .unwrap();
    let header_99 = provider.sealed_header(99).unwrap().unwrap();
    let artifact_99 = decode_outbe_block_artifacts(header_99.header().inner.extra_data.as_ref())
        .unwrap()
        .consensus_header_artifact;
    retry.note_finalized_header_artifact_at(99, header_99.hash(), artifact_99.as_ref());

    let uninterrupted = DkgManagerMailbox::new();
    uninterrupted
        .note_ceremony_started(Epoch::new(0), round, None, participants)
        .unwrap();
    let mut next_height = 40;
    replay_finalized_dealer_logs_into_manager(&provider, &mut next_height, 99, &uninterrupted)
        .unwrap();
    let expected = uninterrupted
        .canonical_output(Epoch::new(0))
        .expect("the uninterrupted canonical prefix reaches threshold");

    assert_eq!(
        retry.canonical_output(Epoch::new(0)),
        Some(expected),
        "retry must rebuild through the verified canonical tip, not the stale queued scheduling height"
    );
}

#[test]
fn live_finalized_dkg_log_cannot_overtake_retry_replay_prefix() {
    let round = 18;
    let (participants, logs) = signed_dkg_logs(round);
    assert_eq!(logs.len(), 4);

    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let mut provider = MockFinalizedHeaderProvider::default();
    for height in 40..=96 {
        provider.insert(height, None);
    }
    for (height, log) in [
        (97, &logs[0]),
        (98, &logs[1]),
        (99, &logs[2]),
        (100, &logs[3]),
    ] {
        provider.insert(
            height,
            Some(ConsensusHeaderArtifact::DealerLog(log.clone())),
        );
    }
    provider.block_sealed_header_at(96, entered.clone(), release.clone());
    let control_provider = provider.without_sealed_header_barrier();
    let tip_hash = provider.block_hash(99).unwrap().unwrap();
    let verified_tip = crate::marshal_update_reporter::ConsensusTip {
        round: Round::new(Epoch::new(0), View::new(99)),
        height: Height::new(99),
        digest: outbe_consensus::digest::Digest(tip_hash),
    };

    let retry = DkgManagerMailbox::new();
    let retry_task = retry.clone();
    let retry_participants = participants.clone();
    let replay = std::thread::spawn(move || {
        restart_dkg_manager_from_finalized_history(
            &provider,
            &retry_task,
            DkgCeremonyReplaySpec {
                epoch: Epoch::new(0),
                round,
                previous_output: None,
                participants: retry_participants,
                finalized_dealer_log_tx: None,
            },
            40,
            41,
            || verified_tip,
        )
    });

    entered.wait();
    let live_header = control_provider.sealed_header(100).unwrap().unwrap();
    let live_artifact =
        decode_outbe_block_artifacts(live_header.header().inner.extra_data.as_ref())
            .unwrap()
            .consensus_header_artifact;
    let live_started = Arc::new(Barrier::new(2));
    let live_started_task = live_started.clone();
    let live_retry = retry.clone();
    let live_delivery = std::thread::spawn(move || {
        live_started_task.wait();
        live_retry.note_finalized_header_artifact_at(
            100,
            live_header.hash(),
            live_artifact.as_ref(),
        );
    });
    live_started.wait();
    release.wait();
    replay.join().unwrap().unwrap();
    live_delivery.join().unwrap();

    let uninterrupted = DkgManagerMailbox::new();
    uninterrupted
        .note_ceremony_started(Epoch::new(0), round, None, participants)
        .unwrap();
    let mut next_height = 40;
    replay_finalized_dealer_logs_into_manager(
        &control_provider,
        &mut next_height,
        100,
        &uninterrupted,
    )
    .unwrap();
    let expected = uninterrupted
        .canonical_output(Epoch::new(0))
        .expect("the uninterrupted canonical prefix reaches threshold");

    assert_eq!(
        retry.canonical_output(Epoch::new(0)),
        Some(expected),
        "a later live DealerLog must not overtake the canonical retry replay prefix"
    );
}

#[test]
fn dkg_recovery_provider_gap_preserves_existing_ceremony() {
    let round = 19;
    let (participants, logs) = signed_dkg_logs(round);
    assert_eq!(logs.len(), 4);

    let manager = DkgManagerMailbox::new();
    manager
        .note_ceremony_started(Epoch::new(0), round, None, participants.clone())
        .unwrap();
    for (height, bytes) in [(70, &logs[0]), (71, &logs[1]), (72, &logs[2])] {
        manager.note_finalized_header_artifact_at(
            height,
            B256::with_last_byte(height as u8),
            Some(&ConsensusHeaderArtifact::DealerLog(bytes.clone())),
        );
    }
    let expected = manager
        .canonical_output(Epoch::new(0))
        .expect("the existing ceremony has already frozen a canonical output");

    let mut provider = MockFinalizedHeaderProvider::default();
    for height in 40..=98 {
        if height != 80 {
            provider.insert(height, None);
        }
    }
    let tip_hash = provider.block_hash(98).unwrap().unwrap();
    let verified_tip = crate::marshal_update_reporter::ConsensusTip {
        round: Round::new(Epoch::new(0), View::new(98)),
        height: Height::new(98),
        digest: outbe_consensus::digest::Digest(tip_hash),
    };
    let (finalized_log_tx, mut finalized_log_rx) = tokio::sync::mpsc::unbounded_channel();

    let error = restart_dkg_manager_from_finalized_history(
        &provider,
        &manager,
        DkgCeremonyReplaySpec {
            epoch: Epoch::new(0),
            round,
            previous_output: None,
            participants,
            finalized_dealer_log_tx: Some(finalized_log_tx),
        },
        40,
        41,
        || verified_tip,
    )
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("missing finalized header at height 80"));
    assert_eq!(manager.canonical_output(Epoch::new(0)), Some(expected));
    assert!(
        finalized_log_rx.try_recv().is_err(),
        "a failed replay must not publish a partial DealerLog prefix to the actor"
    );
}

#[test]
fn dealer_only_handoff_requires_the_same_exact_finalized_carrier() {
    let boundary = test_boundary_with_vrf_hash(B256::with_last_byte(0x55), 9);
    let pending = DealerOnlyDkgActivation {
        target: FrozenDkgTarget {
            dkg_cycle: boundary.dkg_cycle,
            freeze_height: boundary.freeze_height,
            planned_activation_height: boundary.planned_activation_height,
            validator_set: validators::ValidatorSet {
                public_keys: Vec::new(),
                addresses: Vec::new(),
                p2p_addresses: Vec::new(),
            },
            participants: commonware_utils::ordered::Set::from_iter_dedup(
                Vec::<bls12381::PublicKey>::new(),
            ),
            tee_expired_target_exclusions: Vec::new(),
            is_validator_set_change: true,
        },
        boundary_artifact: Some(boundary.clone()),
        recovered_output: None,
    };
    let mut provider = MockFinalizedHeaderProvider::default();
    provider.insert(10, None);
    provider.insert(
        11,
        Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
            epoch: boundary.epoch,
            outcome: Bytes::from_static(b"wrong"),
        }),
    );
    provider.insert(
        12,
        Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
            epoch: boundary.epoch,
            outcome: boundary.outcome.clone(),
        }),
    );

    let published = pending
        .boundary_artifact
        .as_ref()
        .expect("dealer-only completion must publish the public boundary without a private share");
    assert_eq!(
        find_exact_finalized_preannounce_carrier(&provider, published, 11, 30).unwrap(),
        None
    );
    let carrier = find_exact_finalized_preannounce_carrier(&provider, published, 20, 30).unwrap();
    assert_eq!(carrier, Some(12));
    assert_eq!(
        pending_dkg_handoff_decision(20, pending.target.planned_activation_height, 30, carrier,),
        PendingDkgHandoffDecision::Activate {
            activation_anchor: 20,
        }
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
fn active_vrf_material_and_local_share_status_change_together() {
    let (keys, participants, _output, share, polynomial) = run_test_dkg_complete();
    let bridge = outbe_primitives::consensus::ConsensusExecutionBridge::new();
    bridge.set_consensus_status(outbe_primitives::consensus::ConsensusStatus {
        randomness_status: outbe_primitives::consensus::RandomnessStatus::Healthy,
        ..Default::default()
    });
    let vrf_materials = VrfMaterialProvider::new(0, polynomial.clone(), None);
    assert!(!bridge.has_threshold_shares());

    activate_vrf_material_and_publish_local_share(
        &bridge,
        &vrf_materials,
        1,
        polynomial.clone(),
        Some(share),
    );
    assert!(bridge.has_threshold_shares());
    assert!(HybridScheme::<MinSig>::signer_with_vrf_provider(
        &config::outbe_app_namespace(),
        participants.clone(),
        keys[0].clone(),
        vrf_materials.clone(),
    )
    .is_some());

    activate_vrf_material_and_publish_local_share(&bridge, &vrf_materials, 2, polynomial, None);
    assert!(!bridge.has_threshold_shares());
    assert!(HybridScheme::<MinSig>::signer_with_vrf_provider(
        &config::outbe_app_namespace(),
        participants,
        keys[0].clone(),
        vrf_materials,
    )
    .is_none());
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
    let seed = certificate.raw_vrf_seed_bytes();
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

// =============================================================================
// T3 - ordered::Set index shift on prefix-sort join (must pass).
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
// T1 / T2a / T2b / T5 - multi-node simplex deterministic harness.
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
        // Epoch::new(2) -> RoundRobin leader = (2+1) % 3 = 0; arbitrary
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
        // 150-500ms window has no follower route -> Mux drop -> stall.
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
        // `register_epoch_subchannels` at modeled DKG completion -
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
