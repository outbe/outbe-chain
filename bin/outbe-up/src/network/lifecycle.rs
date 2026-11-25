use std::{
    path::Path,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};

use crate::{
    support::progress::Progress,
    validator::maintenance::{ensure_maintenance_safe, DEFAULT_MAINTENANCE_HEADROOM_BLOCKS},
};

use super::{
    remote::{NetworkArtifacts, SshOptions},
    status::{healthy, query_all, FounderStatus},
};

const TARGET: &str = "outbe-validator.target";
const MARKER: &str = "/opt/outbe-chain/state/services-enabled";

pub async fn stop(artifacts: &Path, ssh_key: Option<&Path>, skip_dkg_guard: bool) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("checking founder maintenance safety");
    let build = NetworkArtifacts::load(artifacts)?;
    let ssh = SshOptions::new(ssh_key, &build.manifest.genesis.final_sha256)?;
    let statuses = query_all(&build, &ssh).await?;
    for status in statuses.iter().filter(|status| status.target == "active") {
        guard(status, skip_dkg_guard)?;
    }
    progress.stage("stopping founding validators");
    for founder in &build.manifest.founders {
        ssh.run_quiet(
            founder,
            &as_root(&format!(
                "rm -f {MARKER} && systemctl disable --now {TARGET} && ! systemctl is-active --quiet {TARGET}"
            )),
        )
        .wrap_err_with(|| format!("stop {}", founder.name))?;
    }
    progress.done(&format!(
        "stopped and disabled {} founding validators",
        build.manifest.founders.len()
    ));
    Ok(())
}

pub async fn restart(
    artifacts: &Path,
    ssh_key: Option<&Path>,
    timeout: Duration,
    skip_dkg_guard: bool,
    skip_quorum_guard: bool,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("checking network maintenance safety");
    let build = NetworkArtifacts::load(artifacts)?;
    let ssh = SshOptions::new(ssh_key, &build.manifest.genesis.final_sha256)?;
    let count = build.manifest.founders.len();
    let quorum = simplex_quorum(count);
    if !skip_quorum_guard {
        ensure!(
            count.saturating_sub(1) >= quorum,
            "rolling restart would leave {} validators, below quorum {quorum}; pass --skip-quorum-guard only for an intentional outage",
            count.saturating_sub(1)
        );
    }
    let required_peers = count.saturating_sub(1);
    let statuses = query_all(&build, &ssh).await?;
    ensure!(
        statuses
            .iter()
            .all(|status| healthy(status, build.manifest.chain_id, required_peers)),
        "all founders must be healthy before a rolling restart"
    );

    for founder in &build.manifest.founders {
        let statuses = query_all(&build, &ssh).await?;
        let status = statuses
            .iter()
            .find(|status| status.name == founder.name)
            .ok_or_else(|| eyre::eyre!("missing status for {}", founder.name))?;
        guard(status, skip_dkg_guard)?;
        progress.stage(&format!("restarting {}", founder.name));
        // A rolling restart is transient: preserve the persistent auto-start marker.
        ssh.run_quiet(
            founder,
            &as_root(&format!(
                "systemctl stop {TARGET} && ! systemctl is-active --quiet {TARGET} && systemctl start {TARGET}"
            )),
        )?;
        wait_healthy(
            HealthWait {
                build: &build,
                ssh: &ssh,
                founder: &founder.name,
                required_peers,
                timeout,
            },
            &mut progress,
        )
        .await?;
    }
    progress.done(&format!("rolling restart completed for {count} founders"));
    Ok(())
}

pub(super) fn guard(status: &FounderStatus, skip_dkg_guard: bool) -> Result<()> {
    let consensus = status
        .consensus
        .as_ref()
        .ok_or_else(|| eyre::eyre!("{} has no consensus status", status.name))?;
    ensure_maintenance_safe(
        consensus,
        skip_dkg_guard,
        DEFAULT_MAINTENANCE_HEADROOM_BLOCKS,
    )
    .wrap_err_with(|| format!("{} is not safe for maintenance", status.name))
}

pub(super) struct HealthWait<'a> {
    pub(super) build: &'a NetworkArtifacts,
    pub(super) ssh: &'a SshOptions,
    pub(super) founder: &'a str,
    pub(super) required_peers: usize,
    pub(super) timeout: Duration,
}

pub(super) async fn wait_healthy(options: HealthWait<'_>, progress: &mut Progress) -> Result<()> {
    let deadline = Instant::now() + options.timeout;
    loop {
        let statuses = query_all(options.build, options.ssh).await?;
        if statuses.iter().any(|status| {
            status.name == options.founder
                && healthy(
                    status,
                    options.build.manifest.chain_id,
                    options.required_peers,
                )
        }) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "{} did not become healthy within {} seconds",
                options.founder,
                options.timeout.as_secs()
            );
        }
        progress.update(&format!(
            "waiting for {} to become healthy",
            options.founder
        ));
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

pub(super) const fn simplex_quorum(validators: usize) -> usize {
    (2 * validators) / 3 + 1
}

pub(super) fn as_root(command: &str) -> String {
    format!(
        "if [ \"$(id -u)\" -eq 0 ]; then sh -c {}; else sudo -n sh -c {}; fi",
        outbe_genesis_cli::launch_bundle::quote(command),
        outbe_genesis_cli::launch_bundle::quote(command),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_quorum_requires_four_validator_topology() {
        assert_eq!(simplex_quorum(4), 3);
        assert!(4 > simplex_quorum(4));
        assert!(3 - 1 < simplex_quorum(3));
        assert!(1 - 1 < simplex_quorum(1));
    }

    #[test]
    fn root_wrapper_preserves_command_as_one_argument() {
        let command = as_root("rm -f /tmp/x; systemctl stop x");
        assert!(command.contains("sudo -n sh -c"));
        assert!(command.contains("systemctl stop x"));
    }
}
