//! Rewards commands.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use clap::Subcommand;
use eyre::Result;

use crate::abi::{self, IRewards};
use crate::rpc::Rpc;

#[derive(Subcommand)]
pub enum RewardsCmd {
    /// Claim accumulated emission rewards
    Claim,
    /// Show claimable emission rewards for an address
    Pending {
        /// Validator address
        address: Address,
    },
    /// Show current reward emission parameters
    Emission,
    /// Show reward distribution history from event logs
    History {
        /// Number of recent events to show (default: 20)
        #[arg(long, default_value = "20")]
        limit: usize,
    },
}

impl RewardsCmd {
    pub async fn run(self, client: &(impl Rpc + Sync), private_key: Option<&str>) -> Result<()> {
        match self {
            Self::Claim => claim(client, private_key).await,
            Self::Pending { address } => pending(client, address).await,
            Self::Emission => emission(client).await,
            Self::History { limit } => history(client, limit).await,
        }
    }
}

async fn claim(client: &(impl Rpc + Sync), private_key: Option<&str>) -> Result<()> {
    let signer = super::require_signer(private_key)?;
    let call = IRewards::claimRewardsCall {};
    let tx_hash = signer
        .send_tx(
            client,
            abi::REWARDS_ADDR,
            call.abi_encode(),
            Default::default(),
        )
        .await?;
    println!("Transaction sent: {tx_hash}");
    Ok(())
}

async fn fetch_pending_rewards(client: &(impl Rpc + Sync), address: Address) -> Result<U256> {
    let call = IRewards::pendingRewardsCall { validator: address };
    let result = client
        .eth_call(abi::REWARDS_ADDR, &call.abi_encode())
        .await?;
    Ok(IRewards::pendingRewardsCall::abi_decode_returns(&result)?)
}

async fn pending(client: &(impl Rpc + Sync), address: Address) -> Result<()> {
    let rewards = fetch_pending_rewards(client, address).await?;
    println!(
        "Claimable emission rewards for {:?}: {} COEN",
        address,
        super::format_unit(rewards)
    );
    Ok(())
}

async fn emission(client: &(impl Rpc + Sync)) -> Result<()> {
    let info = client.outbe_get_emission_info().await?;

    let validator_pct = info["validatorRewardPercent"].as_u64().unwrap_or(0);
    let escrow_addr = info["feeEscrowAddress"].as_str().unwrap_or("N/A");

    println!("=== Reward Emission Parameters ===");
    println!("Validator Cap:      {validator_pct}%");
    println!("Fee Escrow:         {escrow_addr}");
    println!("Settlement Source:  finalized block execution summary artifact");

    Ok(())
}

async fn history(_client: &(impl Rpc + Sync), limit: usize) -> Result<()> {
    println!(
        "Validator reward settlement history is committed through block artifacts; receipt-log history is not exposed yet. Requested limit: {limit}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::mock::{abi_u256, call_map, recording_send_tx_rpc, MockRpc};
    use alloy_sol_types::SolCall;
    use std::collections::HashMap;

    fn pending_rewards_mock(val: U256) -> MockRpc {
        let mut map = HashMap::new();
        map.insert(
            (abi::REWARDS_ADDR, IRewards::pendingRewardsCall::SELECTOR),
            abi_u256(val),
        );
        MockRpc {
            eth_call_map: Some(call_map(map)),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_fetch_pending_rewards_returns_correct_value() {
        let expected = U256::from(500u64) * U256::from(10u64).pow(U256::from(18));
        let mock = pending_rewards_mock(expected);
        let addr: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();

        let result = fetch_pending_rewards(&mock, addr).await.unwrap();
        assert_eq!(result, expected, "pending rewards must match mock value");
    }

    #[tokio::test]
    async fn test_fetch_pending_rewards_zero() {
        let mock = pending_rewards_mock(U256::ZERO);
        let addr: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();

        let result = fetch_pending_rewards(&mock, addr).await.unwrap();
        assert_eq!(result, U256::ZERO);
    }

    #[tokio::test]
    async fn test_fetch_pending_rewards_rpc_error() {
        let mock = MockRpc::default();
        let addr: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        assert!(fetch_pending_rewards(&mock, addr).await.is_err());
    }

    #[tokio::test]
    async fn test_emission_happy() {
        let mock = MockRpc {
            emission_info: Ok(serde_json::json!({
                "validatorRewardPercent": 4,
                "feeEscrowAddress": "0x000000000000000000000000000000000000ee03"
            })),
            ..Default::default()
        };
        // emission() only prints, so verify it doesn't error
        emission(&mock).await.unwrap();
    }

    #[tokio::test]
    async fn test_emission_rpc_error() {
        let mock = MockRpc::default();
        assert!(emission(&mock).await.is_err());
    }

    #[tokio::test]
    async fn test_history_is_currently_informational() {
        let mock = MockRpc::default();
        history(&mock, 20).await.unwrap();
    }

    const TEST_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    #[tokio::test]
    async fn test_claim_sends_tx() {
        let data = IRewards::claimRewardsCall {}.abi_encode();
        let mock = recording_send_tx_rpc(TEST_KEY, abi::REWARDS_ADDR, data, U256::ZERO).unwrap();

        claim(&mock, Some(TEST_KEY)).await.unwrap();
        mock.assert_done();
    }

    #[tokio::test]
    async fn test_claim_no_private_key_errors() {
        let mock = recording_send_tx_rpc(TEST_KEY, abi::REWARDS_ADDR, vec![], U256::ZERO).unwrap();
        let err = claim(&mock, None).await.unwrap_err();
        assert!(
            err.to_string().contains("--private-key required"),
            "expected private-key error, got: {err}"
        );
        assert!(
            mock.recorded_calls().is_empty(),
            "missing signer must not call RPC"
        );
    }
}
