use super::*;

const OUTSIDER: Address = address!("0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddead");

// `valid_metadata_with_supplemental_finalize_vote` and the
// V1 supplemental-finalize-vote tests below are retired. V2 contract
// uses the certificate's own signer bitmap as the sole participation
// input; there is no supplemental-vote bitmap-extension path to test.

fn valid_metadata() -> (
    CertifiedParentAccountingMetadata,
    HybridSchemeProvider<MinSig>,
    CommitteeProvider,
) {
    let (keys, participants) = participants();
    let dkg = crate::bls::bootstrap_dkg(3).expect("bootstrap dkg should succeed");
    let schemes: Vec<HybridScheme<MinSig>> = keys
        .iter()
        .map(|key| {
            let pk = bls12381::PublicKey::from(key.clone());
            let idx = participants.index(&pk).expect("participant index");
            HybridScheme::signer(
                &crate::config::outbe_app_namespace(),
                participants.clone(),
                key.clone(),
                dkg.polynomial.clone(),
                dkg.shares[idx.get() as usize].clone(),
            )
            .expect("signer scheme should build")
        })
        .collect();
    let verifier = HybridScheme::<MinSig>::verifier(
        &crate::config::outbe_app_namespace(),
        participants.clone(),
        dkg.polynomial,
    )
    .expect("verifier scheme should build");

    let proposal = Proposal::new(
        Round::new(Epoch::new(0), View::new(5)),
        View::new(4),
        Digest(B256::from_slice(
            Sha256::hash(&[b"handler-finalize"]).as_ref(),
        )),
    );
    let subject = Subject::Finalize {
        proposal: &proposal,
    };
    let attestations: Vec<_> = schemes
        .iter()
        .map(|scheme| {
            scheme
                .sign::<Digest>(subject)
                .expect("finalize attestation")
        })
        .collect();
    let certificate = verifier
        .assemble(
            commonware_utils::iter::NonEmpty::try_new(attestations.into_iter()).unwrap(),
            &Sequential,
        )
        .expect("certificate should assemble");

    let scheme_provider = HybridSchemeProvider::new();
    let committee_provider = CommitteeProvider::new();
    let committee = vec![V1, V2, V3];
    let _ = scheme_provider.register(Epoch::new(0), verifier);
    let _ = committee_provider.register(Epoch::new(0), committee.clone());

    let envelope = Finalization::<HybridScheme<MinSig>, Digest> {
        proposal: proposal.clone(),
        certificate: certificate.clone(),
    };

    (
        CertifiedParentAccountingMetadata {
            finalized_block_number: 5,
            finalized_block_hash: proposal.payload.0,
            finalized_epoch: 0,
            finalized_view: 5,
            parent_view: 4,
            ordered_committee: committee,
            signer_bitmap: build_signer_bitmap(&certificate, 3),
            proof: Bytes::from(envelope.encode()),
            ..Default::default()
        },
        scheme_provider,
        committee_provider,
    )
}

#[test]
fn metadata_is_optional_for_verify() {
    let scheme_provider = HybridSchemeProvider::<MinSig>::new();
    let committee_provider = CommitteeProvider::new();
    assert_eq!(
        validate_consensus_metadata(None, &scheme_provider, &committee_provider),
        AttestationVerdict::AcceptNone
    );
}

#[test]
fn valid_finalized_parent_certificate_is_accepted() {
    let (metadata, scheme_provider, committee_provider) = valid_metadata();
    assert_eq!(
        validate_consensus_metadata(Some(&metadata), &scheme_provider, &committee_provider),
        AttestationVerdict::AcceptValid
    );
}

#[test]
fn finalized_parent_sentinel_metadata_is_rejected_when_present() {
    let (mut metadata, scheme_provider, committee_provider) = valid_metadata();
    metadata.finalized_block_number = 0;
    assert_eq!(
        validate_consensus_metadata(Some(&metadata), &scheme_provider, &committee_provider),
        AttestationVerdict::RejectStructural
    );

    let (mut metadata, scheme_provider, committee_provider) = valid_metadata();
    metadata.finalized_block_hash = B256::ZERO;
    assert_eq!(
        validate_consensus_metadata(Some(&metadata), &scheme_provider, &committee_provider),
        AttestationVerdict::RejectStructural
    );
}

// `supplemental_finalize_vote_extends_signer_bitmap` and
// `supplemental_finalize_vote_is_required_for_extended_bitmap` were
// V1-only tests of the legacy `build_signer_bitmap_with_finalize_votes`
// reconciliation. Under V2 the certificate's own bitmap is authoritative;
// these tests are retired in lockstep with the helper they exercised.

