//! Owner calls that the Hyperlane logic needs.
//!
//! This crate makes no call to another contract by itself. A function that
//! needs an owner call (an ISM write, an ICA router enrollment) returns it as
//! [`Calls`]. `crosschaingov` runs them from the owner address.

use alloy_primitives::{Address, Bytes, U256};

/// One owner call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCall {
    pub to: Address,
    pub value: U256,
    pub data: Bytes,
}

impl RemoteCall {
    /// A zero-value call of `to`.
    pub fn new(to: Address, data: impl Into<Bytes>) -> Self {
        Self {
            to,
            value: U256::ZERO,
            data: data.into(),
        }
    }
}

/// The owner calls of one operation: on Outbe first, then one message for
/// each remote domain.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Calls {
    pub local: Vec<RemoteCall>,
    pub remote: Vec<(u32, Vec<RemoteCall>)>,
}

impl Calls {
    /// All `calls` run on `chain_id`. `outbe` is the chain id of this chain.
    pub fn on(chain_id: u32, outbe: u32, calls: Vec<RemoteCall>) -> Self {
        if chain_id == outbe {
            Self {
                local: calls,
                remote: Vec::new(),
            }
        } else {
            Self {
                local: Vec::new(),
                remote: vec![(chain_id, calls)],
            }
        }
    }

    /// Appends the calls of `other` after the calls of `self`.
    pub fn extend(&mut self, other: Self) {
        self.local.extend(other.local);
        self.remote.extend(other.remote);
    }
}
