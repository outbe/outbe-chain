//! Epoch information commands.

use alloy_primitives::{keccak256, B256, U256};
use alloy_sol_types::SolCall;
use clap::Subcommand;
use eyre::Result;

use crate::abi::{self, IStaking, IValidatorSet, VALIDATOR_SET_ADDR};
use crate::errors::EpochError;
use crate::rpc::Rpc;

#[derive(Subcommand)]
pub enum EpochCmd {
    /// Show current epoch information
    Info,
    /// Show epoch transition history from event logs
    History {
        /// Number of recent events to show (default: 20)
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Show validator set changes (registrations, activations, exits, etc.)
    Transitions {
        /// Number of recent events to show (default: 50)
        #[arg(long, default_value = "50")]
        limit: usize,
    },
}

impl EpochCmd {
    pub async fn run(self, client: &(impl Rpc + Sync)) -> Result<()> {
        match self {
            Self::Info => info(client).await,
            Self::History { limit } => history(client, limit).await,
            Self::Transitions { limit } => transitions(client, limit).await,
        }
    }
}

struct EpochInfo {
    epoch: u64,
    epoch_start_timestamp: u64,
    epoch_start_block: u64,
    active_count: u64,
    total_staked: U256,
}

async fn fetch_epoch_info(client: &(impl Rpc + Sync)) -> Result<EpochInfo> {
    let epoch: u64 = {
        let call = IValidatorSet::getEpochNumberCall {};
        let result = client
            .eth_call(abi::VALIDATOR_SET_ADDR, &call.abi_encode())
            .await?;
        let decoded = IValidatorSet::getEpochNumberCall::abi_decode_returns(&result)?;
        decoded
            .try_into()
            .map_err(|_| EpochError::EpochNumberOverflow)?
    };

    let epoch_start_timestamp: u64 = {
        let call = IValidatorSet::getEpochStartTimestampCall {};
        let result = client
            .eth_call(abi::VALIDATOR_SET_ADDR, &call.abi_encode())
            .await?;
        IValidatorSet::getEpochStartTimestampCall::abi_decode_returns(&result)?
    };

    let epoch_start_block: u64 = {
        let call = IValidatorSet::getEpochStartBlockCall {};
        let result = client
            .eth_call(abi::VALIDATOR_SET_ADDR, &call.abi_encode())
            .await?;
        IValidatorSet::getEpochStartBlockCall::abi_decode_returns(&result)?
    };

    let active_count: u64 = {
        let call = IValidatorSet::activeValidatorCountCall {};
        let result = client
            .eth_call(abi::VALIDATOR_SET_ADDR, &call.abi_encode())
            .await?;
        IValidatorSet::activeValidatorCountCall::abi_decode_returns(&result)?.into()
    };

    let total_staked: U256 = {
        let call = IStaking::getTotalStakedCall {};
        let result = client
            .eth_call(abi::STAKING_ADDR, &call.abi_encode())
            .await?;
        IStaking::getTotalStakedCall::abi_decode_returns(&result)?
    };

    Ok(EpochInfo {
        epoch,
        epoch_start_timestamp,
        epoch_start_block,
        active_count,
        total_staked,
    })
}

async fn info(client: &(impl Rpc + Sync)) -> Result<()> {
    let ei = fetch_epoch_info(client).await?;
    println!("Epoch Number:       {}", ei.epoch);
    println!("Epoch Start Block:  {}", ei.epoch_start_block);
    println!("Epoch Start Time:   {}", ei.epoch_start_timestamp);
    println!("Active Validators:  {}", ei.active_count);
    println!(
        "Total Staked:       {} COEN",
        super::format_coen_amount(ei.total_staked)
    );
    Ok(())
}

async fn history(client: &(impl Rpc + Sync), limit: usize) -> Result<()> {
    // EpochTransition(uint256 indexed newEpochNumber, uint64 timestamp, uint32 activeValidatorCount)
    let topic = format!(
        "{:?}",
        B256::from(keccak256(b"EpochTransition(uint256,uint64,uint32)",))
    );

    let logs = client
        .eth_get_logs(VALIDATOR_SET_ADDR, &[Some(topic)], "earliest", "latest")
        .await?;

    if logs.is_empty() {
        println!("No epoch transition events found.");
        return Ok(());
    }

    println!(
        "{:<8} {:<14} {:<20} {:<20}",
        "Epoch", "Block", "Timestamp", "Active Validators"
    );
    println!("{}", "-".repeat(62));

    for log in super::recent_logs(&logs, limit) {
        let parsed = super::ParsedLog::from_value(log);
        let epoch = parsed
            .topics
            .get(1)
            .and_then(|s| u64::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok())
            .unwrap_or(0);
        let timestamp = parsed.word_u64(0);
        let active_count = parsed.word_u64(1);

        println!(
            "{:<8} {:<14} {:<20} {:<20}",
            epoch, parsed.block, timestamp, active_count
        );
    }

    Ok(())
}

struct TransitionTopics {
    registered: String,
    activated: String,
    deactivated: String,
    forced_exit: String,
}

impl TransitionTopics {
    fn new() -> Self {
        Self {
            registered: event_topic(b"ValidatorRegistered(address,uint64)"),
            activated: event_topic(b"ValidatorActivated(address)"),
            deactivated: event_topic(b"ValidatorDeactivated(address,uint64)"),
            forced_exit: event_topic(b"ValidatorForcedExit(address,uint64)"),
        }
    }

