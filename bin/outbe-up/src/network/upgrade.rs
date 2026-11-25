use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use eyre::{bail, ensure, Context, Result};
use outbe_genesis_cli::launch_bundle::quote;
use outbe_software_bundle::{sha256_file, verify_and_extract};

use crate::{support::progress::Progress, validator::upgrade::validate_manifest};

use super::{
    lifecycle::{as_root, guard, simplex_quorum, wait_healthy, HealthWait},
    remote::{NetworkArtifacts, SshOptions},
    status::{healthy, query_all},
};

const TARGET: &str = "outbe-validator.target";
const SERVICES_ENABLED_MARKER: &str = "/opt/outbe-chain/state/services-enabled";

pub struct UpgradeOptions<'a> {
    pub artifacts: &'a Path,
    pub software_bundle: &'a Path,
    pub ssh_key: Option<&'a Path>,
    pub timeout: Duration,
    pub skip_dkg_guard: bool,
    pub skip_quorum_guard: bool,
}

pub async fn run(options: UpgradeOptions<'_>) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("verifying network and software bundle");
    let build = NetworkArtifacts::load(options.artifacts)?;
    let ssh = SshOptions::new(options.ssh_key, &build.manifest.genesis.final_sha256)?;
    let bundle = options
        .software_bundle
        .canonicalize()
        .wrap_err("software bundle does not exist")?;
    let staging = tempfile::tempdir()?;
    let manifest = verify_and_extract(&bundle, staging.path())?;
    validate_manifest(&manifest)?;
    let staged_outbe_up = staging.path().join("bin/outbe-up");
    fs::set_permissions(&staged_outbe_up, fs::Permissions::from_mode(0o755))?;
    let bundle_sha = sha256_file(&bundle)?;

    let count = build.manifest.founders.len();
    let quorum = simplex_quorum(count);
    if !options.skip_quorum_guard {
        ensure!(
            count.saturating_sub(1) >= quorum,
            "rolling upgrade would leave {} validators, below quorum {quorum}; pass --skip-quorum-guard only for an intentional outage",
            count.saturating_sub(1)
        );
    }
    let required_peers = count.saturating_sub(1);
    let statuses = query_all(&build, &ssh).await?;
    ensure!(
        statuses
            .iter()
            .all(|status| healthy(status, build.manifest.chain_id, required_peers)),
        "all founders must be healthy before a rolling upgrade"
    );

    for founder in &build.manifest.founders {
        let statuses = query_all(&build, &ssh).await?;
        let status = statuses
            .iter()
            .find(|status| status.name == founder.name)
            .ok_or_else(|| eyre::eyre!("missing status for {}", founder.name))?;
        guard(status, options.skip_dkg_guard)?;
        progress.stage(&format!("upgrading {}", founder.name));

        let remote = ssh.capture(founder, "set -eu; mktemp -d /tmp/outbe-upgrade.XXXXXX")?;
        ensure!(safe_staging_path(&remote), "unsafe remote staging path");
        let remote_bundle = format!("{remote}/software-bundle.tar.zst");
        let remote_outbe_up = format!("{remote}/outbe-up");
        if let Err(error) = ssh
            .copy_to(founder, &[&bundle], &remote_bundle, false)
            .and_then(|()| ssh.copy_to(founder, &[&staged_outbe_up], &remote_outbe_up, false))
        {
            let _ = ssh.run_quiet(founder, &format!("rm -rf -- {}", quote(&remote)));
            bail!(
                "upgrade transfer failed on {}; validator was not changed: {error:#}",
                founder.name
            );
        }
        let result = ssh.run_quiet(
            founder,
            &format!(
                "set -eu; chmod 0755 {tool}; \
test \"$(sha256sum {bundle} | cut -d ' ' -f 1)\" = {digest}; \
{root_command}",
                bundle = quote(&remote_bundle),
                tool = quote(&remote_outbe_up),
                digest = quote(&bundle_sha),
                root_command = as_root(&format!(
                    "systemctl stop {TARGET} && ! systemctl is-active --quiet {TARGET} && \
{} internal apply-software-upgrade --software-bundle {} --founder-index {} && \
systemctl start {TARGET}",
                    quote(&remote_outbe_up),
                    quote(&remote_bundle),
                    founder.index
                )),
            ),
        );
        let cleanup = ssh.run_quiet(founder, &format!("rm -rf -- {}", quote(&remote)));
        if let Err(error) = result {
            let fail_stop = fail_stop_command();
            let stop_error = ssh.run_quiet(founder, &fail_stop).err();
            let _ = cleanup;
            bail!(
                "upgrade failed on {}; validator auto-start was disabled and it remains stopped; run `outbe-up network start --artifacts {}` after inspection: {error:#}{}",
                founder.name,
                options.artifacts.display(),
                stop_error.map_or_else(String::new, |stop| format!("; fail-stop also failed: {stop:#}"))
            );
        }
        cleanup?;
        if let Err(error) = wait_healthy(
            HealthWait {
                build: &build,
                ssh: &ssh,
                founder: &founder.name,
                required_peers,
                timeout: options.timeout,
            },
            &mut progress,
        )
        .await
        {
            let stop_error = ssh.run_quiet(founder, &fail_stop_command()).err();
            bail!(
                "upgraded {} failed health checks; validator auto-start was disabled and it was stopped; run `outbe-up network start --artifacts {}` after inspection: {error:#}{}",
                founder.name,
                options.artifacts.display(),
                stop_error.map_or_else(String::new, |stop| format!("; fail-stop also failed: {stop:#}"))
            );
        }
    }
    progress.done(&format!(
        "upgraded {count} founders to {} ({})",
        manifest.release_tag, manifest.commit
    ));
    Ok(())
}

fn fail_stop_command() -> String {
    as_root(&format!(
        "rm -f -- {SERVICES_ENABLED_MARKER}; systemctl disable --now {TARGET}"
    ))
}

fn safe_staging_path(path: &str) -> bool {
    path.starts_with("/tmp/outbe-upgrade.")
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_owned_upgrade_staging_paths() {
        assert!(safe_staging_path("/tmp/outbe-upgrade.Ab12_3"));
        assert!(!safe_staging_path("/tmp/other.Ab12"));
        assert!(!safe_staging_path("/tmp/outbe-upgrade.x;id"));
    }

    #[test]
    fn fail_stop_disables_persistent_restart() {
        let command = fail_stop_command();
        assert!(command.contains(SERVICES_ENABLED_MARKER));
        assert!(command.contains("systemctl disable --now outbe-validator.target"));
    }
}
