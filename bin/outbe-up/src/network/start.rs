use std::{
    path::Path,
    time::{Duration, Instant},
};

use eyre::{bail, Context, Result};
use tokio::task::JoinSet;

use crate::support::progress::Progress;

use super::{
    remote::{NetworkBuild, SshOptions},
    status::{healthy, query_all, render},
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
    progress.stage("waiting for finalized consensus and healthy services");
    loop {
        let statuses = query_all(&build, &ssh).await?;
        if statuses
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
            [
                &status.target,
                &status.validator,
                &status.enclave,
                &status.radicle,
                &status.feeder,
            ]
            .into_iter()
            .any(|service| service == "failed")
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
                    "journalctl --no-pager -n 30 -u outbe-enclave.service -u outbe-radicle.service -u outbe-feeder.service -u outbe-validator.service",
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
        let ready = statuses
            .iter()
            .filter(|status| healthy(status, build.config.chain_id, required_peers))
            .count();
        progress.update(&format!(
            "waiting for finalized consensus ({ready}/{} ready)",
            statuses.len()
        ));
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