    fn format_row(&self, parsed: &super::ParsedLog<'_>) -> Option<String> {
        let topic0 = parsed.topic(0);
        let validator = parsed.topic_addr(1);
        if topic0 == self.registered {
            Some(format!(
                "{:<14} {:<14} {:<44} index={}",
                parsed.block,
                "REGISTERED",
                validator,
                parsed.word_u64(0)
            ))
        } else if topic0 == self.activated {
            Some(format!(
                "{:<14} {:<14} {:<44}",
                parsed.block, "ACTIVATED", validator
            ))
        } else if topic0 == self.deactivated || topic0 == self.forced_exit {
            let label = if topic0 == self.forced_exit {
                "FORCED_EXIT"
            } else {
                "DEACTIVATED"
            };
            Some(format!(
                "{:<14} {:<14} {:<44} at_height={}",
                parsed.block,
                label,
                validator,
                parsed.word_u64(0)
            ))
        } else {
            None
        }
    }
}

fn event_topic(signature: &[u8]) -> String {
    format!("{:?}", B256::from(keccak256(signature)))
}

async fn transitions(client: &(impl Rpc + Sync), limit: usize) -> Result<()> {
    let topics = TransitionTopics::new();
    // Fetch all logs from ValidatorSet (no topic filter to get all event types)
    let all_logs = client
        .eth_get_logs(VALIDATOR_SET_ADDR, &[], "earliest", "latest")
        .await?;

    if all_logs.is_empty() {
        println!("No validator transition events found.");
        return Ok(());
    }

    println!("{:<14} {:<14} {:<44} Details", "Block", "Type", "Validator");
    println!("{}", "-".repeat(90));

    for log in super::recent_logs(&all_logs, limit) {
        let parsed = super::ParsedLog::from_value(log);
        if let Some(line) = topics.format_row(&parsed) {
            println!("{line}");
        }
        // This loop skips all other ValidatorSet events. `history` shows
        // `EpochTransition`. No command shows `ConsensusSetUpdated`,
        // `ValidatorJailed` or `ValidatorUnjailed`.
    }

    Ok(())
}

#[cfg(test)]
use super::format_topic_addr;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_topic_addr_standard_padded() {
        let topic = "0x000000000000000000000000abcdef0123456789abcdef0123456789abcdef01";
        assert_eq!(
            format_topic_addr(topic),
            "0xabcdef0123456789abcdef0123456789abcdef01"
        );
    }

