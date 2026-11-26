use super::super::*;

const FINALIZED_ROUND_RECOVERY_ATTEMPTS: usize = 5;

const FINALIZED_ROUND_RECOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

const FINALIZED_ROUND_RECOVERY_RETRY_DELAY: std::time::Duration =
    std::time::Duration::from_millis(100);

const CERTIFIED_FOLLOWER_FCU_ATTEMPTS: usize = 30;

const CERTIFIED_FOLLOWER_FCU_ATTEMPT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(1);

const CERTIFIED_FOLLOWER_FCU_RETRY_DELAY: std::time::Duration =
    std::time::Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveredFcuAction {
    Complete,
    Retry,
    Fatal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveredRethForkchoice {
    pub head: ProjectionCheckpoint,
    pub safe: Option<ProjectionCheckpoint>,
    pub finalized: Option<ProjectionCheckpoint>,
}

impl From<crate::execution_port::RecoveryForkchoice> for RecoveredRethForkchoice {
    fn from(value: crate::execution_port::RecoveryForkchoice) -> Self {
        Self {
            head: value.head,
            safe: value.safe,
            finalized: value.finalized,
        }
    }
}

impl RecoveredRethForkchoice {
    fn is_exact(self, anchor: ProjectionCheckpoint) -> bool {
        self.head == anchor && self.safe == Some(anchor) && self.finalized == Some(anchor)
    }
}

pub fn classify_recovered_fcu_attempt(
    anchor: ProjectionCheckpoint,
    response: &RecoveredForkchoiceAttempt,
    readback: RecoveredRethForkchoice,
) -> RecoveredFcuAction {
    if matches!(
        response,
        RecoveredForkchoiceAttempt::Invalid(_) | RecoveredForkchoiceAttempt::Fatal(_)
    ) {
        return RecoveredFcuAction::Fatal;
    }
    let finalized_conflict = readback.finalized.is_some_and(|observed| {
        observed.block_number > anchor.block_number
            || (observed.block_number == anchor.block_number
                && observed.block_hash != anchor.block_hash)
    });
    let exact_height_conflict = [Some(readback.head), readback.safe, readback.finalized]
        .into_iter()
        .flatten()
        .any(|observed| {
            observed.block_number == anchor.block_number && observed.block_hash != anchor.block_hash
        });
    if finalized_conflict || exact_height_conflict {
        return RecoveredFcuAction::Fatal;
    }
    if matches!(
        response,
        RecoveredForkchoiceAttempt::Valid | RecoveredForkchoiceAttempt::Retryable(_)
    ) && readback.is_exact(anchor)
    {
        RecoveredFcuAction::Complete
    } else {
        RecoveredFcuAction::Retry
    }
}

pub async fn confirm_recovered_forkchoice<C, Attempt, AttemptFuture, ReadProvider>(
    clock: C,
    anchor: ProjectionCheckpoint,
    mut attempt: Attempt,
    mut read_provider: ReadProvider,
) -> Result<()>
where
    C: Clock,
    Attempt: FnMut() -> AttemptFuture,
    AttemptFuture: Future<Output = RecoveredForkchoiceAttempt> + Send + 'static,
    ReadProvider: FnMut() -> Result<RecoveredRethForkchoice>,
{
    let initially_observed = read_provider()?;
    match classify_recovered_fcu_attempt(
        anchor,
        &RecoveredForkchoiceAttempt::Valid,
        initially_observed,
    ) {
        RecoveredFcuAction::Complete => return Ok(()),
        RecoveredFcuAction::Fatal => {
            return Err(
                crate::errors::StackRecoveryError::RethFinalizedIdentityConflictsWithAnchor {
                    observed: format!("{initially_observed:?}"),
                    anchor,
                }
                .into(),
            );
        }
        RecoveredFcuAction::Retry => {}
    }

    for attempt_number in 1..=CERTIFIED_FOLLOWER_FCU_ATTEMPTS {
        let outcome = match clock
            .timeout(CERTIFIED_FOLLOWER_FCU_ATTEMPT_TIMEOUT, attempt())
            .await
        {
            Ok(outcome) => outcome,
            Err(_) => RecoveredForkchoiceAttempt::Retryable(
                crate::errors::StackRecoveryError::RecoveredFcuAttemptTimedOut(attempt_number)
                    .to_string(),
            ),
        };
        let observed = read_provider()?;
        match classify_recovered_fcu_attempt(anchor, &outcome, observed) {
            RecoveredFcuAction::Complete => return Ok(()),
            RecoveredFcuAction::Fatal => {
                return Err(
                    crate::errors::StackRecoveryError::RecoveredFcuFailedClosed {
                        anchor,
                        outcome: format!("{outcome:?}"),
                        observed: format!("{observed:?}"),
                    }
                    .into(),
                );
            }
            RecoveredFcuAction::Retry if attempt_number < CERTIFIED_FOLLOWER_FCU_ATTEMPTS => {
                clock.sleep(CERTIFIED_FOLLOWER_FCU_RETRY_DELAY).await;
            }
            RecoveredFcuAction::Retry => {
                return Err(
                    crate::errors::StackRecoveryError::RecoveredFcuDidNotConverge {
                        anchor,
                        attempts: CERTIFIED_FOLLOWER_FCU_ATTEMPTS,
                        outcome: format!("{outcome:?}"),
                        observed: format!("{observed:?}"),
                    }
                    .into(),
                );
            }
        }
    }
    unreachable!("certified follower FCU attempt budget is non-zero")
}

// ===========================================================================
// Helper functions
// ===========================================================================

pub async fn recover_application_finalized_round<C>(
    clock: C,
    marshal_mailbox: crate::marshal_types::MarshalMailbox,
    last_execution_height: u64,
) -> Result<Option<RecoveredApplicationFinalization>>
where
    C: Clock,
{
    if last_execution_height == 0 {
        return Ok(None);
    }

    let height = Height::new(last_execution_height);
    for attempt in 1..=FINALIZED_ROUND_RECOVERY_ATTEMPTS {
        // Measure the per-attempt timeout on the consensus runtime `Clock`, not
        // tokio's wall-clock, so recovery is reproducible under the deterministic
        // test runtime. `Clock::timeout` requires a `Send + 'static` future, so this
        // loop clones the mailbox (a cheap sender clone) and moves it into the request.
        let mailbox = marshal_mailbox.clone();
        match clock
            .timeout(FINALIZED_ROUND_RECOVERY_TIMEOUT, async move {
                mailbox.get_finalization(height).await
            })
            .await
        {
            Ok(Some(finalization)) => {
                let round = finalization.proposal.round;
                let digest = finalization.proposal.payload;
                info!(
                    last_execution_height,
                    ?round,
                    %digest,
                    "recovered application finalized round from marshal archive"
                );
                return Ok(Some(RecoveredApplicationFinalization { round, digest }));
            }
            Ok(None) if attempt < FINALIZED_ROUND_RECOVERY_ATTEMPTS => {
                clock.sleep(FINALIZED_ROUND_RECOVERY_RETRY_DELAY).await;
            }
            Ok(None) => {
                return Err(
                    crate::errors::StackRecoveryError::MarshalFinalizationMissingCorrupt(
                        last_execution_height,
                    )
                    .into(),
                );
            }
            Err(_) if attempt < FINALIZED_ROUND_RECOVERY_ATTEMPTS => {
                clock.sleep(FINALIZED_ROUND_RECOVERY_RETRY_DELAY).await;
            }
            Err(_) => {
                return Err(
                    crate::errors::StackRecoveryError::MarshalFinalizationTimeoutCorrupt(
                        last_execution_height,
                    )
                    .into(),
                );
            }
        }
    }

    Err(
        crate::errors::StackRecoveryError::MarshalFinalizationMissingCorrupt(last_execution_height)
            .into(),
    )
}

async fn marshal_get_block_with_timeout_retries<C: Clock>(
    clock: &C,
    marshal_mailbox: &crate::marshal_types::MarshalMailbox,
    height: u64,
    target_height: u64,
) -> Result<Option<commonware_cryptography::Digested<crate::block::ConsensusBlock>>> {
    let h = Height::new(height);
    for attempt in 1..=FINALIZED_ROUND_RECOVERY_ATTEMPTS {
        let mailbox = marshal_mailbox.clone();
        match clock
            .timeout(FINALIZED_ROUND_RECOVERY_TIMEOUT, async move {
                mailbox.get_block(h).await
            })
            .await
        {
            Ok(block_opt) => return Ok(block_opt),
            Err(_) if attempt < FINALIZED_ROUND_RECOVERY_ATTEMPTS => {
                clock.sleep(FINALIZED_ROUND_RECOVERY_RETRY_DELAY).await;
            }
            Err(_) => {
                return Err(
                    crate::errors::StackRecoveryError::MarshalFinalizationTimeoutCorrupt(
                        target_height,
                    )
                    .into(),
                );
            }
        }
    }
    Err(crate::errors::StackRecoveryError::MarshalFinalizationTimeoutCorrupt(target_height).into())
}

async fn marshal_get_finalization_with_timeout_retries<C: Clock>(
    clock: &C,
    marshal_mailbox: &crate::marshal_types::MarshalMailbox,
    height: u64,
    target_height: u64,
) -> Result<Option<crate::marshal_types::Finalization>> {
    let h = Height::new(height);
    for attempt in 1..=FINALIZED_ROUND_RECOVERY_ATTEMPTS {
        let mailbox = marshal_mailbox.clone();
        match clock
            .timeout(FINALIZED_ROUND_RECOVERY_TIMEOUT, async move {
                mailbox.get_finalization(h).await
            })
            .await
        {
            Ok(fin_opt) => return Ok(fin_opt),
            Err(_) if attempt < FINALIZED_ROUND_RECOVERY_ATTEMPTS => {
                clock.sleep(FINALIZED_ROUND_RECOVERY_RETRY_DELAY).await;
            }
            Err(_) => {
                return Err(
                    crate::errors::StackRecoveryError::MarshalFinalizationTimeoutCorrupt(
                        target_height,
                    )
                    .into(),
                );
            }
        }
    }
    Err(crate::errors::StackRecoveryError::MarshalFinalizationTimeoutCorrupt(target_height).into())
}

async fn probe_initial_target_marshal_records<C: Clock>(
    clock: &C,
    marshal_mailbox: &crate::marshal_types::MarshalMailbox,
    target_height: u64,
) -> Result<(
    Option<crate::marshal_types::Finalization>,
    Option<commonware_cryptography::Digested<crate::block::ConsensusBlock>>,
)> {
    let height = Height::new(target_height);
    for attempt in 1..=FINALIZED_ROUND_RECOVERY_ATTEMPTS {
        let fin_mailbox = marshal_mailbox.clone();
        let fin_res = clock
            .timeout(FINALIZED_ROUND_RECOVERY_TIMEOUT, async move {
                fin_mailbox.get_finalization(height).await
            })
            .await;
        let blk_mailbox = marshal_mailbox.clone();
        let blk_res = clock
            .timeout(FINALIZED_ROUND_RECOVERY_TIMEOUT, async move {
                blk_mailbox.get_block(height).await
            })
            .await;

        match (fin_res, blk_res) {
            (Ok(None), Ok(None)) if attempt < FINALIZED_ROUND_RECOVERY_ATTEMPTS => {
                clock.sleep(FINALIZED_ROUND_RECOVERY_RETRY_DELAY).await;
            }
            (Ok(None), Ok(None)) => {
                return Err(
                    crate::errors::StackRecoveryError::MarshalFinalizationMissingCorrupt(
                        target_height,
                    )
                    .into(),
                );
            }
            (Ok(fin_opt), Ok(blk_opt)) => return Ok((fin_opt, blk_opt)),
            (Err(_), _) | (_, Err(_)) if attempt < FINALIZED_ROUND_RECOVERY_ATTEMPTS => {
                clock.sleep(FINALIZED_ROUND_RECOVERY_RETRY_DELAY).await;
            }
            (Err(_), _) | (_, Err(_)) => {
                return Err(
                    crate::errors::StackRecoveryError::MarshalFinalizationTimeoutCorrupt(
                        target_height,
                    )
                    .into(),
                );
            }
        }
    }
    Err(crate::errors::StackRecoveryError::MarshalFinalizationMissingCorrupt(target_height).into())
}

/// Authenticate `(target_height, expected_hash)` from durable Marshal history
/// using ephemeral private recovery committee providers.
///
/// Supports both:
/// - **Direct `F(target_height)`**: historically authenticated via canonical
///   `BoundaryOutcome` + committed `CommitteeSnapshot` at `expected_hash`.
/// - **Sparse ancestor (`get_finalization(target_height) == None`)**: walks
///   contiguous Marshal blocks `target_height + 1 ..= K` up to the nearest local
///   `F(K)` (`K <= archive_ceiling`), advancing `CommitteeChain` +
///   `FollowerEpocher` `D1`-safely across any epoch boundaries, verifies `F(K)`,
///   and extracts `target_height`'s own round and actual-kind parent proof record
///   (`Finalization` or `CertifiedNotarization`) from `Block(target_height + 1)`.
pub async fn recover_authenticated_application_finalization<C, S>(
    clock: C,
    marshal_mailbox: crate::marshal_types::MarshalMailbox,
    target_height: u64,
    expected_hash: B256,
    archive_ceiling: u64,
    source: &S,
    committee_config: crate::stack::recovery::anchor::RecoveryCommitteeConfig,
) -> Result<Option<crate::stack::recovery::anchor::AuthenticatedApplicationFinalization>>
where
    C: Clock,
    S: crate::stack::recovery::anchor::CanonicalHistorySource,
{
    use crate::stack::recovery::anchor::{
        AuthenticatedApplicationFinalization, RecoveryCommitteeAuthority,
    };
    use commonware_consensus::types::Epocher as _;
    use outbe_primitives::consensus_metadata::ParentParticipationProof;

    if target_height == 0 {
        return Ok(None);
    }

    let (fin_opt, block_opt) =
        probe_initial_target_marshal_records(&clock, &marshal_mailbox, target_height).await?;

    if let Some(f_h) = fin_opt {
        if f_h.proposal.payload.0 != expected_hash {
            return Err(
                crate::errors::StackRecoveryError::MarshalFinalizationDigestMismatch {
                    height: target_height,
                    execution: expected_hash,
                    marshal: f_h.proposal.payload.0,
                }
                .into(),
            );
        }
        if let Some(b_h) = block_opt {
            if b_h.inner().number() != target_height {
                return Err(
                    crate::errors::StackRecoveryError::LocalArchivedBlockHeightMismatch {
                        actual: b_h.inner().number(),
                        expected: target_height,
                    }
                    .into(),
                );
            }
            if b_h.inner().digest() != f_h.proposal.payload {
                return Err(
                    crate::errors::StackRecoveryError::LocalArchivedBlockHashMismatch {
                        actual: b_h.inner().digest().0,
                        canonical_hash: f_h.proposal.payload.0,
                        height: target_height,
                    }
                    .into(),
                );
            }
        }

        let authority = RecoveryCommitteeAuthority::init_at_checkpoint(
            source,
            committee_config,
            target_height,
            expected_hash,
        )?;
        if f_h.proposal.round.epoch() != authority.current_epoch() {
            return Err(
                crate::errors::StackRecoveryError::AncestorCertificateCommitteeMismatch.into(),
            );
        }
        let verified_f = crate::finalization::evidence::verify_direct_finalization(
            target_height,
            &f_h,
            authority.schemes(),
            authority.committees(),
        )?
        .ok_or_else(|| {
            crate::errors::StackRecoveryError::MissingRecoveryEpochVerifierScheme(
                authority.current_epoch().get(),
            )
        })?;
        let parent_proof_record = authority.build_finalization_record(target_height, &f_h)?;
        info!(
            target_height,
            round = ?verified_f.round,
            digest = %verified_f.digest,
            "recovered historically authenticated direct finalization from marshal archive"
        );
        return Ok(Some(AuthenticatedApplicationFinalization {
            round: verified_f.round,
            digest: verified_f.digest,
            parent_proof_record,
        }));
    }

    let block_h = block_opt.ok_or(
        crate::errors::StackRecoveryError::MarshalFinalizationMissingCorrupt(target_height),
    )?;
    if block_h.inner().number() != target_height {
        return Err(
            crate::errors::StackRecoveryError::LocalArchivedBlockHeightMismatch {
                actual: block_h.inner().number(),
                expected: target_height,
            }
            .into(),
        );
    }
    if block_h.inner().digest().0 != expected_hash {
        return Err(
            crate::errors::StackRecoveryError::MarshalFinalizationDigestMismatch {
                height: target_height,
                execution: expected_hash,
                marshal: block_h.inner().digest().0,
            }
            .into(),
        );
    }
    if archive_ceiling <= target_height {
        return Err(
            crate::errors::StackRecoveryError::MarshalFinalizationMissingCorrupt(target_height)
                .into(),
        );
    }

    let mut authority = RecoveryCommitteeAuthority::init_at_checkpoint(
        source,
        committee_config,
        target_height,
        expected_hash,
    )?;
    let mut prev_block = block_h.inner().clone();
    let mut child_h: Option<crate::block::ConsensusBlock> = None;

    for k in (target_height.saturating_add(1))..=archive_ceiling {
        let Some(block_k) =
            marshal_get_block_with_timeout_retries(&clock, &marshal_mailbox, k, target_height)
                .await?
        else {
            return Err(
                crate::errors::StackRecoveryError::MarshalFinalizationMissingCorrupt(target_height)
                    .into(),
            );
        };
        if block_k.inner().number() != k {
            return Err(
                crate::errors::StackRecoveryError::LocalArchivedBlockHeightMismatch {
                    actual: block_k.inner().number(),
                    expected: k,
                }
                .into(),
            );
        }
        if block_k.inner().parent_hash() != prev_block.digest().0 {
            return Err(crate::errors::StackRecoveryError::LocalBlockDiffersFromCheckpoint.into());
        }
        if k == target_height.saturating_add(1) {
            child_h = Some(block_k.inner().clone());
        }

        authority.advance_block(&prev_block, block_k.inner())?;

        if let Some(f_k) = marshal_get_finalization_with_timeout_retries(
            &clock,
            &marshal_mailbox,
            k,
            target_height,
        )
        .await?
        {
            if f_k.proposal.payload != block_k.inner().digest() {
                return Err(
                    crate::errors::StackRecoveryError::LocalFinalizationPayloadMismatch {
                        actual: f_k.proposal.payload.0,
                        canonical_hash: block_k.inner().digest().0,
                        height: k,
                    }
                    .into(),
                );
            }
            if f_k.proposal.round.epoch() != authority.current_epoch() {
                return Err(
                    crate::errors::StackRecoveryError::AncestorCertificateCommitteeMismatch.into(),
                );
            }
            let _ = crate::finalization::evidence::verify_direct_finalization(
                k,
                &f_k,
                authority.schemes(),
                authority.committees(),
            )?
            .ok_or_else(|| {
                crate::errors::StackRecoveryError::MissingRecoveryEpochVerifierScheme(
                    authority.current_epoch().get(),
                )
            })?;

            let child_block = child_h
                .as_ref()
                .expect("k > target_height always populates target_height + 1");
            let witness_h = crate::finalization::evidence::verify_child_parent_witness(
                block_h.inner(),
                child_block,
                authority.schemes(),
                authority.committees(),
            )?
            .ok_or_else(|| {
                crate::errors::StackRecoveryError::MissingRecoveryEpochVerifierScheme(
                    authority.current_epoch().get(),
                )
            })?;
            if authority
                .epocher()
                .containing(Height::new(target_height))
                .is_none_or(|bounds| bounds.epoch() != witness_h.round.epoch())
            {
                return Err(
                    crate::errors::StackRecoveryError::AncestorCertificateCommitteeMismatch.into(),
                );
            }

            let parent_proof_record = match witness_h.proof_kind {
                ParentParticipationProof::Finalization => {
                    let f_ref = witness_h.finalization.as_ref().ok_or(
                        crate::errors::StackRecoveryError::MissingDirectRecoveryCertificate,
                    )?;
                    let _ = crate::finalization::evidence::verify_direct_finalization(
                        target_height,
                        f_ref,
                        authority.schemes(),
                        authority.committees(),
                    )?
                    .ok_or_else(|| {
                        crate::errors::StackRecoveryError::MissingRecoveryEpochVerifierScheme(
                            witness_h.round.epoch().get(),
                        )
                    })?;
                    authority.build_finalization_record(target_height, f_ref)?
                }
                ParentParticipationProof::CertifiedNotarization => {
                    RecoveryCommitteeAuthority::build_cn_record(
                        witness_h.clone(),
                        block_h.inner().digest(),
                    )
                }
            };

            info!(
                target_height,
                descendant_finalized_height = k,
                round = ?witness_h.round,
                proof_kind = ?witness_h.proof_kind,
                digest = %block_h.inner().digest(),
                "recovered sparse ancestor own round from descendant-certified marshal archive"
            );
            return Ok(Some(AuthenticatedApplicationFinalization {
                round: witness_h.round,
                digest: block_h.inner().digest(),
                parent_proof_record,
            }));
        }

        prev_block = block_k.inner().clone();
    }

    Err(crate::errors::StackRecoveryError::MarshalFinalizationMissingCorrupt(target_height).into())
}