#[test]
fn mismatched_ordered_committee_is_rejected() {
    let (mut metadata, scheme_provider, committee_provider) = valid_metadata();
    metadata.ordered_committee.swap(0, 1);
    assert_eq!(
        validate_consensus_metadata(Some(&metadata), &scheme_provider, &committee_provider),
        AttestationVerdict::RejectStructural
    );
}

#[test]
fn outsider_missed_proposer_is_rejected() {
    let (mut metadata, scheme_provider, committee_provider) = valid_metadata();
    metadata
        .missed_proposers
        .push(outbe_primitives::consensus_metadata::MissedProposerEvent {
            view: 1,
            validator: OUTSIDER,
        });
    assert_eq!(
        validate_consensus_metadata(Some(&metadata), &scheme_provider, &committee_provider),
        AttestationVerdict::RejectStructural
    );
}

#[test]
fn tampered_signer_bitmap_is_rejected() {
    let (mut metadata, scheme_provider, committee_provider) = valid_metadata();
    metadata.signer_bitmap[0] = 0;
    assert_eq!(
        validate_consensus_metadata(Some(&metadata), &scheme_provider, &committee_provider),
        AttestationVerdict::RejectCertificate
    );
}

#[tokio::test]
async fn boundary_header_artifact_must_match_dkg_manager() {
    let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
    let validator_set = validator_set_from_keys(&keys);
    let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(0),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: true,
        dkg_cycle: 0,
        freeze_height: 0,
        planned_activation_height: 0,
        vrf_material_version: 0,
        is_validator_set_change: true,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap();
    let (scheme_provider, committee_provider) =
        leader_binding_providers(Epoch::new(0), &validator_set);
    let manager = DkgManagerMailbox::new();
    manager.note_bootstrap_outcome(artifact.clone());
    let block = block_with_header_artifact(&ConsensusHeaderArtifact::BoundaryOutcome(artifact));
    let ancestry = TestAncestryReader::ready();

    assert!(validate_header_consensus_artifacts(
        &block,
        None,
        Round::new(Epoch::new(0), View::new(1)),
        &keys[0].public_key(),
        outbe_primitives::chain::CHAIN_ID,
        ValidatorRole::Signer,
        &scheme_provider,
        &committee_provider,
        &manager,
        &ancestry,
    )
    .await
    .is_ok());
}

#[tokio::test]
async fn pending_boundary_must_be_included_exactly() {
    let (keys, _participants, output, _polynomial, dealer_log) = dkg_runtime_artifacts();
    let validator_set = validator_set_from_keys(&keys);
    let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(0),
        validator_set: &validator_set,
        output: &output,
        is_full_dkg: true,
        dkg_cycle: 0,
        freeze_height: 0,
        planned_activation_height: 0,
        vrf_material_version: 0,
        is_validator_set_change: true,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap();
    let (scheme_provider, committee_provider) =
        leader_binding_providers(Epoch::new(0), &validator_set);
    let manager = DkgManagerMailbox::new();
    manager.note_bootstrap_outcome(artifact);
    let ancestry = TestAncestryReader::ready();

    let missing = validate_header_consensus_artifacts(
        &block_with_number(0),
        None,
        Round::new(Epoch::new(0), View::new(1)),
        &keys[0].public_key(),
        outbe_primitives::chain::CHAIN_ID,
        ValidatorRole::Signer,
        &scheme_provider,
        &committee_provider,
        &manager,
        &ancestry,
    )
    .await
    .unwrap_err();
    assert!(missing.contains("omitted pending DKG BoundaryOutcome"));

    let dealer_log_block =
        block_with_header_artifact(&ConsensusHeaderArtifact::DealerLog(dealer_log));
    let wrong_kind = validate_header_consensus_artifacts(
        &dealer_log_block,
        None,
        Round::new(Epoch::new(0), View::new(1)),
        &keys[0].public_key(),
        outbe_primitives::chain::CHAIN_ID,
        ValidatorRole::Signer,
        &scheme_provider,
        &committee_provider,
        &manager,
        &ancestry,
    )
    .await
    .unwrap_err();
    assert!(wrong_kind.contains("omitted pending DKG BoundaryOutcome"));
}

