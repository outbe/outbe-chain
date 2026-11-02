//! Governance actions on the Hyperlane infrastructure: the domains whose ISM
//! gets the Outbe validator set.
//!
//! The CrosschainGov vote target sends these operations here. It checks the
//! payload with [`HyperlaneAction::decode`] when the proposal is created, and
//! runs it with [`HyperlaneAction::apply`] on `applyApproved`.
//!
//! A chain id is also the Hyperlane domain of that chain.

use alloy_primitives::Address;
use outbe_primitives::error::Result;
use outbe_vote::errors::VoteError;
use serde::Deserialize;

use crate::schema::HyperlaneControllerContract;
use crate::Owner;

/// Strict JSON action on the Hyperlane infrastructure.
#[derive(Debug, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum HyperlaneAction {
    /// Puts `chain_id` under the validator sync: the Outbe ICA router enrolls
    /// `ica_router` (the ICA router on that chain), then the owner accepts
    /// the ownership of the ISM there and the sync starts to send the Outbe
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

impl HyperlaneAction {
    /// The `operation` values of this module.
    pub const OPERATIONS: [&'static str; 2] = ["addHyperlaneChain", "removeHyperlaneChain"];

    /// Decodes and checks a payload. `outbe` is the Outbe chain id.
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

    /// Runs the action. Every call to the ICA router or to an ISM goes
    /// through `owner`.
    pub fn apply(
        &self,
        controller: &mut HyperlaneControllerContract<'_>,
        owner: &mut impl Owner,
    ) -> Result<()> {
        match self {
            Self::AddHyperlaneChain {
                chain_id,
                ica_router,
                ism,
                hook,
            } => {
                controller.enroll_ica_router(*chain_id, *ica_router, owner)?;
                controller.add_domain(*chain_id, (*ism, *hook), owner)
            }
            Self::RemoveHyperlaneChain { chain_id } => controller.remove_domain(*chain_id),
        }
    }
}
