use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use eyre::{ensure, eyre, Result};
use serde_json::{json, Value};

use super::{
    account, checkpoint_json, finalize, observe, receipt_identity, retain, unchanged_bond, Account,
    Observation,
};
use crate::internal::{addresses, eth};
use crate::world::rpc::TxOutcome;
use crate::world::World;

#[derive(Clone, Copy)]
pub(super) enum DeactivationRejection {
    Unauthorized,
    Repeated,
}

impl DeactivationRejection {
    pub(super) fn status(self) -> u8 {
        match self {
            Self::Unauthorized => 2,
            Self::Repeated => 3,
        }
    }
}

/// Canonical receipt, transaction and block of one rejected transaction.
pub(super) struct RejectionEvidence<'a> {
    pub(super) outcome: &'a TxOutcome,
    pub(super) transaction: &'a Value,
    pub(super) block: &'a Value,
}

/// Proves canonical inclusion and failure of the intended transaction. Receipt
/// evidence does not prove an exact revert reason. Production tests cover that
/// contract separately. Finalized block-boundary observations check atomicity.
pub(super) fn verify_deactivation_rejection(
    evidence: &RejectionEvidence<'_>,
    before: &Account,
    caller: Address,
    rejection: DeactivationRejection,
) -> Result<()> {
    let (height, block_hash) = receipt_identity(evidence.outcome, false)?;
    ensure!(
        before.status == rejection.status()
            && before.participant
            && before.has_share
            && !before.stake.is_zero(),
        "deactivation rejection is outside its bonded lifecycle phase"
    );
    // ValidatorSet authorizes the config owner or validator itself. Delegated
    // operational keys do not authorize this call. Check the actor independently
    // of receipt status. No diagnostic RPC is needed for this identity check.
    ensure!(
        (caller == before.address) == matches!(rejection, DeactivationRejection::Repeated),
        "deactivation rejection used the wrong actor"
    );
    let hash: B256 = evidence.outcome.transaction_hash.parse()?;
    let index: U256 = serde_json::from_value(evidence.outcome.receipt["transactionIndex"].clone())?;
    let index: usize = index
        .try_into()
        .map_err(|_| eyre!("receipt transaction index overflow"))?;
    let transactions = evidence.block["transactions"]
        .as_array()
        .ok_or_else(|| eyre!("canonical block omitted transactions"))?;
    let included_hash: B256 = serde_json::from_value(
        transactions
            .get(index)
            .ok_or_else(|| eyre!("receipt transaction index is outside its block"))?
            .clone(),
    )?;
    ensure!(
        serde_json::from_value::<B256>(evidence.transaction["hash"].clone())? == hash
            && serde_json::from_value::<B256>(evidence.transaction["blockHash"].clone())?
                == block_hash
            && serde_json::from_value::<U256>(evidence.transaction["blockNumber"].clone())?
                == U256::from(height)
            && serde_json::from_value::<U256>(evidence.transaction["transactionIndex"].clone())?
                == U256::from(index)
            && serde_json::from_value::<B256>(evidence.block["hash"].clone())? == block_hash
            && serde_json::from_value::<U256>(evidence.block["number"].clone())?
                == U256::from(height)
            && included_hash == hash,
        "rejection transaction is not at its canonical receipt position"
    );
    let input = Bytes::from(
        eth::IValidatorSet::deactivateValidatorCall {
            validatorAddress: before.address,
        }
        .abi_encode(),
    );
    ensure!(
        serde_json::from_value::<Address>(evidence.transaction["from"].clone())? == caller
            && serde_json::from_value::<Address>(evidence.transaction["to"].clone())?
                == addresses::VS_ADDR
            && serde_json::from_value::<Bytes>(evidence.transaction["input"].clone())? == input
            && serde_json::from_value::<U256>(evidence.transaction["value"].clone())? == U256::ZERO,
        "rejection transaction changed actor, target, calldata or value"
    );
    Ok(())
}

pub(super) fn finalized_deactivation_rejection(
    world: &mut World,
    phase: &str,
    outcome: &TxOutcome,
    caller: Address,
    rejection: DeactivationRejection,
) -> Result<Observation> {
    let (checkpoint, after) = finalize(world, phase, outcome, false, 30)?;
    let parent_height = checkpoint
        .height
        .checked_sub(1)
        .ok_or_else(|| eyre!("deactivation receipt cannot be in genesis"))?;
    let port = world.validators.primary_port();
    let parent = world.rpc.checkpoint_at(port, parent_height)?;
    let before = observe(world, &format!("{phase}_parent"), parent)?;
    let url = world.rpc.url(port);
    let transaction = eth::raw_json_result(
        &url,
        "eth_getTransactionByHash",
        json!([outcome.transaction_hash]),
    )?;
    let block = eth::raw_json_result(
        &url,
        "eth_getBlockByNumber",
        json!([format!("0x{:x}", checkpoint.height), false]),
    )?;
    ensure!(
        serde_json::from_value::<B256>(block["parentHash"].clone())? == parent.block_hash,
        "rejection block parent differs from the shared finalized prestate anchor"
    );
    retain(
        world,
        &format!("{phase}_canonical_rejection"),
        json!({"checkpoint": checkpoint_json(checkpoint), "parent": checkpoint_json(parent),
            "caller": caller, "validator": account(&before)?.address,
            "proof_kind": "canonical_failed_receipt_and_block_boundary_state",
            "exact_revert_reason_proven": false, "transaction": transaction, "block": block}),
    );
    verify_deactivation_rejection(
        &RejectionEvidence {
            outcome,
            transaction: &transaction,
            block: &block,
        },
        account(&before)?,
        caller,
        rejection,
    )?;
    unchanged_bond(account(&before)?, account(&after)?, rejection.status())?;
    // Fence historical state reads against a changed canonical view,
    // and retain the same complete owned cohort after the proof.
    observe(world, &format!("{phase}_parent_verified"), parent)?;
    observe(world, &format!("{phase}_verified"), checkpoint)
}