#[tokio::test]
async fn dealer_log_header_artifact_allows_foreign_valid_dealer() {
    let (keys, participants, _output, _polynomial, dealer_log) = dkg_runtime_artifacts();
    let validator_set = validator_set_from_keys(&keys);
    let (scheme_provider, committee_provider) =
        leader_binding_providers(Epoch::new(0), &validator_set);
    let manager = DkgManagerMailbox::new();
    manager
        .note_ceremony_started(Epoch::new(0), 7, None, participants)
        .unwrap();
    let block = block_with_header_artifact(&ConsensusHeaderArtifact::DealerLog(dealer_log));
    let ancestry = TestAncestryReader::ready();

    assert!(validate_header_consensus_artifacts(
        &block,
        None,
        Round::new(Epoch::new(0), View::new(2)),
        &keys[0].public_key(),
        outbe_primitives::chain::CHAIN_ID,
        ValidatorRole::Signer,
        &scheme_provider,
        &committee_provider,
        &manager,
        &ancestry,
    )
    .await
    .is_ok());
    assert!(validate_header_consensus_artifacts(
        &block,
        None,
        Round::new(Epoch::new(0), View::new(2)),
        &keys[1].public_key(),
        outbe_primitives::chain::CHAIN_ID,
        ValidatorRole::Signer,
        &scheme_provider,
        &committee_provider,
        &manager,
        &ancestry,
    )
    .await
    .is_ok());
}

#[tokio::test]
async fn dealer_log_header_artifact_rejects_wrong_ceremony() {
    let (keys, participants, _output, _polynomial, dealer_log) = dkg_runtime_artifacts();
    let validator_set = validator_set_from_keys(&keys);
    let (scheme_provider, committee_provider) =
        leader_binding_providers(Epoch::new(0), &validator_set);
    let manager = DkgManagerMailbox::new();
    manager
        .note_ceremony_started(Epoch::new(0), 8, None, participants)
        .unwrap();
    let block = block_with_header_artifact(&ConsensusHeaderArtifact::DealerLog(dealer_log));
    let ancestry = TestAncestryReader::ready();

    assert!(validate_header_consensus_artifacts(
        &block,
        None,
        Round::new(Epoch::new(0), View::new(2)),
        &keys[1].public_key(),
        outbe_primitives::chain::CHAIN_ID,
        ValidatorRole::Signer,
        &scheme_provider,
        &committee_provider,
        &manager,
        &ancestry,
    )
    .await
    .is_err());
}

// ---- Header-artifact admission regressions -------------------------------
//
// A parent that already carries the pending epoch boundary puts the verifier in
// `AlreadyCommitted`. Every artifact a block carries there must still pass the
// same admission rule as in `NoPending`; a pre-announce is only admissible for
// the direct successor epoch and only with this node's own reconstructed outcome.

fn boundary_for_epoch(
    epoch: u64,
    validator_set: &crate::validators::ValidatorSet,
    output: &commonware_cryptography::bls12381::dkg::feldman_desmedt::Output<
        MinSig,
        bls12381::PublicKey,
    >,
) -> outbe_primitives::consensus::DkgBoundaryArtifact {
    dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(epoch),
        validator_set,
        output,
        is_full_dkg: true,
        dkg_cycle: epoch,
        freeze_height: 0,
        planned_activation_height: 0,
        vrf_material_version: epoch,
        is_validator_set_change: true,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap()
}

async fn verify_carried(
    block: &crate::block::ConsensusBlock,
    parent: Option<&crate::block::ConsensusBlock>,
    round_epoch: u64,
    proposer: &bls12381::PublicKey,
    validator_set: &crate::validators::ValidatorSet,
    manager: &DkgManagerMailbox,
) -> Result<(), String> {
    let (scheme_provider, committee_provider) =
        leader_binding_providers(Epoch::new(round_epoch), validator_set);
    validate_header_consensus_artifacts(
        block,
        parent,
        Round::new(Epoch::new(round_epoch), View::new(3)),
        proposer,
        outbe_primitives::chain::CHAIN_ID,
        ValidatorRole::Signer,
        &scheme_provider,
        &committee_provider,
        manager,
        &TestAncestryReader::ready(),
    )
    .await
}

#[tokio::test]
async fn already_committed_rejects_forged_preannounce() {
    let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
    let validator_set = validator_set_from_keys(&keys);
    let pending = boundary_for_epoch(0, &validator_set, &output);
    let manager = DkgManagerMailbox::new();
    manager.note_bootstrap_outcome(pending.clone());
    let parent = block_with_header_artifact(&ConsensusHeaderArtifact::BoundaryOutcome(pending));
    let forged = ConsensusHeaderArtifact::CommitteePreAnnounce {
        epoch: 1,
        outcome: dkg_manager::encode_outcome(Epoch::new(1), &output, true),
    };

    let verdict = verify_carried(
        &block_with_header_artifact(&forged),
        Some(&parent),
        0,
        &keys[0].public_key(),
        &validator_set,
        &manager,
    )
    .await;

    assert!(
        verdict.is_err(),
        "a pre-announce this node cannot verify must not ride an AlreadyCommitted block"
    );
}

