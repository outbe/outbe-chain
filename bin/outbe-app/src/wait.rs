use std::time::Duration;

use alloy_primitives::Address;
use eyre::{bail, ensure, Result};

use crate::{
    progress::Progress,
    rpc::{Block, RpcClient},
};

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
    loop {
        let remote_block = remote.finalized_block().await?;
        let local_block = local.finalized_block().await?;
        let peers = local.peer_count().await.unwrap_or(0);
        let remaining = remote_block.number.saturating_sub(local_block.number);
        let block_seconds = estimate_block_seconds(remote, &remote_block).await;
        progress.update(&format!(
            "sync h={}/{} left={}{} p2p={}",
            local_block.number,
            remote_block.number,
            remaining,
            eta_field(remaining, block_seconds),
            peers
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

pub async fn wait_for_signer(
    local: &RpcClient,
    remote: &RpcClient,
    validator: Address,
    progress: &mut Progress,
) -> Result<u64> {
    loop {
        let status = local.consensus_status().await?;
        let record = remote.validator(validator).await?;
        let remote_block = remote.finalized_block().await?;
        let remaining = status
            .next_planned_activation_height
            .saturating_sub(remote_block.number);
        let block_seconds = estimate_block_seconds(remote, &remote_block).await;
        progress.update(&format!(
            "DKG mode={} shares={} signers={} activation={} left={}{}",
            status.mode,
            yes_no(status.has_threshold_shares),
            status.connected_peers,
            status.next_planned_activation_height,
            remaining,
            eta_field(remaining, block_seconds)
        ));
        if status.is_ready_signer(&record) {
            wait_for_sync(local, remote, progress).await?;
            return Ok(status.next_planned_activation_height);
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

fn eta_field(blocks: u64, block_seconds: Option<f64>) -> String {
    let Some(block_seconds) = block_seconds else {
        return String::new();
    };
    let seconds = (blocks as f64 * block_seconds).round() as u64;
    format!(" eta=~{}", human_duration(seconds))
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
