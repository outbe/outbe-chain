//! Strict JSON governance actions on the multi-chain contracts that
//! `0x...EE14` owns.
//!
//! Place of a new action:
//!
//! | The action changes | Place |
//! |---|---|
//! | any of our UUPS contracts in the same way | [`common`] |
//! | the functions of one contract family | `actions/<family>.rs` |
//! | the Hyperlane infrastructure | `outbe_hyperlanecontroller::actions` |
//!
//! A chain id is also the Hyperlane domain of that chain. Calls on the Outbe
//! chain id go through `call_local`. Calls on another chain id go in one
//! `call_remote`.

pub mod common;
pub mod crosschain;
pub mod tokens;

use outbe_hyperlanecontroller::{Owner, RemoteCall};
use outbe_primitives::error::Result;
use outbe_vote::errors::VoteError;
use serde::Deserialize;

/// One action of this module. The `operation` field selects the variant.
#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "camelCase")]
pub enum Action {
    UpgradeContract(common::UpgradeContract),
    SetBridgePaused(crosschain::SetBridgePaused),
    SetBridgeGateway(crosschain::SetBridgeGateway),
    LinkBridges(crosschain::LinkBridges),
    LinkTokenBridges(tokens::LinkTokenBridges),
}

/// The checks and the owner calls of one operation.
pub trait Operation {
    /// Returns `true` when the fields are valid. `outbe` is the Outbe chain id.
    fn is_valid(&self, outbe: u64) -> bool;

    /// The owner calls that the operation makes. `outbe` is the Outbe domain.
    fn calls(&self, outbe: u32) -> Calls;
}

/// The owner calls of one action: on Outbe first, then one remote message.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Calls {
    pub local: Vec<RemoteCall>,
    pub remote: Option<(u32, Vec<RemoteCall>)>,
}

impl Calls {
    /// All `calls` run on `chain_id`.
    pub fn on(chain_id: u32, outbe: u32, calls: Vec<RemoteCall>) -> Self {
        if chain_id == outbe {
            Self {
                local: calls,
                remote: None,
            }
        } else {
            Self {
                local: Vec::new(),
                remote: Some((chain_id, calls)),
            }
        }
    }
}

/// Returns `true` for a non-zero chain id other than `outbe`.
pub(crate) fn is_remote(chain_id: u32, outbe: u64) -> bool {
    chain_id != 0 && u64::from(chain_id) != outbe
}

impl Action {
    /// Decodes and checks a payload. `outbe` is the Outbe chain id.
    pub fn decode(payload: &[u8], outbe: u64) -> Result<Self> {
        // Decode the original bytes so duplicate fields are rejected.
        let action: Self =
            serde_json::from_slice(payload).map_err(|_| VoteError::InvalidPayload)?;
        if !action.operation().is_valid(outbe) {
            return Err(VoteError::InvalidPayload.into());
        }
        Ok(action)
    }

    fn operation(&self) -> &dyn Operation {
        match self {
            Self::UpgradeContract(op) => op,
            Self::SetBridgePaused(op) => op,
            Self::SetBridgeGateway(op) => op,
            Self::LinkBridges(op) => op,
            Self::LinkTokenBridges(op) => op,
        }
    }

    /// The owner calls of the action. `outbe` is the Outbe domain.
    pub fn calls(&self, outbe: u32) -> Calls {
        self.operation().calls(outbe)
    }

    /// Runs the owner calls: the Outbe calls first, then the remote message.
    pub fn apply(&self, outbe: u32, owner: &mut impl Owner) -> Result<()> {
        let calls = self.calls(outbe);
        for call in calls.local {
            owner.call_local(call.to, call.value, call.data)?;
        }
        if let Some((chain_id, remote)) = calls.remote {
            owner.call_remote(chain_id, &remote)?;
        }
        Ok(())
    }
}
