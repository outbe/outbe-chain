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
pub(in crate::stack) enum RecoveredFcuAction {
    Complete,
    Retry,
    Fatal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::stack) struct RecoveredRethForkchoice {
    pub(in crate::stack) head: ProjectionCheckpoint,
    pub(in crate::stack) safe: Option<ProjectionCheckpoint>,
    pub(in crate::stack) finalized: Option<ProjectionCheckpoint>,
}

impl RecoveredRethForkchoice {
    fn is_exact(self, anchor: ProjectionCheckpoint) -> bool {
        self.head == anchor && self.safe == Some(anchor) && self.finalized == Some(anchor)
    }
}

pub(in crate::stack) fn classify_recovered_fcu_attempt(
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

pub(in crate::stack) async fn confirm_recovered_forkchoice<
    C,
    Attempt,
    AttemptFuture,
    ReadProvider,
>(
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

pub(in crate::stack) fn read_reth_recovery_forkchoice(
    state: &reth_chain_state::CanonicalInMemoryState<outbe_primitives::OutbePrimitives>,
) -> Result<RecoveredRethForkchoice> {
    // Persisted execution progress may remain ahead after a canonical rollback.
    let head = state.get_canonical_head();
    if head.hash() == B256::ZERO {
        return Err(crate::errors::StackRecoveryError::RethZeroCanonicalHead.into());
    }
    let checkpoint = |checkpoint: alloy_eips::BlockNumHash| ProjectionCheckpoint {
        block_number: checkpoint.number,
        block_hash: checkpoint.hash,
    };
    Ok(RecoveredRethForkchoice {
        head: checkpoint(head.num_hash()),
        safe: state.get_safe_num_hash().map(checkpoint),
        finalized: state.get_finalized_num_hash().map(checkpoint),
    })
}

pub(in crate::stack) async fn wait_for_recovered_projection(
    name: &str,
    readiness: ProjectionReadinessHandle,
    anchor: ProjectionCheckpoint,
) -> Result<()> {
    readiness
        .wait_without_budget(anchor)
        .await
        .map_err(|failure| {
            failure.into_error(
                || {
                    crate::errors::StackRecoveryError::ProjectionReadinessExpired(name.to_string())
                        .into()
                },
                || {
                    crate::errors::StackRecoveryError::ProjectionAheadOfRecoveryAnchor {
                        name: name.to_string(),
                        block_number: anchor.block_number,
                        block_hash: anchor.block_hash,
                    }
                    .into()
                },
                |failure| {
                    crate::errors::StackRecoveryError::ProjectionReadinessFailed {
                        name: name.to_string(),
                        class: format!("{:?}", failure.class),
                        message: failure.message,
                    }
                    .into()
                },
            )
        })
}

// ===========================================================================
// Helper functions
// ===========================================================================

pub(in crate::stack) async fn recover_application_finalized_round<C>(
    clock: C,
    marshal_mailbox: outbe_consensus::marshal_types::MarshalMailbox,
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
