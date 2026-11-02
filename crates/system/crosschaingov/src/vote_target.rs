//! The one vote target of the owner address: strict JSON actions on our
//! multi-chain contracts and on the Hyperlane chains.
//!
//! The vote tally runs where sub-calls are not available. So an approved
//! proposal only stores the hash of its payload. Permissionless
//! `applyApproved(proposalId, payload)` runs it later, with the owner address
//! as `msg.sender` on Outbe and its Interchain Account on a remote chain.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_hyperlanecontroller::errors::HyperlaneControllerError;
use outbe_hyperlanecontroller::precompile::IHyperlaneController;
use outbe_hyperlanecontroller::{actions as hyperlane, Executor as _, HyperlaneControllerContract};
use outbe_primitives::addresses::HYPERLANE_CONTROLLER_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_vote::errors::VoteError;
use outbe_vote::handlers::{
    decode_proposal_payload, decode_stored_proposal_payload, TargetExecutionOutcome, VoteTarget,
    VoteTargetContext,
};

use crate::actions::Action;
use crate::executor::Executor;
use crate::schema::CrosschainGovContract;

/// A payload goes to the module that owns its operation.
enum Payload {
    /// An operation on the Hyperlane chains, run by `hyperlanecontroller`.
    Hyperlane(hyperlane::Action),
    /// An operation on our multi-chain contracts.
    Contracts(Action),
}

impl Payload {
    fn decode(payload: &[u8], outbe: u64) -> Result<Self> {
        match hyperlane::Action::decode(payload, outbe) {
            Ok(action) => Ok(Self::Hyperlane(action)),
            Err(_) => Action::decode(payload, outbe).map(Self::Contracts),
        }
    }
}

impl CrosschainGovContract<'_> {
    /// Runs an approved action once, inside one checkpoint.
    pub fn apply_approved(&mut self, proposal_id: U256, payload: &[u8]) -> Result<()> {
        let approved = self.approved_payload.read(&proposal_id)?;
        if approved == B256::ZERO {
            return Err(HyperlaneControllerError::UnknownApprovedAction { proposal_id }.into());
        }
        if keccak256(payload) != approved {
            return Err(HyperlaneControllerError::ApprovedPayloadMismatch { proposal_id }.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            // Consume first. The checkpoint restores it if the action fails.
            self.approved_payload.write(&proposal_id, B256::ZERO)?;
            let deadline = self
                .approved_at
                .read(&proposal_id)?
                .saturating_add(outbe_chain_constants::get_governance_voting_window_blocks());
            if self.storage.block_number()? > deadline {
                return self.emit(IHyperlaneController::ApprovedActionExpired {
                    proposalId: proposal_id,
                });
            }
            let payload = Payload::decode(payload, self.storage.chain_id()?)
                .map_err(|_| VoteError::stored_payload_invalid("CrosschainGov"))?;
            let mut controller = HyperlaneControllerContract::new(self.storage.clone());
            let mut executor = Executor::new(self.storage.clone());
            match payload {
                Payload::Hyperlane(action) => action.apply(&mut controller, &mut executor)?,
                Payload::Contracts(action) => {
                    let calls = action.calls(controller.local_domain()?);
                    for call in calls.local {
                        executor.call_local(call.to, call.value, call.data)?;
                    }
                    if let Some((chain_id, remote)) = calls.remote {
                        executor.call_remote(chain_id, &remote)?;
                    }
                }
            }
            self.emit(IHyperlaneController::ApprovedActionApplied {
                proposalId: proposal_id,
            })
        })
    }
}

/// Validator-only, zero-value vote target. Approval stores the payload hash
/// for `applyApproved`.
pub struct CrosschainVoteTarget;

impl VoteTarget for CrosschainVoteTarget {
    fn target_module(&self) -> Address {
        HYPERLANE_CONTROLLER_ADDRESS
    }

    fn validate(&self, payload: &[u8], context: VoteTargetContext) -> Result<()> {
        decode_proposal_payload(payload)?;
        Payload::decode(payload, context.chain_id).map(|_| ())
    }

    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        payload: &[u8],
        context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        // Stored-payload corruption is an invariant failure, never proposal Error.
        decode_stored_proposal_payload(payload, "CrosschainGov")?;
        Payload::decode(payload, context.chain_id)
            .map_err(|_| VoteError::stored_payload_invalid("CrosschainGov"))?;
        let gov = CrosschainGovContract::new(ctx.storage.clone());
        gov.approved_payload
            .write(&proposal_id, keccak256(payload))?;
        gov.approved_at
            .write(&proposal_id, ctx.block.block_number)?;
        Ok(TargetExecutionOutcome::Applied)
    }
}
