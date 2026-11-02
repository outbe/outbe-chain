//! The owner calls that this module needs from the owner of the Hyperlane
//! contracts.
//!
//! The module has no address of its own. It runs in the frame of the
//! CrosschainGov precompile (`0x…EE14`), which owns the ISMs and the ICA
//! router. Every state-changing call to an ISM or to the ICA router goes
//! through [`Owner`], so `msg.sender` of that call is the precompile.

use alloy_primitives::{Address, Bytes, B256, U256};
use outbe_primitives::error::Result;

/// One call executed by the owner's Interchain Account on a remote chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCall {
    pub to: Address,
    pub value: U256,
    pub data: Bytes,
}

impl RemoteCall {
    /// A call of `data` on `to` with zero value.
    pub fn new(to: Address, data: impl Into<Bytes>) -> Self {
        Self {
            to,
            value: U256::ZERO,
            data: data.into(),
        }
    }
}

/// The owner calls of the precompile at `0x…EE14`.
pub trait Owner {
    /// Calls `to` on Outbe with `value` and `data`. Returns the return data.
    fn call_local(&mut self, to: Address, value: U256, data: Bytes) -> Result<Bytes>;

    /// Sends `calls` in one Interchain Account message to `domain`. Returns
    /// the Hyperlane message id.
    fn call_remote(&mut self, domain: u32, calls: &[RemoteCall]) -> Result<B256>;
}
