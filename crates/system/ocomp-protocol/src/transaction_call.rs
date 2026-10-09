//! The call fields of a signed transaction envelope that do not depend on
//! signer recovery.

use core::fmt;

use alloy_consensus::Transaction;
use alloy_primitives::{Address, U256};

/// The call fields of one signed transaction envelope. A view that needs the
/// envelope type or the recovered signer keeps that field beside these fields.
#[derive(Debug, Clone, Copy)]
pub struct TransactionCallFields<'a> {
    /// Call target. Contract creation transactions are represented as `None`.
    pub to: Option<Address>,
    /// Native value attached to the transaction.
    pub value: U256,
    /// ABI calldata bytes.
    pub input: &'a [u8],
    /// Transaction gas limit.
    pub gas_limit: u64,
    /// EIP-1559 max fee per gas.
    pub max_fee_per_gas: u128,
    /// EIP-1559 priority fee per gas, if present.
    pub max_priority_fee_per_gas: Option<u128>,
}

impl<'a> TransactionCallFields<'a> {
    /// Borrow the call fields of `tx`. The getters run in field order.
    pub fn from_transaction<T: Transaction + ?Sized>(tx: &'a T) -> Self {
        Self {
            to: tx.to(),
            value: tx.value(),
            input: tx.input().as_ref(),
            gas_limit: tx.gas_limit(),
            max_fee_per_gas: tx.max_fee_per_gas(),
            max_priority_fee_per_gas: tx.max_priority_fee_per_gas(),
        }
    }

    /// Add the six call fields to `out` in field order, with the same names
    /// and values as a derived `Debug` of a view that holds them directly.
    pub fn debug_fields(&self, out: &mut fmt::DebugStruct<'_, '_>) {
        out.field("to", &self.to)
            .field("value", &self.value)
            .field("input", &self.input)
            .field("gas_limit", &self.gas_limit)
            .field("max_fee_per_gas", &self.max_fee_per_gas)
            .field("max_priority_fee_per_gas", &self.max_priority_fee_per_gas);
    }
}
