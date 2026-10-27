//! Generic on-chain vote proposal commands.

use std::path::PathBuf;

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use clap::Subcommand;
use eyre::{Result, WrapErr};

use crate::abi::{IVote, VOTE_ADDRESS};
use crate::errors::VoteError;
use crate::rpc::Rpc;

#[derive(Subcommand)]
pub enum VoteCmd {
    /// Create a proposal (active validator only).
    Propose {
        /// Target system module precompile address.
        #[arg(long)]
        target_module: Address,
        /// Target JSON; standard targets may include votingWindowBlocks.
        /// Update plans derive their windows and activation height from protocol policy.
        #[arg(
            long,
            conflicts_with = "payload_file",
            required_unless_present = "payload_file"
        )]
        payload: Option<String>,
        /// Read the JSON payload from a file instead of the command line.
        #[arg(long, value_name = "PATH", conflicts_with = "payload")]
        payload_file: Option<PathBuf>,
    },
    /// Cast a vote on a pending proposal (active validator only).
    Cast {
        /// Proposal id.
        #[arg(long)]
        proposal_id: U256,
        /// Vote yes.
        #[arg(long, group = "vote")]
        yes: bool,
        /// Vote no.
        #[arg(long, group = "vote")]
        no: bool,
    },
    /// Show proposal status.
    Status {
        /// Vote proposal id.
        #[arg(long)]
        proposal_id: U256,
    },
}

impl VoteCmd {
    pub async fn run(self, client: &(impl Rpc + Sync), private_key: Option<&str>) -> Result<()> {
        match self {
            Self::Propose {
                target_module,
                payload,
                payload_file,
            } => {
                let payload = resolve_proposal_payload(payload, payload_file)?;
                propose(client, private_key, target_module, payload).await
            }
            Self::Cast {
                proposal_id,
                yes,
                no,
            } => {
                let approve = match (yes, no) {
                    (true, false) => true,
                    (false, true) => false,
                    (true, true) => {
                        return Err(VoteError::ConflictingVoteDirection.into());
                    }
                    (false, false) => return Err(VoteError::MissingVoteDirection.into()),
                };
                cast_vote(client, private_key, proposal_id, approve).await
            }
            Self::Status { proposal_id } => status(client, proposal_id).await,
        }
    }
}

fn resolve_proposal_payload(
    payload: Option<String>,
    payload_file: Option<PathBuf>,
) -> Result<String> {
    match (payload, payload_file) {
        (Some(payload), None) => Ok(payload),
        (None, Some(path)) => std::fs::read_to_string(&path)
            .wrap_err_with(|| format!("read proposal payload from {}", path.display())),
        _ => Err(VoteError::MissingOrDuplicatePayloadSource.into()),
    }
}

/// Voting duration belongs to Vote, not the target module's payload schema.
/// Keep legacy payload bytes intact when no duration is supplied.
fn split_proposal_json(payload: String) -> Result<(String, Option<u64>)> {
    let mut value: serde_json::Value =
        serde_json::from_str(&payload).wrap_err("payload must be valid JSON")?;
    let Some(window) = value
        .as_object_mut()
        .and_then(|object| object.remove("votingWindowBlocks"))
    else {
        return Ok((payload, None));
    };
    let window = window
        .as_u64()
        .filter(|window| *window > 0)
        .ok_or(VoteError::InvalidVotingWindowBlocks)?;
    Ok((serde_json::to_string(&value)?, Some(window)))
}

async fn fetch_vote_proposal(
    client: &(impl Rpc + Sync),
    proposal_id: U256,
    block_tag: &str,
) -> Result<IVote::ProposalInfo> {
    let call = IVote::getProposalCall {
        proposalId: proposal_id,
    };
    let ret = client
        .eth_call_at(VOTE_ADDRESS, &call.abi_encode(), block_tag)
        .await
        .wrap_err("vote getProposal eth_call failed")?;
    IVote::getProposalCall::abi_decode_returns(&ret).wrap_err("failed to decode vote proposal")
}

