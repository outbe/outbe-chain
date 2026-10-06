use super::*;

struct FinalizedBlock {
    round: Round,
    block: ConsensusBlock,
    finalization: Finalization<HybridScheme<MinSig>, Digest>,
}

fn finalized_pair(
    fixture: &FinalizationMetadataContext,
    tags: (u8, u8),
) -> (
    FinalizedBlock,
    FinalizedBlock,
    CertifiedParentAccountingMetadata,
) {
    let epoch = Epoch::new(0);
    let previous_round = Round::new(epoch, View::new(5));
    let previous_block = consensus_block_with_number(tags.0, 4);
    let (_, previous_finalization) = finalization_metadata_from_context(
        &previous_block,
        previous_round,
        View::new(4),
        FinalizationSigningContext::new(
            &fixture.signers,
            &fixture.verifier,
            fixture.committee.clone(),
        ),
    );
    let current_round = Round::new(epoch, View::new(8));
    let current_block = consensus_block_with_number(tags.1, 5);
    let (metadata, current_finalization) = finalization_metadata_from_context(
        &current_block,
        current_round,
        View::new(5),
        FinalizationSigningContext::new(
            &fixture.signers,
            &fixture.verifier,
            fixture.committee.clone(),
        ),
    );
    (
        FinalizedBlock {
            round: previous_round,
            block: previous_block,
            finalization: previous_finalization,
        },
        FinalizedBlock {
            round: current_round,
            block: current_block,
            finalization: current_finalization,
        },
        metadata,
    )
}

async fn publish_pair(
    mailbox: &crate::marshal_types::MarshalMailbox,
    previous: FinalizedBlock,
    current: FinalizedBlock,
) {
    let _ = mailbox.verified(previous.round, previous.block).await;
    let mut reporter = mailbox.clone();
    // Reporter::report is synchronous. Preserve verified/finalization ordering.
    let _ = reporter.report(Activity::Finalization(previous.finalization));
    let _ = mailbox.verified(current.round, current.block).await;
    let _ = reporter.report(Activity::Finalization(current.finalization));
}

pub(super) async fn verify(
    context: commonware_runtime::deterministic::Context,
    tags: (u8, u8),
    missed_proposers: Vec<outbe_primitives::consensus_metadata::MissedProposerEvent>,
) -> Option<AttestationVerdict> {
    let epoch = Epoch::new(0);
    let fixture = finalization_metadata_context(epoch);
    let elector_provider = HybridElectorConfigProvider::<MinSig>::new();
    let _ = elector_provider.register(epoch, HybridRandom::default());
    let clock = context.child("verify");
    let (previous, current, mut metadata) = finalized_pair(&fixture, tags);
    let previous_digest = previous.block.digest();
    let current_digest = current.block.digest();
    metadata.missed_proposers = missed_proposers;
    let (mailbox, resolver_keepalive, actor) = start_marshal_with_resolver(
        context,
        fixture.scheme_provider.clone(),
        EmptyMarshalBuffer::default(),
    )
    .await;
    publish_pair(&mailbox, previous, current).await;
    let current_info = wait_for_marshal_info(&clock, &mailbox, current_digest).await;
    let previous_info = wait_for_marshal_info(&clock, &mailbox, previous_digest).await;
    let verdict = if current_info.is_some() && previous_info.is_some() {
        Some(
            metadata_verify_verdict(
                &clock,
                &metadata,
                &AttestationValidationContext {
                    certificate_scheme_provider: &fixture.scheme_provider,
                    elector_config_provider: &elector_provider,
                    committee_provider: &fixture.committee_provider,
                    marshal_mailbox: &mailbox,
                    proposed_block_number: 6,
                },
            )
            .await,
        )
    } else {
        None
    };
    drop(resolver_keepalive);
    actor.abort();
    let _ = actor.await;
    verdict
}

pub(super) fn check_verdict(
    tags: (u8, u8),
    validators: [u8; 2],
    is_expected: impl FnOnce(AttestationVerdict) -> bool + Send + 'static,
) -> bool {
    // Deterministic runtime keeps marshal teardown within the scenario.
    commonware_runtime::deterministic::Runner::timed(Duration::from_secs(30)).start(
        |context| async move {
            let missed_proposers = vec![
                outbe_primitives::consensus_metadata::MissedProposerEvent {
                    view: 1,
                    validator: Address::with_last_byte(validators[0]),
                },
                outbe_primitives::consensus_metadata::MissedProposerEvent {
                    view: 2,
                    validator: Address::with_last_byte(validators[1]),
                },
            ];
            verify(context, tags, missed_proposers)
                .await
                .is_some_and(is_expected)
        },
    )
}
