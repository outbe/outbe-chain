//! The vote target of the Hyperlane chains: strict JSON actions that add a
//! chain to the validator sync or remove it. A chain id is also the Hyperlane
//! domain of that chain.
//!
//! The vote tally runs where sub-calls are not available. So an approved
//! proposal only stores the hash of its payload. Permissionless
//! `applyApproved(proposalId, payload)` runs it later. Its writes to the ICA
//! router and the ISMs are owner calls through `crosschainbridge`.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::addresses::HYPERLANE_CONTROLLER_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_vote::errors::VoteError;
use outbe_vote::handlers::{
    decode_proposal_payload, decode_stored_proposal_payload, TargetExecutionOutcome, VoteTarget,
    VoteTargetContext,
};
use serde::Deserialize;

use crate::errors::HyperlaneControllerError;
use crate::precompile::IHyperlaneController;
use crate::schema::HyperlaneControllerContract;

/// One action, as strict JSON with an `operation` field.
#[derive(Debug, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Action {
    /// Puts `chain_id` under the validator sync: the Outbe ICA router enrolls
    /// `ica_router` (the ICA router on that chain), then the owner accepts
    /// the ownership of the ISM there and starts to sync the Outbe validators
    /// to it.
    AddHyperlaneChain {
        chain_id: u32,
        ica_router: Address,
        ism: Address,
        hook: Address,
    },
    /// Stops the validator sync to `chain_id`.
    RemoveHyperlaneChain { chain_id: u32 },
}

impl Action {
    /// Decodes and checks `payload`. `outbe` is the chain id of this chain.
    fn decode(payload: &[u8], outbe: u64) -> Result<Self> {
        // Decode the original bytes so duplicate fields are rejected.
        let action: Self =
            serde_json::from_slice(payload).map_err(|_| VoteError::InvalidPayload)?;
        let remote = |chain_id: &u32| *chain_id != 0 && u64::from(*chain_id) != outbe;
        let valid = match &action {
            Self::AddHyperlaneChain {
                chain_id,
                ica_router,
                ism,
                hook,
            } => remote(chain_id) && ![ica_router, ism, hook].iter().any(|a| a.is_zero()),
            Self::RemoveHyperlaneChain { chain_id } => remote(chain_id),
        };
        if !valid {
            return Err(VoteError::InvalidPayload.into());
        }
        Ok(action)
    }

    fn apply(&self, controller: &mut HyperlaneControllerContract<'_>) -> Result<()> {
        match self {
            Self::AddHyperlaneChain {
                chain_id,
                ica_router,
                ism,
                hook,
            } => {
                controller.enroll_ica_router(*chain_id, *ica_router)?;
                controller.add_domain(*chain_id, *ism, *hook)
            }
            Self::RemoveHyperlaneChain { chain_id } => controller.remove_domain(*chain_id),
        }
    }
}

impl HyperlaneControllerContract<'_> {
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
            let action = Action::decode(payload, self.storage.chain_id()?)
                .map_err(|_| VoteError::stored_payload_invalid("HyperlaneController"))?;
            action.apply(self)?;
            self.emit(IHyperlaneController::ApprovedActionApplied {
                proposalId: proposal_id,
            })
        })
    }
}

/// Validator-only, zero-value vote target. Approval stores the payload hash
/// for `applyApproved`.
pub struct HyperlaneVoteTarget;

impl VoteTarget for HyperlaneVoteTarget {
    fn target_module(&self) -> Address {
        HYPERLANE_CONTROLLER_ADDRESS
    }

    fn validate(&self, payload: &[u8], context: VoteTargetContext) -> Result<()> {
        decode_proposal_payload(payload)?;
        Action::decode(payload, context.chain_id).map(|_| ())
    }

    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        payload: &[u8],
        context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        // Stored-payload corruption is an invariant failure, never proposal Error.
        decode_stored_proposal_payload(payload, "HyperlaneController")?;
        Action::decode(payload, context.chain_id)
            .map_err(|_| VoteError::stored_payload_invalid("HyperlaneController"))?;
        let controller = HyperlaneControllerContract::new(ctx.storage.clone());
        controller
            .approved_payload
            .write(&proposal_id, keccak256(payload))?;
        controller
            .approved_at
            .write(&proposal_id, ctx.block.block_number)?;
        Ok(TargetExecutionOutcome::Applied)
    }
}
