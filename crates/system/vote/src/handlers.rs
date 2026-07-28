//! Compile-time registry of vote target-module handlers.

use alloy_primitives::{Address, U256};
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;

use crate::errors::VoteError;
use crate::schema::{ProposalRecord, ProposalStatus};

/// Static handler table entry type.
pub type VoteTargetHandlers = &'static [&'static dyn VoteTarget];

/// Compile-time proposal admission class owned by a target module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetAdmission {
    ActiveValidatorOnly,
    PublicBonded { amount: U256 },
}

/// Consensus context passed explicitly to target validation and execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoteTargetContext {
    pub proposer: Address,
    pub attached_value: U256,
    pub block_number: u64,
    pub chain_id: u64,
}

/// Deterministic result of applying an approved proposal to its target module.
///
/// Infrastructure/provider failures remain the outer [`Result::Err`] and abort
/// execution. Only a target-declared proposal execution failure uses [`Self::Error`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetExecutionOutcome {
    Applied,
    Error { reason: String },
}

/// Target-module handler for approved vote proposals.
pub trait VoteTarget: Send + Sync {
    /// Precompile address this handler serves.
    fn target_module(&self) -> Address;

    /// Compile-time admission class. V1 targets default to validator-only, zero value.
    fn admission(&self) -> TargetAdmission {
        TargetAdmission::ActiveValidatorOnly
    }

    /// Fail-fast validation used during proposal creation.
    fn validate(&self, payload: &[u8], context: VoteTargetContext) -> Result<()>;

    /// Applies side effects when a proposal is approved.
    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        payload: &[u8],
        context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome>;

    /// Dispatches terminal proposal outcomes to the target module.
    /// Only result of tally is possible (Expired or Approved).
    fn handle_tally(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        payload: &[u8],
        context: VoteTargetContext,
        status: ProposalStatus,
    ) -> Result<TargetExecutionOutcome> {
        match status {
            ProposalStatus::Approved => self.handle_approved(ctx, proposal_id, payload, context),
            ProposalStatus::Rejected
            | ProposalStatus::Expired
            | ProposalStatus::Pending
            | ProposalStatus::Error => Ok(TargetExecutionOutcome::Applied),
        }
    }
}

/// Read-only view over a compile-time handler table.
#[derive(Clone, Copy)]
pub struct VoteTargetRegistry {
    handlers: VoteTargetHandlers,
}

impl VoteTargetRegistry {
    /// Builds a registry from a static handler table.
    pub const fn new(handlers: VoteTargetHandlers) -> Self {
        Self { handlers }
    }

    /// Returns the handler registered for `target_module`, if any.
    ///
    /// Returns an error when more than one handler is registered for the same address.
    pub fn lookup(&self, target_module: Address) -> Result<&'static dyn VoteTarget> {
        let mut matches = self
            .handlers
            .iter()
            .filter(|handler| handler.target_module() == target_module);
        let Some(first) = matches.next() else {
            return Err(VoteError::UnknownTargetModule.into());
        };
        if matches.next().is_some() {
            return Err(VoteError::DuplicateTargetModule.into());
        }
        Ok(*first)
    }
}

/// Validates a target payload during proposal creation.
pub fn validate_target_payload(
    registry: &VoteTargetRegistry,
    target_module: Address,
    payload: &[u8],
    context: VoteTargetContext,
) -> Result<()> {
    let target = registry.lookup(target_module)?;
    target.validate(payload, context)
}

/// Dispatches a terminal proposal outcome to its target module.
pub fn handle_target_tally(
    registry: &VoteTargetRegistry,
    ctx: &BlockRuntimeContext,
    proposal_id: U256,
    proposal: &ProposalRecord,
    status: ProposalStatus,
) -> Result<TargetExecutionOutcome> {
    let target = registry.lookup(proposal.target_module)?;
    let context = VoteTargetContext {
        proposer: proposal.proposer,
        attached_value: U256::ZERO,
        block_number: ctx.block.block_number,
        chain_id: ctx.storage.chain_id()?,
    };
    target.handle_tally(
        ctx,
        proposal_id,
        proposal.payload.as_bytes(),
        context,
        status,
    )
}
