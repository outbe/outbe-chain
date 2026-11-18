use std::{
    path::Path,
    time::{Duration, Instant},
};

use eyre::{bail, Context, Result};
use tokio::task::JoinSet;

use crate::support::progress::Progress;

use super::{
    remote::{NetworkBuild, SshOptions},
    status::{healthy, query_all, ready_for_hyperlane_setup, render},
};

pub async fn run(artifacts: &Path, ssh_key: Option<&Path>, timeout: Duration) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("verifying network artifacts and SSH targets");
    let build = NetworkBuild::load(artifacts)?;
    let ssh = SshOptions::new(ssh_key)?;
    progress.stage(&format!(
        "starting {} founding validators",
        build.config.founders.len()
    ));
    let mut tasks = JoinSet::new();
    for founder in build.config.founders.clone() {
        let ssh = ssh.clone();
        tasks.spawn_blocking(move || {
            ssh.run_quiet(
                &founder,
                "if [ \"$(id -u)\" -eq 0 ]; then systemctl enable --now outbe-validator.target; else sudo -n systemctl enable --now outbe-validator.target; fi",
            )
            .wrap_err_with(|| format!("start {}", founder.name))
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.wrap_err("founder start task panicked")??;
    }

    let required_peers = build.config.founders.len().saturating_sub(1);
    let deadline = Instant::now() + timeout;
    let mut hyperlane_configured = false;
    progress.stage("waiting for finalized consensus");
    loop {
        let statuses = query_all(&build, &ssh).await?;
        if !hyperlane_configured
            && statuses.iter().all(|status| {
                ready_for_hyperlane_setup(status, build.config.chain_id, required_peers)
            })
        {
            progress.stage("configuring Hyperlane signers on chain");
            configure_hyperlane(&build, &ssh).await?;
            hyperlane_configured = true;
            progress.stage("waiting for healthy Hyperlane validators");
            continue;
        }
        if hyperlane_configured
            && statuses
                .iter()
                .all(|status| healthy(status, build.config.chain_id, required_peers))
        {
            progress.done(&format!(
                "network {} finalized with {} founding validators",
                build.config.name,
                statuses.len()
            ));
            println!("\n{}", render(&build, &statuses));
            return Ok(());
        }
        if let Some(failed) = statuses.iter().find(|status| {
            let core_failed = [
                &status.target,
                &status.validator,
                &status.enclave,
                &status.radicle,
                &status.feeder,
            ]
            .into_iter()
            .any(|service| service == "failed");
            let hyperlane_failed = hyperlane_configured && status.hyperlane == "failed";
            core_failed || hyperlane_failed
        }) {
            let founder = build
                .config
                .founders
                .iter()
                .find(|founder| founder.name == failed.name)
                .expect("status belongs to configured founder");
            let logs = ssh
                .capture(
                    founder,
                    "journalctl --no-pager -n 30 -u outbe-enclave.service -u outbe-radicle.service -u outbe-feeder.service -u outbe-hyperlane-validator.service -u outbe-validator.service",
                )
                .unwrap_or_else(|error| format!("could not read journal: {error}"));
            bail!("{} failed during network start:\n{logs}", failed.name);
        }
        if Instant::now() >= deadline {
            bail!(
                "network did not reach finalized consensus within {} seconds:\n{}",
                timeout.as_secs(),
                render(&build, &statuses)
            );
        }
        let ready = statuses.iter().filter(|status| {
            if hyperlane_configured {
                healthy(status, build.config.chain_id, required_peers)
            } else {
                ready_for_hyperlane_setup(status, build.config.chain_id, required_peers)
            }
        });
        progress.update(&format!("{} of {} ready", ready.count(), statuses.len()));
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn configure_hyperlane(build: &super::remote::NetworkBuild, ssh: &SshOptions) -> Result<()> {
    let mut tasks = JoinSet::new();
    for (index, founder) in build.config.founders.clone().into_iter().enumerate() {
        let ssh = ssh.clone();
        tasks.spawn_blocking(move || {
            let script = format!("/opt/outbe-chain/validator-{index}/configure-hyperlane.sh");
            ssh.run_quiet(
                &founder,
                &format!("if [ \"$(id -u)\" -eq 0 ]; then {script}; else sudo -n {script}; fi"),
            )
            .wrap_err_with(|| format!("configure Hyperlane for {}", founder.name))
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.wrap_err("Hyperlane setup task panicked")??;
    }
    Ok(())
}
