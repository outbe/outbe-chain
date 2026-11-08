//! Vote target handler for governed StablecoinFactory proposals.

use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::{STABLECOIN_ADDRESS_PREFIX, STABLECOIN_FACTORY_ADDRESS};
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::stablecoin::{decode_canonical_stablecoin_create, predict_stablecoin};
use outbe_primitives::stablecoin_fork::STABLECOIN_CREATE_BOND;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::vote_target::{
    ProposalStatus, TargetAdmission, TargetExecutionOutcome, VoteTarget, VoteTargetContext,
};

use crate::{FactoryReservation, StablecoinFactoryApi, ValidatedStablecoinCreate};

/// Errors that occur when validating a `StablecoinFactory` vote proposal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StablecoinVoteTargetError {
    #[error("non-canonical stablecoin payload")]
    NonCanonicalPayload,
    #[error("stablecoin proposer must equal issuer")]
    ProposerMustEqualIssuer,
    #[error("invalid stablecoin identity")]
    InvalidIdentity,
}

impl From<StablecoinVoteTargetError> for PrecompileError {
    fn from(error: StablecoinVoteTargetError) -> Self {
        Self::Revert(error.to_string())
    }
}

/// Checks whether a precompile error is a user revert.
const fn is_revert_error(error: &PrecompileError) -> bool {
    matches!(
        error,
        PrecompileError::Revert(_) | PrecompileError::RevertBytes(_)
    )
}

/// Vote target handler for `STABLECOIN_FACTORY_ADDRESS`.
pub struct StablecoinFactoryVoteTarget;

impl StablecoinFactoryVoteTarget {
    fn validate_payload(
        payload: &[u8],
        context: VoteTargetContext,
    ) -> Result<ValidatedStablecoinCreate> {
        let decoded = decode_canonical_stablecoin_create(payload)
            .map_err(|_| StablecoinVoteTargetError::NonCanonicalPayload)?;
        if decoded.issuer != context.proposer {
            return Err(StablecoinVoteTargetError::ProposerMustEqualIssuer.into());
        }
        let (token_id, token) = predict_stablecoin(
            context.chain_id,
            STABLECOIN_FACTORY_ADDRESS,
            decoded.issuer,
            &decoded.ticker,
            STABLECOIN_ADDRESS_PREFIX,
        )
        .map_err(|_| StablecoinVoteTargetError::InvalidIdentity)?;
        Ok(ValidatedStablecoinCreate {
            payload: decoded,
            token_id,
            token,
        })
    }
}

impl VoteTarget for StablecoinFactoryVoteTarget {
    fn target_module(&self) -> Address {
        STABLECOIN_FACTORY_ADDRESS
    }

    fn admission(&self) -> TargetAdmission {
        TargetAdmission::PublicBonded {
            amount: STABLECOIN_CREATE_BOND,
        }
    }

    fn validate(&self, payload: &[u8], context: VoteTargetContext) -> Result<()> {
        Self::validate_payload(payload, context).map(|_| ())
    }

    fn reserve(
        &self,
        storage: StorageHandle<'_>,
        proposal_id: U256,
        payload: &[u8],
        context: VoteTargetContext,
    ) -> Result<()> {
        let validated =
            StablecoinFactoryApi::validate_create(storage.clone(), context.proposer, payload)?;
        StablecoinFactoryApi::reserve(
            storage,
            &FactoryReservation {
                proposal_id,
                token_id: validated.token_id,
                ticker: validated.payload.ticker,
                token: validated.token,
            },
        )
    }

    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        payload: &[u8],
        context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        match StablecoinFactoryApi::execute_approved(
            ctx.storage.clone(),
            proposal_id,
            context.proposer,
            payload,
            0,
        ) {
            Ok(_) => Ok(TargetExecutionOutcome::Applied),
            Err(error) if is_revert_error(&error) => Ok(TargetExecutionOutcome::Error {
                reason: error.to_string(),
            }),
            Err(error) => Err(error),
        }
    }

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
            ProposalStatus::Expired => {
                StablecoinFactoryApi::release(ctx.storage.clone(), proposal_id)?;
                Ok(TargetExecutionOutcome::Applied)
            }
            ProposalStatus::Pending | ProposalStatus::Error => Ok(TargetExecutionOutcome::Applied),
        }
    }
}
