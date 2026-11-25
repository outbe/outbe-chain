//! Chain information commands.

use alloy_primitives::U256;
use clap::Subcommand;
use eyre::Result;

use crate::rpc::Rpc;

#[derive(Subcommand)]
pub enum ChainCmd {
    /// Show detailed chain status including consensus and epoch data.
    Status,
    /// Show finalized consensus status snapshot.
    Consensus,
}

impl ChainCmd {
    pub async fn run(self, client: &(impl Rpc + Sync)) -> Result<()> {
        match self {
            Self::Status => status(client).await,
            Self::Consensus => consensus(client).await,
        }
    }
}

struct ChainInfo {
    block: u64,
    chain_id: u64,
    gas_price: U256,
}

async fn fetch_chain_info(client: &(impl Rpc + Sync)) -> Result<ChainInfo> {
    let block = client.eth_block_number().await?;
    let chain_id = client.eth_chain_id().await?;
    let gas_price = client.eth_gas_price().await?;
    Ok(ChainInfo {
        block,
        chain_id,
        gas_price,
    })
}

async fn status(client: &(impl Rpc + Sync)) -> Result<()> {
    let ci = fetch_chain_info(client).await?;

    println!("=== Chain Status ===");
    println!("Chain ID:       {}", ci.chain_id);
    println!("Block Number:   {}", ci.block);
    println!("Gas Price:      {} unit", ci.gas_price);

    if let Ok(epoch_info) = client.outbe_get_epoch_info().await {
        print_epoch_status(&epoch_info);
    }

    match client.outbe_consensus_status().await {
        Ok(cs) => print_consensus_status(&cs, ci.block),
        Err(_) => {
            println!();
            println!("=== Consensus ===");
            println!("(unavailable - non-validator node or consensus not started)");
        }
    }

    print_block_timing(client, ci.block).await;

    Ok(())
}

fn print_epoch_status(epoch_info: &serde_json::Value) {
    println!();
    println!("=== Epoch ===");
    for (key, label) in [
        ("epochNumber", "Epoch Number:  "),
        ("epochStartTimestamp", "Epoch Start Time:"),
        ("epochStartBlock", "Epoch Start Block:"),
        ("activeValidatorCount", "Active Validators:"),
    ] {
        if let Some(val) = epoch_info.get(key).and_then(|v| v.as_u64()) {
            println!("{label} {val}");
        }
    }
    if let Some(s) = epoch_info.get("totalStaked").and_then(|v| v.as_str()) {
        println!("Total Staked:   {s}");
    }
}

fn print_consensus_status(cs: &serde_json::Value, block_number: u64) {
    println!();
    println!("=== Consensus ===");
    if let Some(v) = cs.get("currentView").and_then(|v| v.as_u64()) {
        println!("Current View:   {v}");
    }
    if let Some(p) = cs.get("connectedPeers").and_then(|v| v.as_u64()) {
        println!("Connected Peers: {p}");
    }
    let active = cs
        .get("isActive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    println!("Active:         {active}");
    let shares = cs
        .get("hasThresholdShares")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    println!("Threshold Shares: {shares}");
    if let Some(f) = cs.get("lastFinalizedBlock").and_then(|v| v.as_u64()) {
        println!("Last Finalized: {f}");
        if block_number > 0 && f > 0 {
            let lag = block_number.saturating_sub(f);
            println!("Finality Lag:   {lag} blocks");
        }
    }
    if let Some(seed) = cs.get("lastVrfSeed").and_then(|v| v.as_str()) {
        println!("VRF Seed:       {seed}");
    }
}

fn parse_block_timestamp(block: &serde_json::Value) -> Option<u64> {
    block
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(|s| u64::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok())
}

async fn print_block_timing(client: &(impl Rpc + Sync), block_number: u64) {
    if block_number < 2 {
        return;
    }
    let (Ok(prev), Ok(curr)) = (
        client.eth_get_block_by_number(block_number - 1).await,
        client.eth_get_block_by_number(block_number).await,
    ) else {
        return;
    };
    if let (Some(t1), Some(t2)) = (parse_block_timestamp(&prev), parse_block_timestamp(&curr)) {
        let block_time = t2.saturating_sub(t1);
        println!();
        println!("=== Timing ===");
        println!("Last Block Time: {block_time}s");
    }
}

async fn consensus(client: &(impl Rpc + Sync)) -> Result<()> {
    let cs = client.outbe_consensus_status().await?;

    println!("=== Consensus Status ===");

    if let Some(v) = cs.get("currentView").and_then(|v| v.as_u64()) {
        println!("Current View:     {v}");
    }
    if let Some(p) = cs.get("connectedPeers").and_then(|v| v.as_u64()) {
        println!("Connected Peers:  {p}");
    }
    let active = cs
        .get("isActive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    println!("Active:           {active}");
    let shares = cs
        .get("hasThresholdShares")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    println!("Threshold Shares: {shares}");
    if let Some(f) = cs.get("lastFinalizedBlock").and_then(|v| v.as_u64()) {
        println!("Last Finalized:   {f}");
    }
    if let Some(seed) = cs.get("lastVrfSeed") {
        if seed.is_null() {
            println!("VRF Seed:         (none)");
        } else if let Some(s) = seed.as_str() {
            println!("VRF Seed:         {s}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::mock::MockRpc;

    #[tokio::test]
    async fn test_fetch_chain_info_returns_correct_values() {
        let mock = MockRpc {
            block_number: Ok(100),
            chain_id: Ok(1337),
            gas_price: Ok(U256::from(alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE)),
            ..Default::default()
        };
        let ci = fetch_chain_info(&mock).await.unwrap();
        assert_eq!(ci.block, 100);
        assert_eq!(ci.chain_id, 1337);
        assert_eq!(
            ci.gas_price,
            U256::from(alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE)
        );
    }

    #[tokio::test]
    async fn test_fetch_chain_info_rpc_error() {
        let mock = MockRpc::default();
        assert!(fetch_chain_info(&mock).await.is_err());
    }

    #[tokio::test]
    async fn test_consensus_happy() {
        let mock = MockRpc {
            consensus_status: Ok(serde_json::json!({
                "currentView": 42,
                "connectedPeers": 5,
                "isActive": true,
                "hasThresholdShares": true,
                "lastFinalizedBlock": 100,
                "lastVrfSeed": "0xabcd"
            })),
            ..Default::default()
        };
        consensus(&mock).await.unwrap();
    }

    #[tokio::test]
    async fn test_consensus_rpc_error() {
        let mock = MockRpc::default();
        assert!(consensus(&mock).await.is_err());
    }
}
