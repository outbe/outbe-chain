//! Probe execution readiness against the certified consensus tip.
use super::super::*;
use super::runtime::*;
impl<E> EpochSupervisor<E>
where
    E: BufferPooler
        + Clock
        + CryptoRng
        + Network
        + Resolver
        + Spawner
        + Storage
        + Metrics
        + Send
        + Sync
        + 'static,
{
    pub(super) fn check_execution(
        &self,
        ctx: &E,
        latest_consensus_tip: Option<crate::marshal_update_reporter::ConsensusTip>,
        watchdog_started_at: SystemTime,
        unhealthy_since: &mut Option<SystemTime>,
    ) -> Result<()> {
        let Some(tip) = latest_consensus_tip else {
            debug!("execution watchdog waiting for first consensus tip");
            return Ok(());
        };

        let consensus_tip_height = tip.height.get();
        let mut reth_head_height = match self.node.provider.last_block_number() {
            Ok(height) => height,
            Err(error) => {
                let now = ctx.current();
                let (decision, next_unhealthy_since) = execution_watchdog_decision(
                    ExecutionWatchdogObservation::ProviderReadError,
                    now,
                    watchdog_started_at,
                    *unhealthy_since,
                );
                (*unhealthy_since) = next_unhealthy_since;
                match decision {
                    ExecutionWatchdogDecision::StartupGrace => {
                        let startup_elapsed = elapsed_since(now, watchdog_started_at);
                        warn!(
                            %error,
                            startup_elapsed_ms = startup_elapsed.as_millis(),
                            startup_grace_sec = config::EXECUTION_WATCHDOG_STARTUP_GRACE_SEC,
                            "execution watchdog failed to read Reth provider head during startup/backfill grace"
                        );
                    }
                    ExecutionWatchdogDecision::Unhealthy { unhealthy_for } => {
                        warn!(
                            %error,
                            unhealthy_for_ms = unhealthy_for.as_millis(),
                            "execution watchdog failed to read Reth provider head"
                        );
                    }
                    ExecutionWatchdogDecision::Fatal { unhealthy_for } => {
                        return Err(eyre::eyre!(
                            "execution watchdog provider read failed for {:?}: {error}",
                            unhealthy_for
                        ));
                    }
                    ExecutionWatchdogDecision::Healthy => {}
                }
                return Ok(());
            }
        };
        let provider_tip_hash = match self.node.provider.block_hash(consensus_tip_height) {
            Ok(hash) => hash,
            Err(error) => {
                let now = ctx.current();
                let (decision, next_unhealthy_since) = execution_watchdog_decision(
                    ExecutionWatchdogObservation::ProviderReadError,
                    now,
                    watchdog_started_at,
                    *unhealthy_since,
                );
                (*unhealthy_since) = next_unhealthy_since;
                match decision {
                    ExecutionWatchdogDecision::StartupGrace => {
                        let startup_elapsed = elapsed_since(now, watchdog_started_at);
                        warn!(
                            %error,
                            consensus_tip_height,
                            consensus_tip_digest = %tip.digest,
                            reth_head_height,
                            startup_elapsed_ms = startup_elapsed.as_millis(),
                            startup_grace_sec = config::EXECUTION_WATCHDOG_STARTUP_GRACE_SEC,
                            "execution watchdog failed to read Reth provider hash at consensus tip during startup/backfill grace"
                        );
                    }
                    ExecutionWatchdogDecision::Unhealthy { unhealthy_for } => {
                        warn!(
                            %error,
                            consensus_tip_height,
                            consensus_tip_digest = %tip.digest,
                            reth_head_height,
                            unhealthy_for_ms = unhealthy_for.as_millis(),
                            "execution watchdog failed to read Reth provider hash at consensus tip"
                        );
                    }
                    ExecutionWatchdogDecision::Fatal { unhealthy_for } => {
                        return Err(eyre::eyre!(
                                        "execution watchdog provider hash read failed at consensus tip height {} for {:?}: {error}",
                                        consensus_tip_height,
                                        unhealthy_for
                                    ));
                    }
                    ExecutionWatchdogDecision::Healthy => {}
                }
                return Ok(());
            }
        };
        let hash_match = provider_tip_hash == Some(tip.digest.0);
        match self.node.provider.last_block_number() {
            Ok(height) => {
                reth_head_height = height;
            }
            Err(error) => {
                warn!(
                    %error,
                    consensus_tip_height,
                    consensus_tip_digest = %tip.digest,
                    previous_reth_head_height = reth_head_height,
                    "execution watchdog failed to refresh Reth provider head after consensus tip hash probe; using previous height sample"
                );
            }
        }
        outbe_consensus::metrics::record_consensus_reth_state(
            consensus_tip_height,
            reth_head_height,
            hash_match,
        );

        let consensus_ahead = consensus_tip_height.saturating_sub(reth_head_height);
        let now = ctx.current();
        let (decision, next_unhealthy_since) = execution_watchdog_decision(
            ExecutionWatchdogObservation::ProviderState {
                consensus_tip_height,
                reth_head_height,
                hash_match,
            },
            now,
            watchdog_started_at,
            *unhealthy_since,
        );
        (*unhealthy_since) = next_unhealthy_since;
        match decision {
            ExecutionWatchdogDecision::Healthy => {}
            ExecutionWatchdogDecision::StartupGrace => {
                let startup_elapsed = elapsed_since(now, watchdog_started_at);
                warn!(
                    consensus_tip_height,
                    consensus_tip_digest = %tip.digest,
                    reth_head_height,
                    ?provider_tip_hash,
                    consensus_ahead,
                    hash_match,
                    startup_elapsed_ms = startup_elapsed.as_millis(),
                    startup_grace_sec = config::EXECUTION_WATCHDOG_STARTUP_GRACE_SEC,
                    "execution watchdog detected Reth provider behind consensus tip during startup/backfill grace"
                );
            }
            ExecutionWatchdogDecision::Unhealthy { unhealthy_for } => {
                warn!(
                    consensus_tip_height,
                    consensus_tip_digest = %tip.digest,
                    reth_head_height,
                    ?provider_tip_hash,
                    consensus_ahead,
                    hash_match,
                    unhealthy_for_ms = unhealthy_for.as_millis(),
                    "execution watchdog detected Reth provider behind consensus tip"
                );
            }
            ExecutionWatchdogDecision::Fatal { unhealthy_for } => {
                return Err(eyre::eyre!(
                                "execution watchdog fatal: Reth provider head/hash not ready for consensus tip height {} digest {} (reth_head={}, provider_tip_hash={:?}, unhealthy_for={:?})",
                                consensus_tip_height,
                                tip.digest,
                                reth_head_height,
                                provider_tip_hash,
                                unhealthy_for,
                            ));
            }
        }

        Ok(())
    }
}
