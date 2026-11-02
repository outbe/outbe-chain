//! The owner calls of one vote action. An action builds them, and the
//! executor runs them.

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
}
