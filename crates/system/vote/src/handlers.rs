//! Compile-time registry of vote target-module handlers.

use alloy_primitives::{Address, U256};
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::errors::VoteError;
use crate::schema::{ProposalRecord, ProposalStatus};

/// Timing policy selected by the target for newly admitted proposals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProposalLifecycle {
    Standard,
    ThreeWindow {
        voting_window: u64,
        replace_window: u64,
        activation_window: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProposalAction {
    Primary,
    Cancel(U256),
    Replace(U256),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VotePlanContext {
    pub replace_end: u64,
    pub activation_height: u64,
}

/// Static handler table entry type.
pub type VoteTargetHandlers = &'static [&'static dyn VoteTarget];

/// Compile-time proposal admission class owned by a target module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetAdmission {
    ActiveValidatorOnly,
    PublicBonded { amount: U256 },
}

/// Electorate policy selected by the target and persisted at proposal admission.
/// The policy is fixed, but membership is read live for casting and finalization;
/// this value is not a creation-time membership snapshot.
/// Zero preserves the ACTIVE-only policy of existing proposals and targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TargetElectorate {
    Active = 0,
    ActiveConsensus = 1,
}

impl TargetElectorate {
    pub(crate) fn from_stored(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Active),
            1 => Ok(Self::ActiveConsensus),
            _ => Err(VoteError::InvalidElectorate.into()),
        }
    }
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

/// Decodes a JSON proposal payload for admission.
/// Reverts malformed JSON with [`VoteError::InvalidPayload`].
pub fn decode_proposal_payload(payload: &[u8]) -> Result<serde_json::Value> {
    serde_json::from_slice(payload).map_err(|_| VoteError::InvalidPayload.into())
}

/// Decodes the stored JSON payload of an approved proposal for `module`.
/// Returns a fatal error if the stored payload is malformed JSON.
pub fn decode_stored_proposal_payload(payload: &[u8], module: &str) -> Result<serde_json::Value> {
    serde_json::from_slice(payload).map_err(|_| VoteError::stored_payload_invalid(module))
}

/// Target-module handler for approved vote proposals.
pub trait VoteTarget: Send + Sync {
    /// Precompile address this handler serves.
    fn target_module(&self) -> Address;

    /// Compile-time admission class. V1 targets default to validator-only, zero value.
    fn admission(&self) -> TargetAdmission {
        TargetAdmission::ActiveValidatorOnly
    }

    /// Controls both cast eligibility and the exact tally denominator.
    fn electorate(&self) -> TargetElectorate {
        TargetElectorate::Active
    }

    fn lifecycle(&self) -> ProposalLifecycle {
        ProposalLifecycle::Standard
    }

    /// Validate admission and classify the parent-bound action in one pass.
    fn proposal_action(
        &self,
        payload: &[u8],
        context: VoteTargetContext,
    ) -> Result<ProposalAction> {
        self.validate(payload, context)?;
        Ok(ProposalAction::Primary)
    }

    fn register_plan(
        &self,
        _ctx: &BlockRuntimeContext,
        _id: U256,
        _payload: &[u8],
        _context: VoteTargetContext,
        _plan: VotePlanContext,
    ) -> Result<TargetExecutionOutcome> {
        Err(outbe_primitives::error::PrecompileError::Unsupported)
    }

    fn cancel_plan(
        &self,
        _ctx: &BlockRuntimeContext,
        _parent: U256,
        _by_proposal: U256,
    ) -> Result<TargetExecutionOutcome> {
        Err(outbe_primitives::error::PrecompileError::Unsupported)
    }

    fn prepare_plan(
        &self,
        _ctx: &BlockRuntimeContext,
        _id: U256,
        _activation_height: u64,
    ) -> Result<()> {
        Err(outbe_primitives::error::PrecompileError::Unsupported)
    }

    fn existing_plan(
        &self,
        _storage: StorageHandle<'_>,
        _proposal: &ProposalRecord,
    ) -> Result<Option<VotePlanContext>> {
        Ok(None)
    }

    /// Fail-fast validation used during proposal creation.
    fn validate(&self, payload: &[u8], context: VoteTargetContext) -> Result<()>;

    /// Atomically reserves target-owned admission state for the allocated proposal id.
    fn reserve(
        &self,
        _storage: StorageHandle<'_>,
        _proposal_id: U256,
        _payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<()> {
        Ok(())
    }

    /// Applies side effects when a proposal is approved.
    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        payload: &[u8],
        context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome>;

    /// Dispatches terminal proposal outcomes to the target module.
    /// Only a tally result is possible (Expired or Approved).
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
            ProposalStatus::Expired
            | ProposalStatus::Pending
            | ProposalStatus::Error
            | ProposalStatus::Canceled => Ok(TargetExecutionOutcome::Applied),
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

    /// Dispatches a terminal proposal outcome to the target module of
    /// `proposal`. The target context carries `attached_value`, the current
    /// block number and the chain id.
    pub fn handle_tally(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        proposal: &ProposalRecord,
        attached_value: U256,
        status: ProposalStatus,
    ) -> Result<TargetExecutionOutcome> {
        let target = self.lookup(proposal.target_module)?;
        let context = VoteTargetContext {
            proposer: proposal.proposer,
            attached_value,
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
}
