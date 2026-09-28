//! Header-artifact admission tests: which consensus header artifact a
//! proposer emits and a verifier admits for a given parent and round.

use super::*;
use crate::test_fixtures::{
    block_with_header_artifact, dkg_runtime_artifacts, validator_set_from_keys, TestAncestryReader,
};

fn boundary(
    epoch: u64,
    validator_set: &ValidatorSet,
    output: &Output<MinSig, bls12381::PublicKey>,
) -> DkgBoundaryArtifact {
    build_boundary_artifact(BoundaryArtifactInput {
        epoch: Epoch::new(epoch),
        validator_set,
        output,
        is_full_dkg: true,
        dkg_cycle: epoch,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: epoch,
        is_validator_set_change: true,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap()
}

fn preannounce(epoch: u64, outcome: Bytes) -> ConsensusHeaderArtifact {
    ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, outcome }
}

#[tokio::test]
async fn preannounce_is_admitted_only_for_the_local_successor_outcome() {
    let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
    let validator_set = validator_set_from_keys(&keys);
    let next = boundary(1, &validator_set, &output);
    let ancestry = TestAncestryReader::ready();
    let round_epoch = Epoch::new(0);
    let admit = |manager: Mailbox, carried: ConsensusHeaderArtifact| {
        let ancestry = ancestry.clone();
        async move {
            manager
                .admit_header_artifact(None, round_epoch, Some(&carried), &ancestry)
                .await
        }
    };

    let fresh = Mailbox::new();
    assert!(
        admit(fresh, preannounce(1, next.outcome.clone()))
            .await
            .is_err(),
        "fail-closed: no local successor boundary to compare against"
    );

    let manager = Mailbox::new();
    manager.note_ceremony_completed(next.clone());
    assert_eq!(
        admit(manager.clone(), preannounce(1, next.outcome.clone())).await,
        Ok(BoundaryRequirement::NoPending)
    );
    assert!(
        admit(
            manager.clone(),
            preannounce(1, Bytes::from_static(b"forged-dkg-outcome"))
        )
        .await
        .is_err(),
        "an outcome that differs from the local DKG output must be rejected"
    );
    assert!(
        admit(manager, preannounce(2, next.outcome.clone()))
            .await
            .is_err(),
        "a pre-announce must name the round's direct successor epoch"
    );
}

#[tokio::test]
async fn pending_next_epoch_artifact_is_distinct_from_current_activation_boundary() {
    let (keys, _participants, output, _polynomial, _dealer_log) = dkg_runtime_artifacts();
    let validator_set = validator_set_from_keys(&keys);
    let next_epoch_artifact = boundary(1, &validator_set, &output);

    let manager = Mailbox::new();
    manager.note_ceremony_completed(next_epoch_artifact.clone());

    assert!(
        manager
            .pending_boundary_artifact(Epoch::new(0))
            .await
            .is_none(),
        "an epoch-1 artifact must not be exposed as the epoch-0 activation boundary"
    );
    assert_eq!(
        manager.pending_next_epoch_artifact(Epoch::new(0)),
        Some(next_epoch_artifact),
        "an epoch-1 artifact must be available for pre-announcement during epoch 0"
    );
    assert!(
        manager.pending_next_epoch_artifact(Epoch::new(1)).is_none(),
        "the same artifact must not be reused as an epoch-2 pre-announcement"
    );
    assert!(
        manager
            .pending_next_epoch_artifact(Epoch::new(u64::MAX))
            .is_none(),
        "epoch overflow must fail closed"
    );
}

/// One local DKG state a node can be in when it proposes or verifies a block of
/// round epoch 0: the manager, the parent the block builds on, and the boundary
/// requirement that state resolves to.
struct AdmissionState {
    name: &'static str,
    manager: Mailbox,
    parent: Option<crate::block::ConsensusBlock>,
    requirement: BoundaryRequirement,
}

struct Fixture {
    current: DkgBoundaryArtifact,
    next: DkgBoundaryArtifact,
    dealer_log: Bytes,
    states: Vec<AdmissionState>,
}

fn fixture() -> Fixture {
    let (keys, participants, output, _polynomial, dealer_log) = dkg_runtime_artifacts();
    let validator_set = validator_set_from_keys(&keys);
    let current = boundary(0, &validator_set, &output);
    let next = boundary(1, &validator_set, &output);
    let committed_parent =
        block_with_header_artifact(&ConsensusHeaderArtifact::BoundaryOutcome(current.clone()));

    let must_emit = Mailbox::new();
    must_emit.note_bootstrap_outcome(current.clone());
    let committed = Mailbox::new();
    committed.note_bootstrap_outcome(current.clone());
    committed
        .note_ceremony_started(Epoch::new(0), 7, None, participants.clone())
        .unwrap();
    let successor = Mailbox::new();
    successor.note_ceremony_completed(next.clone());
    let ceremony = Mailbox::new();
    ceremony
        .note_ceremony_started(Epoch::new(0), 7, None, participants)
        .unwrap();

    let state = |name, manager, parent, requirement| AdmissionState {
        name,
        manager,
        parent,
        requirement,
    };
    Fixture {
        states: vec![
            state("must-emit", must_emit, None, BoundaryRequirement::MustEmit),
            state(
                "already-committed",
                committed,
                Some(committed_parent),
                BoundaryRequirement::AlreadyCommitted,
            ),
            state(
                "successor-pending",
                successor,
                None,
                BoundaryRequirement::NoPending,
            ),
            state(
                "ceremony-running",
                ceremony,
                None,
                BoundaryRequirement::NoPending,
            ),
            state(
                "empty",
                Mailbox::new(),
                None,
                BoundaryRequirement::NoPending,
            ),
        ],
        current,
        next,
        dealer_log,
    }
}

/// Every row of the admission table: (state, carried artifact) -> admitted?
#[tokio::test]
async fn admission_table_is_enforced_for_every_state() {
    let Fixture {
        current,
        next,
        dealer_log,
        states,
    } = fixture();
    let ancestry = TestAncestryReader::ready();
    let carried: Vec<(&str, Option<ConsensusHeaderArtifact>)> = vec![
        ("none", None),
        (
            "current-boundary",
            Some(ConsensusHeaderArtifact::BoundaryOutcome(current.clone())),
        ),
        (
            "next-boundary",
            Some(ConsensusHeaderArtifact::BoundaryOutcome(next.clone())),
        ),
        (
            "dealer-log",
            Some(ConsensusHeaderArtifact::DealerLog(dealer_log)),
        ),
        (
            "successor-preannounce",
            Some(preannounce(1, next.outcome.clone())),
        ),
        (
            "forged-preannounce",
            Some(preannounce(
                1,
                encode_outcome(Epoch::new(1), &Output::clone(&decoded(&current)), false),
            )),
        ),
        (
            "current-epoch-preannounce",
            Some(preannounce(0, current.outcome.clone())),
        ),
    ];
    // (state, carried) pairs that must be admitted; every other pair is rejected.
    let admitted = [
        ("must-emit", "current-boundary"),
        ("already-committed", "none"),
        ("already-committed", "dealer-log"),
        ("successor-pending", "none"),
        ("successor-pending", "successor-preannounce"),
        ("ceremony-running", "none"),
        ("ceremony-running", "dealer-log"),
        ("empty", "none"),
    ];

    for state in &states {
        for (carried_name, artifact) in &carried {
            let verdict = state
                .manager
                .admit_header_artifact(
                    state.parent.as_ref(),
                    Epoch::new(0),
                    artifact.as_ref(),
                    &ancestry,
                )
                .await;
            let expect_admitted = admitted.contains(&(state.name, *carried_name));
            match verdict {
                Ok(requirement) => {
                    assert!(
                        expect_admitted,
                        "{} admitted {carried_name} but the table rejects it",
                        state.name
                    );
                    assert_eq!(requirement, state.requirement, "{}", state.name);
                }
                Err(error) => {
                    assert!(
                        !expect_admitted,
                        "{} rejected {carried_name}: {error}",
                        state.name
                    );
                    assert!(
                        !error.is_unavailable(),
                        "{} {carried_name}: a table rejection is not an ancestry outage",
                        state.name
                    );
                }
            }
        }
    }
}

/// Whatever a proposer plans in a given local state, a verifier in the same
/// state admits: the two paths cannot drift because they share one table.
#[tokio::test]
async fn verifier_admits_what_the_proposer_plans_in_the_same_state() {
    let Fixture { states, .. } = fixture();
    let ancestry = TestAncestryReader::ready();

    for state in &states {
        let plan = state
            .manager
            .plan_header_artifact(state.parent.as_ref(), Epoch::new(0), 2, &ancestry)
            .await
            .unwrap();
        assert_eq!(plan.requirement, state.requirement, "{}", state.name);
        let artifact = plan.artifact;
        assert_eq!(
            state
                .manager
                .admit_header_artifact(
                    state.parent.as_ref(),
                    Epoch::new(0),
                    artifact.as_ref(),
                    &ancestry
                )
                .await,
            Ok(state.requirement),
            "{}: planned {artifact:?} must be admitted",
            state.name
        );
    }
}

#[tokio::test]
async fn proposer_forfeits_block_one_without_genesis_boundary() {
    let forfeit = Mailbox::new()
        .plan_header_artifact(None, Epoch::new(0), 1, &TestAncestryReader::ready())
        .await
        .unwrap_err();
    assert_eq!(forfeit, ProposalForfeit::GenesisBoundaryNotReady);
}

#[tokio::test]
async fn ancestry_outage_is_reported_as_unavailable() {
    let Fixture { current, .. } = fixture();
    let manager = Mailbox::new();
    manager.note_bootstrap_outcome(current);
    let parent = crate::test_fixtures::block_with_number(5);

    let error = manager
        .admit_header_artifact(
            Some(&parent),
            Epoch::new(0),
            None,
            &TestAncestryReader::not_ready(),
        )
        .await
        .unwrap_err();

    assert!(error.is_unavailable(), "{error}");
}

fn decoded(artifact: &DkgBoundaryArtifact) -> Output<MinSig, bls12381::PublicKey> {
    decode_boundary_outcome(artifact.outcome.as_ref()).unwrap()
}