async fn propose(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    target_module: Address,
    payload: String,
) -> Result<()> {
    let (payload, voting_window_blocks) = split_proposal_json(payload)?;
    let payload = prepare_enclave_upgrade_payload(client, target_module, payload).await?;
    let signer = super::require_signer(private_key)?;

    let data = match voting_window_blocks {
        Some(window) => IVote::createProposalWithVotingWindowCall {
            targetModule: target_module,
            payload,
            votingWindowBlocks: window,
        }
        .abi_encode(),
        None => IVote::createProposalCall {
            targetModule: target_module,
            payload,
        }
        .abi_encode(),
    };
    let tx_hash = signer
        .send_tx(client, VOTE_ADDRESS, data, U256::ZERO)
        .await?;
    println!("Proposal transaction sent: {tx_hash} (target {target_module:?})");
    Ok(())
}

async fn prepare_enclave_upgrade_payload(
    client: &(impl Rpc + Sync),
    target: Address,
    payload: String,
) -> Result<String> {
    use outbe_primitives::{
        addresses::{TEE_REGISTRY_ADDRESS, UPDATE_ADDRESS},
        tee_attestation_v1::TeePolicyV1,
        tee_registry_abi_v1::ITeeRegistryV1,
    };
    if target != UPDATE_ADDRESS {
        return Ok(payload);
    }
    let value: serde_json::Value = serde_json::from_str(&payload)?;
    let action = outbe_update::payload::UpdateVoteAction::from_value(&value)?;
    let Some(update) = action.plan() else {
        return Ok(payload);
    };
    update.validate()?;
    if update.mrenclave.is_none() {
        return Ok(payload);
    }
    let finalized = client.eth_get_finalized_block().await?;
    let tag = finalized
        .get("number")
        .and_then(serde_json::Value::as_str)
        .ok_or(VoteError::FinalizedBlockMissingNumber)?;
    let bytes = client
        .eth_call_at(
            TEE_REGISTRY_ADDRESS,
            &ITeeRegistryV1::activePolicyV1Call {}.abi_encode(),
            tag,
        )
        .await?;
    let canonical = ITeeRegistryV1::activePolicyV1Call::abi_decode_returns(&bytes)?;
    let policy =
        TeePolicyV1::decode_canonical(&canonical).map_err(VoteError::InvalidFinalizedPolicy)?;
    if policy.measurement_rules.len() != 1
        || update.mrenclave == Some(policy.measurement_rules[0].mrenclave)
    {
        return Err(VoteError::InvalidEnclaveUpgradeMeasurement.into());
    }
    let chain_id = client.eth_chain_id().await?;
    if policy.chain_id != U256::from(chain_id).to_be_bytes() {
        return Err(VoteError::TeePolicyChainMismatch.into());
    }
    println!("Enclave upgrade proposal: {payload}");
    Ok(payload)
}

async fn cast_vote(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    proposal_id: U256,
    approve: bool,
) -> Result<()> {
    let signer = super::require_signer(private_key)?;
    let call = IVote::castVoteCall {
        proposalId: proposal_id,
        approve,
    };
    let tx_hash = signer
        .send_tx(client, VOTE_ADDRESS, call.abi_encode(), U256::ZERO)
        .await?;
    println!("Vote transaction sent: {tx_hash} (proposal {proposal_id}, approve={approve})");
    Ok(())
}

async fn status(client: &(impl Rpc + Sync), proposal_id: U256) -> Result<()> {
    // Both views must use one height when a replacement window closes between RPCs.
    let block_tag = format!("0x{:x}", client.eth_block_number().await?);
    let proposal = fetch_vote_proposal(client, proposal_id, &block_tag).await?;
    let call = IVote::getProposalLifecycleCall {
        proposalId: proposal_id,
    };
    let bytes = client
        .eth_call_at(VOTE_ADDRESS, &call.abi_encode(), &block_tag)
        .await
        .wrap_err("vote getProposalLifecycle eth_call failed")?;
    let lifecycle = IVote::getProposalLifecycleCall::abi_decode_returns(&bytes)
        .wrap_err("failed to decode vote lifecycle")?;
    print_vote_proposal("Proposal", &proposal, &lifecycle);
    Ok(())
}

fn proposal_status_label(status: IVote::ProposalStatus) -> &'static str {
    match status {
        IVote::ProposalStatus::Pending => "pending",
        IVote::ProposalStatus::Approved => "approved",
        IVote::ProposalStatus::Expired => "expired",
        IVote::ProposalStatus::Error => "error",
        IVote::ProposalStatus::Canceled => "canceled",
        _ => "unknown",
    }
}

