//! Vote actions on the Hyperlane infrastructure: the chains that the
//! validator sync reaches.
//!
//! The crosschain vote target in `crosschaingov` sends these operations here.
//! A chain id is also the Hyperlane domain of that chain.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;
use outbe_vote::errors::VoteError;
use serde::Deserialize;

use crate::errors::HyperlaneControllerError;
use crate::executor::Executor;
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::IHyperlaneRouter;

/// One Hyperlane operation, as strict JSON with an `operation` field.
#[derive(Debug, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Action {
    /// Puts `chain_id` under the controller: the Outbe ICA router enrolls
    /// `ica_router` (the ICA router on that chain), then the controller
    /// accepts the ownership of the ISM there and starts to sync the Outbe
    /// validators to it.
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
    pub fn decode(payload: &[u8], outbe: u64) -> Result<Self> {
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

    /// Runs the action. Every owner call goes through `owner`.
    pub fn apply(
        &self,
        controller: &mut HyperlaneControllerContract<'_>,
        executor: &mut impl Executor,
    ) -> Result<()> {
        match self {
            Self::AddHyperlaneChain {
                chain_id,
                ica_router,
                ism,
                hook,
            } => {
                controller.enroll_ica_router(executor, *chain_id, *ica_router)?;
                controller.add_domain(executor, *chain_id, *ism, *hook)
            }
            Self::RemoveHyperlaneChain { chain_id } => controller.remove_domain(*chain_id),
        }
    }
}

impl HyperlaneControllerContract<'_> {
    /// Enrolls the ICA router of `domain` on the Outbe ICA router, so that
    /// `call_remote` can reach that chain. The router keeps the first
    /// enrollment forever, so the same router is a no-op and another router
    /// reverts.
    fn enroll_ica_router(
        &mut self,
        executor: &mut impl Executor,
        domain: u32,
        router: Address,
    ) -> Result<()> {
        let ica_router = self.ica_router.read()?;
        let ret = self.storage.staticcall(
            ica_router,
            IHyperlaneRouter::routersCall { _domain: domain }
                .abi_encode()
                .into(),
        )?;
        let enrolled = IHyperlaneRouter::routersCall::abi_decode_returns(&ret).map_err(|_| {
            HyperlaneControllerError::UndecodableReturn("InterchainAccountRouter routers")
        })?;
        if enrolled == router.into_word() {
            return Ok(());
        }
        if enrolled != B256::ZERO {
            return Err(HyperlaneControllerError::RemoteRouterMismatch { domain, enrolled }.into());
        }
        let data = IHyperlaneRouter::enrollRemoteRouterCall {
            _domain: domain,
            _router: router.into_word(),
        }
        .abi_encode();
        executor
            .call_local(ica_router, U256::ZERO, data.into())
            .map(|_| ())
    }
}
