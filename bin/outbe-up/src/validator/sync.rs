use std::path::Path;

use eyre::{bail, ensure, Result};

use crate::{
    support::{process, progress::Progress},
    validator::{
        bundle,
        journal::Journal,
        rpc::RpcClient,
        snapshot,
        wait::{wait_for_rpc_chain, wait_for_sync},
        FULL_NODE, LOCAL_RPC, VALIDATOR_SERVICE, VALIDATOR_TARGET,
    },
};

pub async fn run(snapshot_path: Option<&Path>) -> Result<()> {
    let _lock = process::LifecycleLock::acquire()?;
    let mut journal = Journal::load()?;
    let mut progress = Progress::new();
    ensure!(
        !process::systemctl_is_active(VALIDATOR_TARGET)
            && !process::systemctl_is_active(VALIDATOR_SERVICE),
        "validator stack is already active; refusing to restore or replace its data"
    );

    if let Some(path) = snapshot_path {
        let digest = bundle::digest_file(path)?;
        match journal.snapshot_sha256.as_deref() {
            Some(existing) if existing == digest => {
                progress.done("requested snapshot was already restored");
            }
            Some(_) => {
                bail!("a different snapshot was already restored on this host");
            }
            None => {
                ensure!(
                    !process::systemctl_is_active(FULL_NODE),
                    "stop the full node before restoring a snapshot"
                );
                let resume = match journal.snapshot_restore_in_progress.as_deref() {
                    Some(existing) => {
                        ensure!(
                            existing == digest,
                            "an interrupted restore used a different snapshot"
                        );
                        true
                    }
                    None => {
                        journal.snapshot_restore_in_progress = Some(digest.clone());
                        journal.save()?;
                        false
                    }
                };
                progress.stage("verifying and restoring the snapshot");
                let height = snapshot::restore(path, &journal.config, resume)?;
                journal.snapshot_sha256 = Some(digest);
                journal.snapshot_restore_in_progress = None;
                journal.snapshot_finalized_height = Some(height);
                journal.save()?;
                progress.done(&format!("snapshot restored at finalized height {height}"));
            }
        }
    }

    progress.stage("starting keyless full node");
    process::systemctl(&["enable", "--now", FULL_NODE])?;
    let local = RpcClient::new(LOCAL_RPC)?;
    let remote = RpcClient::new(journal.config.network.certified_rpc.clone())?;
    wait_for_rpc_chain(&local, journal.config.network.chain_id, &mut progress).await?;
    progress.stage("synchronizing keyless full node");
    let (height, peers) = wait_for_sync(&local, &remote, &mut progress).await?;
    progress.done(&format!(
        "full node synchronized at finalized height {height} with {peers} peers"
    ));
    Ok(())
}
