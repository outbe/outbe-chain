//! Slash information and evidence submission commands.

use alloy_primitives::Address;
use alloy_sol_types::SolCall;
use clap::Subcommand;
use eyre::Result;

use crate::abi::{self, ISlashIndicator};
use crate::errors::SlashError;
use crate::rpc::Rpc;

#[derive(Subcommand)]
pub enum SlashCmd {
    /// Show slash info for a validator
    Info {
        /// Validator address
        address: Address,
    },
    /// Show slashing configuration parameters
    Config,
    /// Submit slashing evidence
    Evidence {
        #[command(subcommand)]
        kind: EvidenceCmd,
    },
}

#[derive(Subcommand)]
pub enum EvidenceCmd {
    /// Submit double-proposal evidence
    DoubleProposal {
        /// First signed proposal, hex: `pubkey[48] || bls_signature[96] || proposal_bytes`
        #[arg(long)]
        block1: String,
        /// Second signed proposal, hex, same format as `--block1`
        #[arg(long)]
        block2: String,
    },
    /// Submit conflicting-vote evidence
    ConflictingVote {
        /// First signed vote, hex: `pubkey[48] || bls_signature[96] || proposal_bytes`
        #[arg(long)]
        vote1: String,
        /// Second signed vote, hex, same format as `--vote1`
        #[arg(long)]
        vote2: String,
    },
}

impl SlashCmd {
    pub async fn run(self, client: &(impl Rpc + Sync), private_key: Option<&str>) -> Result<()> {
        match self {
            Self::Info { address } => info(client, address).await,
            Self::Config => config(client).await,
            Self::Evidence { kind } => submit_evidence(client, private_key, kind).await,
        }
    }
}

struct SlashInfo {
    proposer_miss: u64,
    voter_miss: u64,
    felony: u64,
}

async fn fetch_slash_info(client: &(impl Rpc + Sync), address: Address) -> Result<SlashInfo> {
    let proposer_miss: u64 = {
        let call = ISlashIndicator::getProposerMissCountCall { validator: address };
        let result = client
            .eth_call(abi::SLASH_INDICATOR_ADDR, &call.abi_encode())
            .await?;
        ISlashIndicator::getProposerMissCountCall::abi_decode_returns(&result)?
    };

    let voter_miss: u64 = {
        let call = ISlashIndicator::getVoterMissCountCall { validator: address };
        let result = client
            .eth_call(abi::SLASH_INDICATOR_ADDR, &call.abi_encode())
            .await?;
        ISlashIndicator::getVoterMissCountCall::abi_decode_returns(&result)?
    };

    let felony: u64 = {
        let call = ISlashIndicator::getFelonyCountCall { validator: address };
        let result = client
            .eth_call(abi::SLASH_INDICATOR_ADDR, &call.abi_encode())
            .await?;
        ISlashIndicator::getFelonyCountCall::abi_decode_returns(&result)?
    };

    Ok(SlashInfo {
        proposer_miss,
        voter_miss,
        felony,
    })
}

async fn info(client: &(impl Rpc + Sync), address: Address) -> Result<()> {
    let si = fetch_slash_info(client, address).await?;
    println!("Slash info for {:?}:", address);
    println!("  Proposer Miss Count: {}", si.proposer_miss);
    println!("  Voter Miss Count:    {}", si.voter_miss);
    println!("  Felony Count:        {}", si.felony);
    Ok(())
}

async fn submit_evidence(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    kind: EvidenceCmd,
) -> Result<()> {
    let signer = super::require_signer(private_key)?;
    let calldata = match kind {
        EvidenceCmd::DoubleProposal { block1, block2 } => {
            ISlashIndicator::submitDoubleProposalEvidenceCall {
                block1: parse_evidence_hex("block1", &block1)?.into(),
                block2: parse_evidence_hex("block2", &block2)?.into(),
            }
            .abi_encode()
        }
        EvidenceCmd::ConflictingVote { vote1, vote2 } => {
            ISlashIndicator::submitConflictingVoteEvidenceCall {
                vote1: parse_evidence_hex("vote1", &vote1)?.into(),
                vote2: parse_evidence_hex("vote2", &vote2)?.into(),
            }
            .abi_encode()
        }
    };
    let tx_hash = signer
        .send_tx(
            client,
            abi::SLASH_INDICATOR_ADDR,
            calldata,
            Default::default(),
        )
        .await?;
    println!("Transaction sent: {tx_hash}");
    Ok(())
}