#[tokio::test]
async fn already_committed_verifies_dealer_log() {
    let (keys, participants, output, _polynomial, dealer_log) = dkg_runtime_artifacts();
    let validator_set = validator_set_from_keys(&keys);
    let pending = boundary_for_epoch(0, &validator_set, &output);
    let parent =
        block_with_header_artifact(&ConsensusHeaderArtifact::BoundaryOutcome(pending.clone()));
    let block = block_with_header_artifact(&ConsensusHeaderArtifact::DealerLog(dealer_log));

    let matching = DkgManagerMailbox::new();
    matching.note_bootstrap_outcome(pending.clone());
    matching
        .note_ceremony_started(Epoch::new(0), 7, None, participants.clone())
        .unwrap();
    assert!(verify_carried(
        &block,
        Some(&parent),
        0,
        &keys[0].public_key(),
        &validator_set,
        &matching
    )
    .await
    .is_ok());

    let other_ceremony = DkgManagerMailbox::new();
    other_ceremony.note_bootstrap_outcome(pending);
    other_ceremony
        .note_ceremony_started(Epoch::new(0), 8, None, participants)
        .unwrap();
    assert!(
        verify_carried(
            &block,
            Some(&parent),
            0,
            &keys[0].public_key(),
            &validator_set,
            &other_ceremony
        )
        .await
        .is_err(),
        "a dealer log for another ceremony must be rejected after the boundary committed"
    );
}

#[tokio::test]
async fn already_committed_rejects_duplicate_boundary_and_accepts_empty() {
    let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
    let validator_set = validator_set_from_keys(&keys);
    let pending = boundary_for_epoch(0, &validator_set, &output);
    let manager = DkgManagerMailbox::new();
    manager.note_bootstrap_outcome(pending.clone());
    let artifact = ConsensusHeaderArtifact::BoundaryOutcome(pending);
    let parent = block_with_header_artifact(&artifact);
    let proposer = keys[0].public_key();

    let duplicate = verify_carried(
        &block_with_header_artifact(&artifact),
        Some(&parent),
        0,
        &proposer,
        &validator_set,
        &manager,
    )
    .await
    .unwrap_err();
    assert!(duplicate.contains("duplicate DKG BoundaryOutcome"));

    assert!(verify_carried(
        &block_with_number(0),
        Some(&parent),
        0,
        &proposer,
        &validator_set,
        &manager
    )
    .await
    .is_ok());
}

#[tokio::test]
async fn no_pending_rejects_non_successor_preannounce() {
    let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
    let validator_set = validator_set_from_keys(&keys);
    // The local pending boundary is for epoch 2 while the round is epoch 0: the
    // carried bytes match it exactly, but epoch 2 is not the round's successor.
    let pending = boundary_for_epoch(2, &validator_set, &output);
    let manager = DkgManagerMailbox::new();
    manager.note_ceremony_completed(pending.clone());
    let preannounce = ConsensusHeaderArtifact::CommitteePreAnnounce {
        epoch: 2,
        outcome: pending.outcome,
    };

    let verdict = verify_carried(
        &block_with_header_artifact(&preannounce),
        None,
        0,
        &keys[0].public_key(),
        &validator_set,
        &manager,
    )
    .await;

    assert!(
        verdict.is_err(),
        "a pre-announce must name the round's direct successor epoch"
    );
}

#[tokio::test]
async fn no_pending_accepts_successor_preannounce() {
    let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
    let validator_set = validator_set_from_keys(&keys);
    let pending = boundary_for_epoch(1, &validator_set, &output);
    let manager = DkgManagerMailbox::new();
    manager.note_ceremony_completed(pending.clone());
    let preannounce = ConsensusHeaderArtifact::CommitteePreAnnounce {
        epoch: 1,
        outcome: pending.outcome,
    };

    assert!(verify_carried(
        &block_with_header_artifact(&preannounce),
        None,
        0,
        &keys[0].public_key(),
        &validator_set,
        &manager
    )
    .await
    .is_ok());
}

/// Block 1 without the genesis `BoundaryOutcome` is already rejected by the
/// system-tx set rule, before header-artifact admission runs; this pins that the
/// admission table needs no separate height-1 row.
#[tokio::test]
async fn height_one_without_boundary_is_rejected_by_system_tx_set() {
    let (keys, _participants, _output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
    let validator_set = validator_set_from_keys(&keys);
    let manager = DkgManagerMailbox::new();

    let verdict = verify_carried(
        &rewarded_block_with_number(1),
        None,
        0,
        &keys[0].public_key(),
        &validator_set,
        &manager,
    )
    .await;

    let error = verdict.unwrap_err();
    assert!(
        error.contains("block 1 must carry a BoundaryOutcome"),
        "unexpected rejection: {error}"
    );
}
