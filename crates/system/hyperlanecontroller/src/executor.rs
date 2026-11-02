//! Owner calls of the controller address.
//!
//! The controller owns no contract by itself. Every write to an ISM or to the
//! ICA router goes through [`Executor`], which the module that serves the address
//! implements. All calls run in the frame of that address, so it is the
//! `msg.sender` of each call.

use alloy_primitives::{Address, Bytes, B256, U256};
use outbe_primitives::error::Result;

/// One call that the owner makes on a remote chain through its Interchain Account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCall {
    pub to: Address,
    pub value: U256,
    pub data: Bytes,
}

/// Owner calls of the controller address.
pub trait Executor {
    /// Calls `to` on this chain. The owner balance pays `value`.
    fn call_local(&mut self, to: Address, value: U256, data: Bytes) -> Result<Bytes>;

    /// Runs `calls` on `domain` through the Interchain Account. The owner
    /// balance pays the dispatch fee.
    fn call_remote(&mut self, domain: u32, calls: &[RemoteCall]) -> Result<B256>;
}