fn parse_evidence_hex(label: &'static str, value: &str) -> Result<Vec<u8>> {
    let bytes = hex::decode(value.strip_prefix("0x").unwrap_or(value))?;
    if bytes.is_empty() {
        return Err(SlashError::EmptyEvidence(label).into());
    }
    Ok(bytes)
}

async fn config(client: &(impl Rpc + Sync)) -> Result<()> {
    let cfg = client.outbe_get_slash_config().await?;

    let proposer_misd = cfg["proposerMisdemeanorThreshold"].as_u64().unwrap_or(0);
    let proposer_felony = cfg["proposerFelonyThreshold"].as_u64().unwrap_or(0);
    let voter_misd = cfg["voterMisdemeanorThreshold"].as_u64().unwrap_or(0);
    let voter_felony = cfg["voterFelonyThreshold"].as_u64().unwrap_or(0);
    let slash_pct = cfg["slashAmountPercent"].as_u64().unwrap_or(0);
    let evidence_pct = cfg["evidenceRewardPercent"].as_u64().unwrap_or(0);

    println!("=== Slashing Configuration ===");
    println!("Proposer Misdemeanor Threshold: {proposer_misd}");
    println!("Proposer Felony Threshold:      {proposer_felony}");
    println!("Voter Misdemeanor Threshold:    {voter_misd}");
    println!("Voter Felony Threshold:         {voter_felony}");
    println!("Slash Amount:                   {slash_pct}%");
    println!("Evidence Reward:                {evidence_pct}%");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::mock::{abi_u64, call_map, recording_send_tx_rpc, MockRpc};
    use alloy_primitives::U256;
    use alloy_sol_types::SolCall;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_fetch_slash_info_returns_correct_values() {
        let mut map = HashMap::new();
        map.insert(
            (
                abi::SLASH_INDICATOR_ADDR,
                ISlashIndicator::getProposerMissCountCall::SELECTOR,
            ),
            abi_u64(3),
        );
        map.insert(
            (
                abi::SLASH_INDICATOR_ADDR,
                ISlashIndicator::getVoterMissCountCall::SELECTOR,
            ),
            abi_u64(7),
        );
        map.insert(
            (
                abi::SLASH_INDICATOR_ADDR,
                ISlashIndicator::getFelonyCountCall::SELECTOR,
            ),
            abi_u64(1),
        );
        let mock = MockRpc {
            eth_call_map: Some(call_map(map)),
            ..Default::default()
        };
        let addr: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();

        let si = fetch_slash_info(&mock, addr).await.unwrap();
        assert_eq!(si.proposer_miss, 3);
        assert_eq!(si.voter_miss, 7);
        assert_eq!(si.felony, 1);
    }

    #[tokio::test]
    async fn test_fetch_slash_info_rpc_error() {
        let mock = MockRpc::default();
        let addr: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        assert!(fetch_slash_info(&mock, addr).await.is_err());
    }

    #[tokio::test]
    async fn test_slash_config_happy() {
        let mock = MockRpc {
            slash_config: Ok(serde_json::json!({
                "proposerMisdemeanorThreshold": 10,
                "proposerFelonyThreshold": 50,
                "voterMisdemeanorThreshold": 10,
                "voterFelonyThreshold": 30,
                "slashAmountPercent": 5,
                "evidenceRewardPercent": 10
            })),
            ..Default::default()
        };
        config(&mock).await.unwrap();
    }

    #[tokio::test]
    async fn test_slash_config_rpc_error() {
        let mock = MockRpc::default();
        assert!(config(&mock).await.is_err());
    }

    const TEST_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    #[test]
    fn test_parse_evidence_hex_rejects_empty_and_invalid_inputs() {
        for (label, value, expected) in [
            ("block1", "0x", "evidence must be non-empty"),
            ("block2", "", "evidence must be non-empty"),
            ("vote1", "abc", "Odd"),
            ("vote2", "zz", "Invalid"),
        ] {
            let err = parse_evidence_hex(label, value).unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "label={label} value={value:?}: expected {expected:?}, got {err}"
            );
        }
    }

    #[tokio::test]
    async fn test_submit_double_proposal_sends_tx() {
        let block1 = vec![0xaa, 0xbb];
        let block2 = vec![0xcc, 0xdd];
        let data = ISlashIndicator::submitDoubleProposalEvidenceCall {
            block1: block1.clone().into(),
            block2: block2.clone().into(),
        }
        .abi_encode();
        let mock =
            recording_send_tx_rpc(TEST_KEY, abi::SLASH_INDICATOR_ADDR, data, U256::ZERO).unwrap();

        submit_evidence(
            &mock,
            Some(TEST_KEY),
            EvidenceCmd::DoubleProposal {
                block1: "0xaabb".to_string(),
                block2: "0xccdd".to_string(),
            },
        )
        .await
        .unwrap();
        mock.assert_done();
    }

    #[tokio::test]
    async fn test_submit_double_proposal_no_key_errors() {
        let mock =
            recording_send_tx_rpc(TEST_KEY, abi::SLASH_INDICATOR_ADDR, vec![], U256::ZERO).unwrap();
        let err = submit_evidence(
            &mock,
            None,
            EvidenceCmd::DoubleProposal {
                block1: "0xaabb".into(),
                block2: "0xccdd".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("--private-key required"),
            "expected private-key error, got: {err}"
        );
        assert!(
            mock.recorded_calls().is_empty(),
            "missing signer must not call RPC"
        );
    }

    #[tokio::test]
    async fn test_submit_double_proposal_invalid_inputs_do_not_call_rpc() {
        for (block1, block2, expected) in [
            ("zz", "0xcc", "Invalid"),
            ("abc", "0xcc", "Odd"),
            ("0x", "0xcc", "evidence must be non-empty"),
            ("0xaa", "zz", "Invalid"),
            ("0xaa", "abc", "Odd"),
            ("0xaa", "", "evidence must be non-empty"),
        ] {
            let mock =
                recording_send_tx_rpc(TEST_KEY, abi::SLASH_INDICATOR_ADDR, vec![], U256::ZERO)
                    .unwrap();
            let err = submit_evidence(
                &mock,
                Some(TEST_KEY),
                EvidenceCmd::DoubleProposal {
                    block1: block1.to_string(),
                    block2: block2.to_string(),
                },
            )
            .await
            .unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "block1={block1:?} block2={block2:?}: expected {expected:?}, got {err}"
            );
            assert!(
                mock.recorded_calls().is_empty(),
                "invalid double-proposal evidence must not call RPC"
            );
        }
    }

    #[tokio::test]
    async fn test_submit_conflicting_vote_sends_tx() {
        let vote1 = vec![0x11, 0x22];
        let vote2 = vec![0x33, 0x44];
        let data = ISlashIndicator::submitConflictingVoteEvidenceCall {
            vote1: vote1.clone().into(),
            vote2: vote2.clone().into(),
        }
        .abi_encode();
        let mock =
            recording_send_tx_rpc(TEST_KEY, abi::SLASH_INDICATOR_ADDR, data, U256::ZERO).unwrap();

        submit_evidence(
            &mock,
            Some(TEST_KEY),
            EvidenceCmd::ConflictingVote {
                vote1: "0x1122".into(),
                vote2: "0x3344".into(),
            },
        )
        .await
        .unwrap();
        mock.assert_done();
    }

    #[tokio::test]
    async fn test_submit_conflicting_vote_no_key_errors() {
        let mock =
            recording_send_tx_rpc(TEST_KEY, abi::SLASH_INDICATOR_ADDR, vec![], U256::ZERO).unwrap();
        let err = submit_evidence(
            &mock,
            None,
            EvidenceCmd::ConflictingVote {
                vote1: "0x1122".into(),
                vote2: "0x3344".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("--private-key required"),
            "expected private-key error, got: {err}"
        );
        assert!(
            mock.recorded_calls().is_empty(),
            "missing signer must not call RPC"
        );
    }

    #[tokio::test]
    async fn test_submit_conflicting_vote_invalid_inputs_do_not_call_rpc() {
        for (vote1, vote2, expected) in [
            ("zz", "0x3344", "Invalid"),
            ("abc", "0x3344", "Odd"),
            ("0x", "0x3344", "evidence must be non-empty"),
            ("0x1122", "zz", "Invalid"),
            ("0x1122", "abc", "Odd"),
            ("0x1122", "", "evidence must be non-empty"),
        ] {
            let mock =
                recording_send_tx_rpc(TEST_KEY, abi::SLASH_INDICATOR_ADDR, vec![], U256::ZERO)
                    .unwrap();
            let err = submit_evidence(
                &mock,
                Some(TEST_KEY),
                EvidenceCmd::ConflictingVote {
                    vote1: vote1.to_string(),
                    vote2: vote2.to_string(),
                },
            )
            .await
            .unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "vote1={vote1:?} vote2={vote2:?}: expected {expected:?}, got {err}"
            );
            assert!(
                mock.recorded_calls().is_empty(),
                "invalid conflicting-vote evidence must not call RPC"
            );
        }
    }
}
