use std::time::{Duration, Instant};

use eyre::{bail, ensure, Result};

use crate::{
    support::progress::Progress,
    validator::rpc::{Block, RpcClient},
};

const SYNC_POLL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct CatchupEta {
    baseline: Option<(u64, Instant)>,
}

impl CatchupEta {
    fn seconds_remaining(&mut self, remaining: u64) -> Option<u64> {
        let now = Instant::now();
        let Some((baseline, started)) = self.baseline else {
            self.baseline = Some((remaining, now));
            return None;
        };
        if remaining >= baseline {
            self.baseline = Some((remaining, now));
            return None;
        }
        let elapsed = started.elapsed().as_secs_f64();
        let caught_up = (baseline - remaining) as f64;
        (elapsed >= 1.0 && caught_up > 0.0)
            .then(|| (remaining as f64 * elapsed / caught_up).round() as u64)
    }
}

pub async fn wait_for_rpc_chain(
    rpc: &RpcClient,
    expected_chain: u64,
    progress: &mut Progress,
) -> Result<()> {
    loop {
        match rpc.chain_id().await {
            Ok(chain) => {
                ensure!(
                    chain == expected_chain,
                    "local RPC is on chain {chain}, expected {expected_chain}"
                );
                return Ok(());
            }
            Err(error) => progress.update(&format!("waiting for local RPC: {error}")),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

pub async fn wait_for_sync(
    local: &RpcClient,
    remote: &RpcClient,
    progress: &mut Progress,
) -> Result<(u64, u64)> {
    let started = Instant::now();
    let mut eta = CatchupEta::default();
    let mut last = None;
    loop {
        let poll = tokio::time::timeout(SYNC_POLL_TIMEOUT, async {
            let remote_block = remote.finalized_block().await?;
            let local_block = local.finalized_block().await?;
            let peers = local.peer_count().await.unwrap_or(0);
            Ok::<_, eyre::Report>((remote_block, local_block, peers))
        })
        .await;
        let (remote_block, local_block, peers) = match poll {
            Ok(result) => result?,
            Err(_) => {
                let detail = last.map_or_else(String::new, |(local, remote)| {
                    format!("; last h={local}/{remote}")
                });
                progress.update(&format!(
                    "sync RPC busy; node still running{}; running={}",
                    detail,
                    human_duration(started.elapsed().as_secs())
                ));
                continue;
            }
        };
        let remaining = remote_block.number.saturating_sub(local_block.number);
        last = Some((local_block.number, remote_block.number));
        progress.update(&format!(
            "sync h={}/{} left={}{} p2p={} running={}",
            local_block.number,
            remote_block.number,
            remaining,
            eta_seconds_field(eta.seconds_remaining(remaining)),
            peers,
            human_duration(started.elapsed().as_secs())
        ));
        if peers > 0 && local_block.number >= remote_block.number {
            let local_at_remote = local.block_number(remote_block.number).await?;
            if local_at_remote.hash == remote_block.hash {
                return Ok((remote_block.number, peers));
            }
            bail!(
                "local and certified RPC finalized hashes disagree at height {}",
                remote_block.number
            );
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

pub async fn wait_for_verifier(
    local: &RpcClient,
    remote: &RpcClient,
    progress: &mut Progress,
) -> Result<()> {
    loop {
        wait_for_sync(local, remote, progress).await?;
        let status = local.consensus_status().await?;
        progress.update(&format!(
            "validator bootstrap mode={} signers={} shares={}",
            status.mode,
            status.connected_peers,
            yes_no(status.has_threshold_shares)
        ));
        if status.is_validator && matches!(status.mode.as_str(), "verifier" | "signer") {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

pub async fn wait_transaction_finalized(
    remote: &RpcClient,
    hash: &str,
    progress: &mut Progress,
) -> Result<()> {
    loop {
        if let Some(block) = remote.transaction_block(hash).await? {
            let finalized = remote.finalized_block().await?.number;
            progress.update(&format!(
                "transaction {hash} block={block} finalized={finalized}"
            ));
            if finalized >= block {
                return Ok(());
            }
        } else {
            progress.update(&format!("waiting for transaction {hash}"));
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

pub async fn estimate_block_seconds(remote: &RpcClient, head: &Block) -> Option<f64> {
    let window = head.number.min(120);
    if window == 0 {
        return None;
    }
    let previous = remote.block_number(head.number - window).await.ok()?;
    let elapsed = head.timestamp.checked_sub(previous.timestamp)?;
    (elapsed > 0).then_some(elapsed as f64 / window as f64)
}

pub fn eta_suffix(blocks: u64, block_seconds: Option<f64>) -> String {
    let Some(block_seconds) = block_seconds else {
        return String::new();
    };
    let seconds = (blocks as f64 * block_seconds).round() as u64;
    format!(", ~{}", human_duration(seconds))
}

fn eta_seconds_field(seconds: Option<u64>) -> String {
    seconds.map_or_else(String::new, |seconds| {
        format!(" eta=~{}", human_duration(seconds))
    })
}

fn human_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds.div_ceil(60))
    } else {
        let hours = seconds / 3_600;
        let minutes = (seconds % 3_600).div_ceil(60);
        if minutes == 0 || minutes == 60 {
            format!("{}h", hours + u64::from(minutes == 60))
        } else {
            format!("{hours}h {minutes}m")
        }
    }
}

pub const fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}
