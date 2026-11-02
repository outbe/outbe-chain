//! The one vote target of `0x...EE14`.
//!
//! A payload is strict JSON with an `operation` field. The vote target reads
//! `operation`. `addHyperlaneChain` and `removeHyperlaneChain` go to
//! `outbe-hyperlanecontroller`. Every other operation goes to
//! [`crate::actions`]. The selected module checks the payload when the
//! proposal is created, and runs it on `applyApproved`.
//!
//! The vote tally runs where sub-calls are not available. So an approved
//! proposal only stores the hash of its payload. Permissionless
//! `applyApproved(proposalId, payload)` runs it later, with `0x...EE14` as
//! `msg.sender` on Outbe and its Interchain Account on a remote chain.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_hyperlanecontroller::abi::ICrosschainGov;
use outbe_hyperlanecontroller::HyperlaneAction;
use outbe_primitives::addresses::CROSSCHAIN_GOV_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_vote::errors::VoteError;
use outbe_vote::handlers::{
    decode_proposal_payload, decode_stored_proposal_payload, TargetExecutionOutcome, VoteTarget,
    VoteTargetContext,
};
use serde::Deserialize;

use crate::actions::Action;
use crate::errors::CrosschainGovError;
use crate::schema::CrosschainGov;

/// The label of this module in stored-payload invariant errors.
const MODULE: &str = "CrosschainGov";

/// The `operation` field of a payload. Other fields are read later by the
/// selected module.
#[derive(Deserialize)]
struct Tag {
    operation: String,
}

/// A decoded payload and the module that runs it.
#[derive(Debug)]
pub(crate) enum Route {
    Hyperlane(HyperlaneAction),
    Gov(Action),
}

impl Route {
    /// Reads `operation` and decodes the payload in the selected module.
    pub(crate) fn decode(payload: &[u8], outbe: u64) -> Result<Self> {
        let Tag { operation } =
            serde_json::from_slice(payload).map_err(|_| VoteError::InvalidPayload)?;
        if HyperlaneAction::OPERATIONS.contains(&operation.as_str()) {
            HyperlaneAction::decode(payload, outbe).map(Self::Hyperlane)
        } else {
            Action::decode(payload, outbe).map(Self::Gov)
        }
    }

    fn apply(&self, gov: &mut CrosschainGov<'_>) -> Result<()> {
        let mut hyperlane = gov.hyperlane();
        match self {
            Self::Hyperlane(action) => action.apply(&mut hyperlane, gov),
            Self::Gov(action) => action.apply(hyperlane.local_domain()?, gov),
        }
    }
}

impl CrosschainGov<'_> {
    /// Runs an approved action once, inside one checkpoint.
    pub fn apply_approved(&mut self, proposal_id: U256, payload: &[u8]) -> Result<()> {
        let approved = self.approved_payload.read(&proposal_id)?;
        if approved == B256::ZERO {
            return Err(CrosschainGovError::UnknownApprovedAction { proposal_id }.into());
        }
        if keccak256(payload) != approved {
            return Err(CrosschainGovError::ApprovedPayloadMismatch { proposal_id }.into());
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
                return self.emit(ICrosschainGov::ApprovedActionExpired {
                    proposalId: proposal_id,
                });
            }
            let route = Route::decode(payload, self.storage.chain_id()?)
                .map_err(|_| VoteError::stored_payload_invalid(MODULE))?;
            route.apply(self)?;
            self.emit(ICrosschainGov::ApprovedActionApplied {
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
        CROSSCHAIN_GOV_ADDRESS
    }

    fn validate(&self, payload: &[u8], context: VoteTargetContext) -> Result<()> {
        decode_proposal_payload(payload)?;
        Route::decode(payload, context.chain_id).map(|_| ())
    }

    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        payload: &[u8],
        context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        // Stored-payload corruption is an invariant failure, never proposal Error.
        decode_stored_proposal_payload(payload, MODULE)?;
        Route::decode(payload, context.chain_id)
            .map_err(|_| VoteError::stored_payload_invalid(MODULE))?;
        let gov = CrosschainGov::new(ctx.storage.clone());
        gov.approved_payload
            .write(&proposal_id, keccak256(payload))?;
        gov.approved_at
            .write(&proposal_id, ctx.block.block_number)?;
        Ok(TargetExecutionOutcome::Applied)
    }
}