fn print_vote_proposal(
    label: &str,
    proposal: &IVote::ProposalInfo,
    lifecycle: &IVote::ProposalLifecycleInfo,
) {
    let proposal_id = proposal.proposalId;
    let status = proposal_status_label(proposal.status);
    let deadline = proposal.votingDeadlineHeight;
    let yes = proposal.state.yes;
    let no = proposal.state.no;
    let duration = deadline.saturating_sub(proposal.createdHeight);
    let phase = match lifecycle.phase {
        IVote::ProposalPhase::Voting => "voting",
        IVote::ProposalPhase::Replace => "replace",
        IVote::ProposalPhase::Activation => "activation",
        IVote::ProposalPhase::Finished => "finished",
        _ => "unknown",
    };
    let kind = match lifecycle.kind {
        IVote::ProposalKind::Standard => "standard",
        IVote::ProposalKind::Primary => "primary",
        IVote::ProposalKind::Cancel => "cancel",
        IVote::ProposalKind::Replacement => "replacement",
        IVote::ProposalKind::LegacyPlan => "legacy_plan",
        _ => "unknown",
    };

    println!(
        "{label} #{proposal_id}: target={:?} status={status} votingWindowBlocks={duration} deadline={deadline} votes={yes}/{no} kind={kind} phase={phase} parent={} replaceEnd={} activationHeight={}",
        proposal.targetModule, lifecycle.parentProposalId,
        lifecycle.replaceEnd, lifecycle.activationHeight
    );
    println!("  payload: {}", proposal.payload);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::mock::MockRpc;
    use crate::Cli;
    use clap::Parser;
    use outbe_primitives::addresses::UPDATE_ADDRESS;

    const SAMPLE_PAYLOAD: &str = r#"{"version":"1.2","info":"notes"}"#;

    #[test]
    fn proposal_json_duration_is_vote_metadata_and_legacy_bytes_are_preserved() {
        let raw = "{\n  \"version\": \"1.2\", \"activationHeight\": 2000\n}";
        assert_eq!(split_proposal_json(raw.into()).unwrap(), (raw.into(), None));
        for window in [1, 1000, 30000, 86400] {
            let mut value: serde_json::Value = serde_json::from_str(raw).unwrap();
            value["votingWindowBlocks"] = serde_json::json!(window);
            let (payload, actual) = split_proposal_json(value.to_string()).unwrap();
            assert_eq!(actual, Some(window));
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&payload).unwrap(),
                serde_json::from_str::<serde_json::Value>(raw).unwrap()
            );
        }
        for invalid in [
            "0",
            "-1",
            "1.5",
            "null",
            "true",
            "\"1000\"",
            "18446744073709551616",
        ] {
            let err =
                split_proposal_json(format!("{{\"votingWindowBlocks\":{invalid}}}")).unwrap_err();
            assert!(err.to_string().contains("votingWindowBlocks"), "{err}");
        }
    }

    #[test]
    fn test_cli_rejects_inline_and_file_payload_together() {
        let cli = Cli::try_parse_from([
            "outbe-cli",
            "vote",
            "propose",
            "--target-module",
            "0x000000000000000000000000000000000000EE00",
            "--payload",
            SAMPLE_PAYLOAD,
            "--payload-file",
            "proposal.json",
        ]);
        assert!(cli.is_err());
    }

    #[tokio::test]
    async fn propose_rejects_invalid_json_payload() {
        let mock = MockRpc::default();
        let err = VoteCmd::Propose {
            target_module: UPDATE_ADDRESS,
            payload: Some("not-json".to_string()),
            payload_file: None,
        }
        .run(
            &mock,
            Some("0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("valid JSON"));
    }

    #[tokio::test]
    async fn live_update_rejects_caller_selected_activation_height() {
        let rpc = MockRpc::default();
        let payload = r#"{"version":"1.2","activationHeight":1000}"#;
        assert!(
            prepare_enclave_upgrade_payload(&rpc, UPDATE_ADDRESS, payload.into())
                .await
                .is_err()
        );
        let replacement =
            r#"{"replaceProposalId":"0x1","update":{"version":"1.2","activationHeight":1000}}"#;
        assert!(
            prepare_enclave_upgrade_payload(&rpc, UPDATE_ADDRESS, replacement.into())
                .await
                .is_err()
        );
    }
}
