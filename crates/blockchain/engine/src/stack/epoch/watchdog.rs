use super::super::*;

pub(in crate::stack) fn provider_matches_consensus_tip(
    provider: &impl BlockHashReader,
    tip: crate::marshal_update_reporter::ConsensusTip,
    required_height: u64,
) -> Result<bool> {
    if tip.height.get() < required_height {
        return Ok(false);
    }
    let Some(provider_hash) = provider.block_hash(tip.height.get()).map_err(|error| {
        eyre::eyre!(
            "failed to read provider block hash at consensus tip height {}: {error}",
            tip.height.get()
        )
    })?
    else {
        return Ok(false);
    };
    Ok(provider_hash == tip.digest.0)
}

pub(in crate::stack) fn elapsed_since(now: SystemTime, since: SystemTime) -> Duration {
    match now.duration_since(since) {
        Ok(elapsed) => elapsed,
        Err(_) => Duration::ZERO,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::stack) enum ExecutionWatchdogObservation {
    ProviderReadError,
    ProviderState {
        consensus_tip_height: u64,
        reth_head_height: u64,
        hash_match: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::stack) enum ExecutionWatchdogDecision {
    Healthy,
    StartupGrace,
    Unhealthy { unhealthy_for: Duration },
    Fatal { unhealthy_for: Duration },
}

pub(in crate::stack) fn execution_watchdog_decision(
    observation: ExecutionWatchdogObservation,
    now: SystemTime,
    startup_started_at: SystemTime,
    unhealthy_since: Option<SystemTime>,
) -> (ExecutionWatchdogDecision, Option<SystemTime>) {
    let unhealthy = match observation {
        ExecutionWatchdogObservation::ProviderReadError => true,
        ExecutionWatchdogObservation::ProviderState {
            consensus_tip_height,
            reth_head_height,
            hash_match,
        } => {
            if hash_match {
                false
            } else if reth_head_height >= consensus_tip_height {
                true
            } else {
                consensus_tip_height.saturating_sub(reth_head_height)
                    > config::EXECUTION_WATCHDOG_LAG_BLOCKS
            }
        }
    };

    if !unhealthy {
        return (ExecutionWatchdogDecision::Healthy, None);
    }

    let startup_grace = Duration::from_secs(config::EXECUTION_WATCHDOG_STARTUP_GRACE_SEC);
    if elapsed_since(now, startup_started_at) < startup_grace {
        return (ExecutionWatchdogDecision::StartupGrace, None);
    }

    let since = unhealthy_since.unwrap_or(now);
    let unhealthy_for = elapsed_since(now, since);
    if unhealthy_for >= config::EXECUTION_WATCHDOG_GRACE {
        (
            ExecutionWatchdogDecision::Fatal { unhealthy_for },
            Some(since),
        )
    } else {
        (
            ExecutionWatchdogDecision::Unhealthy { unhealthy_for },
            Some(since),
        )
    }
}
