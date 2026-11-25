use eyre::{ensure, Result};

use crate::support::{process, progress::Progress};

use super::{journal::Journal, rpc::ConsensusStatus, sync, VALIDATOR_TARGET};

pub const DEFAULT_MAINTENANCE_HEADROOM_BLOCKS: u64 = 64;

pub fn ensure_maintenance_safe(
    status: &ConsensusStatus,
    skip_dkg_guard: bool,
    headroom_blocks: u64,
) -> Result<()> {
    if skip_dkg_guard {
        eprintln!("warning: DKG maintenance guard explicitly skipped");
        return Ok(());
    }
    let safe = status.maintenance_safe.ok_or_else(|| {
        eyre::eyre!(
            "node does not expose maintenanceSafe; upgrade it first or explicitly pass --skip-dkg-guard"
        )
    })?;
    ensure!(safe, "maintenance is unsafe during the current DKG window");
    let freeze = status.next_dkg_freeze_height.ok_or_else(|| {
        eyre::eyre!("node does not expose nextDkgFreezeHeight; explicitly pass --skip-dkg-guard")
    })?;
    let remaining = freeze.saturating_sub(status.last_finalized_block);
    ensure!(
        remaining > headroom_blocks,
        "only {remaining} blocks remain before DKG freeze; require more than {headroom_blocks}"
    );
    Ok(())
}

async fn local_guard(journal: &Journal, skip_dkg_guard: bool) -> Result<()> {
    let local = super::rpc::RpcClient::new(journal.config.network.local_rpc_url())?;
    let status = local.consensus_status().await?;
    ensure_maintenance_safe(&status, skip_dkg_guard, DEFAULT_MAINTENANCE_HEADROOM_BLOCKS)
}

pub async fn start() -> Result<()> {
    let _lock = process::LifecycleLock::acquire()?;
    let journal = Journal::load()?;
    let mut progress = Progress::new();
    sync::start_and_wait(&journal, &mut progress, true).await
}

pub async fn stop(skip_dkg_guard: bool) -> Result<()> {
    let _lock = process::LifecycleLock::acquire()?;
    let journal = Journal::load()?;
    if process::systemctl_is_active(VALIDATOR_TARGET) {
        local_guard(&journal, skip_dkg_guard).await?;
    }
    process::clear_services_enabled()?;
    process::systemctl(&["disable", "--now", VALIDATOR_TARGET])?;
    ensure!(
        !process::systemctl_is_active(VALIDATOR_TARGET),
        "validator target is still active"
    );
    println!("validator stopped and disabled across VM reboots");
    Ok(())
}

pub async fn restart(skip_dkg_guard: bool) -> Result<()> {
    let _lock = process::LifecycleLock::acquire()?;
    let journal = Journal::load()?;
    ensure!(
        process::systemctl_is_active(VALIDATOR_TARGET),
        "validator target is not active; run `outbe-up validator start`"
    );
    local_guard(&journal, skip_dkg_guard).await?;
    let enabled_on_boot = process::services_enabled();
    process::systemctl(&["stop", VALIDATOR_TARGET])?;
    let mut progress = Progress::new();
    sync::start_and_wait(&journal, &mut progress, enabled_on_boot).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(last: u64, freeze: Option<u64>, safe: Option<bool>) -> ConsensusStatus {
        ConsensusStatus {
            last_finalized_block: last,
            next_dkg_freeze_height: freeze,
            maintenance_safe: safe,
            ..Default::default()
        }
    }

    #[test]
    fn guard_requires_new_rpc_and_headroom() {
        assert!(ensure_maintenance_safe(&status(100, None, None), false, 10).is_err());
        assert!(ensure_maintenance_safe(&status(100, Some(200), Some(false)), false, 10).is_err());
        assert!(ensure_maintenance_safe(&status(195, Some(200), Some(true)), false, 10).is_err());
        assert!(ensure_maintenance_safe(&status(100, Some(200), Some(true)), false, 10).is_ok());
        assert!(ensure_maintenance_safe(&status(100, None, None), true, 10).is_ok());
    }
}
