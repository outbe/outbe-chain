use std::{
    path::Path,
    time::{Duration, Instant},
};

use eyre::{bail, Context, Result};
use tokio::task::JoinSet;

use crate::{support::progress::Progress, validator::config::EnclaveRuntime};

use super::{
    remote::{NetworkBuild, SshOptions},
    status::{healthy, query_all, render},
};

pub async fn run(build: &Path, identity: Option<&Path>, timeout: Duration) -> Result<()> {
    let build = NetworkBuild::load(build)?;
    let ssh = SshOptions::new(identity)?;
    let start_renewal = !matches!(build.config.tee.runtime, EnclaveRuntime::Mock);
    let mut tasks = JoinSet::new();
    for founder in build.config.founders.clone() {
        let ssh = ssh.clone();
        tasks.spawn_blocking(move || {
            let services = if start_renewal {
                "outbe-validator.target outbe-tee-renew.timer"
            } else {
                "outbe-validator.target"
            };
            ssh.run(
                &founder,
                &format!(
                    "if [ \"$(id -u)\" -eq 0 ]; then systemctl start {services}; else sudo -n systemctl start {services}; fi"
                ),
            )
            .wrap_err_with(|| format!("start {}", founder.name))
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.wrap_err("founder start task panicked")??;
    }

    let required_peers = build.config.founders.len().saturating_sub(1);
    let deadline = Instant::now() + timeout;
    let mut progress = Progress::new();
    progress.stage("starting founding validators");
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
        if let Some(failed) = statuses
            .iter()
            .find(|status| status.target == "failed" || status.validator == "failed")
        {
            let founder = build
                .config
                .founders
                .iter()
                .find(|founder| founder.name == failed.name)
                .expect("status belongs to configured founder");
            let logs = ssh
                .capture(
                    founder,
                    "journalctl --no-pager -n 30 -u outbe-enclave.service -u outbe-radicle.service -u outbe-validator.service",
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