    #[test]
    fn test_format_topic_addr_no_prefix() {
        let topic = "000000000000000000000000abcdef0123456789abcdef0123456789abcdef01";
        assert_eq!(
            format_topic_addr(topic),
            "0xabcdef0123456789abcdef0123456789abcdef01"
        );
    }

    #[test]
    fn test_format_topic_addr_zero_address() {
        let topic = "0x0000000000000000000000000000000000000000000000000000000000000000";
        assert_eq!(
            format_topic_addr(topic),
            "0x0000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn test_format_topic_addr_short_string_returned_verbatim() {
        assert_eq!(format_topic_addr("0xabc"), "0xabc");
        assert_eq!(format_topic_addr(""), "");
    }

    #[test]
    fn test_format_topic_addr_exactly_40_no_prefix() {
        let topic = "abcdef0123456789abcdef0123456789abcdef01";
        assert_eq!(
            format_topic_addr(topic),
            "0xabcdef0123456789abcdef0123456789abcdef01"
        );
    }

    // --- Mock RPC tests ---
    use crate::rpc::mock::{abi_u256, abi_u64, call_map, MockRpc};
    use alloy_sol_types::SolCall;
    use std::collections::HashMap;

    fn epoch_info_mock(
        epoch: u64,
        ts: u64,
        start_block: u64,
        active: u64,
        staked: U256,
    ) -> MockRpc {
        let mut map = HashMap::new();
        map.insert(
            (
                abi::VALIDATOR_SET_ADDR,
                IValidatorSet::getEpochNumberCall::SELECTOR,
            ),
            abi_u64(epoch),
        );
        map.insert(
            (
                abi::VALIDATOR_SET_ADDR,
                IValidatorSet::getEpochStartTimestampCall::SELECTOR,
            ),
            abi_u64(ts),
        );
        map.insert(
            (
                abi::VALIDATOR_SET_ADDR,
                IValidatorSet::getEpochStartBlockCall::SELECTOR,
            ),
            abi_u64(start_block),
        );
        map.insert(
            (
                abi::VALIDATOR_SET_ADDR,
                IValidatorSet::activeValidatorCountCall::SELECTOR,
            ),
            abi_u64(active),
        );
        map.insert(
            (abi::STAKING_ADDR, IStaking::getTotalStakedCall::SELECTOR),
            abi_u256(staked),
        );
        MockRpc {
            eth_call_map: Some(call_map(map)),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_fetch_epoch_info_returns_correct_values() {
        let staked = U256::from(1_000_000_000u64);
        let mock = epoch_info_mock(5, 1700000000, 1234, 3, staked);

        let result = fetch_epoch_info(&mock).await.unwrap();
        assert_eq!(result.epoch, 5);
        assert_eq!(result.epoch_start_timestamp, 1700000000);
        assert_eq!(result.epoch_start_block, 1234);
        assert_eq!(result.active_count, 3);
        assert_eq!(result.total_staked, staked);
    }

    #[tokio::test]
    async fn test_fetch_epoch_info_zero_epoch() {
        let mock = epoch_info_mock(0, 0, 0, 0, U256::ZERO);
        let result = fetch_epoch_info(&mock).await.unwrap();
        assert_eq!(result.epoch, 0);
        assert_eq!(result.active_count, 0);
        assert_eq!(result.total_staked, U256::ZERO);
    }

    #[tokio::test]
    async fn test_fetch_epoch_info_rpc_error() {
        let mock = MockRpc::default();
        assert!(fetch_epoch_info(&mock).await.is_err());
    }

    #[tokio::test]
    async fn test_history_empty_logs() {
        let mock = MockRpc {
            logs: Ok(vec![]),
            ..Default::default()
        };
        history(&mock, 20).await.unwrap();
    }

    #[tokio::test]
    async fn test_transitions_empty_logs() {
        let mock = MockRpc {
            logs: Ok(vec![]),
            ..Default::default()
        };
        transitions(&mock, 50).await.unwrap();
    }

    #[tokio::test]
    async fn test_transitions_rpc_error() {
        let mock = MockRpc::default();
        assert!(transitions(&mock, 50).await.is_err());
    }
}
