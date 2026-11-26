use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_node_role::progress::Progress;
use tokio::task::JoinSet;

use super::{
    build::DeploymentKind,
    remote::{capture_founder, run_quiet_founder, HostExec, NetworkArtifacts, SshOptions},
    status::{healthy, query_all, ready_for_hyperlane_setup, render, FounderStatus},
};

pub async fn run(
    artifacts: &Path,
    ssh_key: Option<&Path>,
    ssh_config: Option<&Path>,
    timeout: Duration,
) -> Result<()> {
    let build = NetworkArtifacts::load(artifacts)?;
    let ssh: Arc<dyn HostExec> = Arc::new(
        SshOptions::new(ssh_key, ssh_config)?
            .with_network_known_hosts(&build.manifest.genesis.final_sha256)?,
    );
    run_with_exec(artifacts, ssh, timeout).await
}

pub(crate) async fn run_with_exec(
    artifacts: &Path,
    exec: Arc<dyn HostExec>,
    timeout: Duration,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("verifying network artifacts and SSH targets");
    let build = NetworkArtifacts::load(artifacts)?;
    ensure!(
        build.manifest.deployment == DeploymentKind::Host,
        "VM `outbe-net-up network start` orchestration is scheduled for S6.3"
    );
    start_founders(&build, &exec, &mut progress).await?;
    wait_until_ready(&build, &exec, timeout, &mut progress).await
}

async fn start_founders(
    build: &NetworkArtifacts,
    exec: &Arc<dyn HostExec>,
    progress: &mut Progress,
) -> Result<()> {
    progress.stage(&format!(
        "starting {} founding validators",
        build.config.founders.len()
    ));
    let mut tasks = JoinSet::new();
    for founder in build.config.founders.clone() {
        let exec = Arc::clone(exec);
        tasks.spawn_blocking(move || {
            let marker = outbe_operator_node_role::process::SERVICES_ENABLED_MARKER;
            run_quiet_founder(
                exec.as_ref(),
                &founder,
                &format!(
                    "if [ \"$(id -u)\" -eq 0 ]; then \
systemctl enable --now outbe-validator.target && install -d -m 0755 /opt/outbe-chain/state && install -m 0644 /dev/null {marker}; \
else sudo -n sh -c 'systemctl enable --now outbe-validator.target && install -d -m 0755 /opt/outbe-chain/state && install -m 0644 /dev/null {marker}'; fi"
                ),
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
    exec: &Arc<dyn HostExec>,
    timeout: Duration,
    progress: &mut Progress,
) -> Result<()> {
    let required_peers = build.config.founders.len().saturating_sub(1);
    let deadline = Instant::now() + timeout;
    let mut hyperlane_configured = false;
    progress.stage("waiting for finalized consensus");
    loop {
        let statuses = query_all(build, exec).await?;
        if !hyperlane_configured
            && statuses.iter().all(|status| {
                ready_for_hyperlane_setup(status, build.config.chain_id, required_peers)
            })
        {
            progress.stage("configuring Hyperlane signers on chain");
            configure_hyperlane(build, exec).await?;
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
            println!("\n{}", render(build, &statuses));
            return Ok(());
        }
        fail_if_service_failed(build, exec.as_ref(), &statuses, hyperlane_configured)?;
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
    exec: &dyn HostExec,
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
        .config
        .founders
        .iter()
        .find(|founder| founder.name == failed.name)
        .expect("status belongs to configured founder");
    let logs = capture_founder(
        exec,
        founder,
        "journalctl --no-pager -n 30 -u outbe-radicle.service -u outbe-feeder.service -u outbe-hyperlane-agent.service -u outbe-validator.service",
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
                healthy(status, build.config.chain_id, required_peers)
            } else {
                ready_for_hyperlane_setup(status, build.config.chain_id, required_peers)
            }
        })
        .count()
}

async fn configure_hyperlane(build: &NetworkArtifacts, exec: &Arc<dyn HostExec>) -> Result<()> {
    let mut tasks = JoinSet::new();
    for (index, founder) in build.config.founders.clone().into_iter().enumerate() {
        let exec = Arc::clone(exec);
        tasks.spawn_blocking(move || {
            let script = format!("/opt/outbe-chain/validator-{index}/configure-hyperlane.sh");
            run_quiet_founder(
                exec.as_ref(),
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
