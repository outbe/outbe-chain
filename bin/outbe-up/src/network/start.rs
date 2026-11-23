use std::{
    path::Path,
    time::{Duration, Instant},
};

use eyre::{bail, Context, Result};
use tokio::task::JoinSet;

use crate::support::progress::Progress;

use super::{
    remote::{NetworkArtifacts, SshOptions},
    status::{healthy, query_all, ready_for_hyperlane_setup, render, FounderStatus},
};

pub async fn run(artifacts: &Path, ssh_key: Option<&Path>, timeout: Duration) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("verifying network artifacts and SSH targets");
    let build = NetworkArtifacts::load(artifacts)?;
    let ssh = SshOptions::new(ssh_key, &build.manifest.genesis.final_sha256)?;
    start_founders(&build, &ssh, &mut progress).await?;
    wait_until_ready(&build, &ssh, timeout, &mut progress).await
}

async fn start_founders(
    build: &NetworkArtifacts,
    ssh: &SshOptions,
    progress: &mut Progress,
) -> Result<()> {
    progress.stage(&format!(
        "starting {} founding validators",
        build.manifest.founders.len()
    ));
    let mut tasks = JoinSet::new();
    for founder in build.manifest.founders.clone() {
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
    Ok(())
}

async fn wait_until_ready(
    build: &NetworkArtifacts,
    ssh: &SshOptions,
    timeout: Duration,
    progress: &mut Progress,
) -> Result<()> {
    let required_peers = build.manifest.founders.len().saturating_sub(1);
    let deadline = Instant::now() + timeout;
    let mut hyperlane_configured = false;
    progress.stage("waiting for finalized consensus");
    loop {
        let statuses = query_all(build, ssh).await?;
        if !hyperlane_configured
            && statuses.iter().all(|status| {
                ready_for_hyperlane_setup(status, build.manifest.chain_id, required_peers)
            })
        {
            progress.stage("configuring Hyperlane signers on chain");
            configure_hyperlane(build, ssh).await?;
            hyperlane_configured = true;
            progress.stage("waiting for healthy Hyperlane validators");
            continue;
        }
        if hyperlane_configured
            && statuses
                .iter()
                .all(|status| healthy(status, build.manifest.chain_id, required_peers))
        {
            progress.done(&format!(
                "network {} finalized with {} founding validators",
                build.manifest.name,
                statuses.len()
            ));
            println!("\n{}", render(build, &statuses));
            return Ok(());
        }
        fail_if_service_failed(build, ssh, &statuses, hyperlane_configured)?;
        if Instant::now() >= deadline {
            bail!(
                "network did not reach finalized consensus within {} seconds:\n{}",
                timeout.as_secs(),
                render(build, &statuses)
            );
        }
        let ready = ready_count(build, &statuses, required_peers, hyperlane_configured);
        progress.update(&format!("{ready} of {} ready", statuses.len()));
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

fn fail_if_service_failed(
    build: &NetworkArtifacts,
    ssh: &SshOptions,
    statuses: &[FounderStatus],
    hyperlane_configured: bool,
) -> Result<()> {
    let Some(failed) = statuses
        .iter()
        .find(|status| service_failed(status, hyperlane_configured))
    else {
        return Ok(());
    };
    let founder = build
        .manifest
        .founders
        .iter()
        .find(|founder| founder.name == failed.name)
        .expect("status belongs to configured founder");
    let logs = ssh
        .capture(
            founder,
            "journalctl --no-pager -n 30 -u outbe-radicle.service -u outbe-feeder.service -u outbe-hyperlane-validator.service -u outbe-validator.service",
        )
        .unwrap_or_else(|error| format!("could not read journal: {error}"));
    bail!("{} failed during network start:\n{logs}", failed.name);
}

fn service_failed(status: &FounderStatus, hyperlane_configured: bool) -> bool {
    status.services().contains(&"failed") || hyperlane_configured && status.hyperlane == "failed"
}

fn ready_count(
    build: &NetworkArtifacts,
    statuses: &[FounderStatus],
    required_peers: usize,
    hyperlane_configured: bool,
) -> usize {
    statuses
        .iter()
        .filter(|status| {
            if hyperlane_configured {
                healthy(status, build.manifest.chain_id, required_peers)
            } else {
                ready_for_hyperlane_setup(status, build.manifest.chain_id, required_peers)
            }
        })
        .count()
}

async fn configure_hyperlane(
    build: &super::remote::NetworkArtifacts,
    ssh: &SshOptions,
) -> Result<()> {
    let mut tasks = JoinSet::new();
    for founder in build.manifest.founders.clone() {
        let ssh = ssh.clone();
        tasks.spawn_blocking(move || {
            let index = founder.index;
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
