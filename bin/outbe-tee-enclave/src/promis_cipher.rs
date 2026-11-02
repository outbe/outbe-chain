//! Rollback-safe mutable Promis balance envelopes (`PRO2`), readable with view keys.
use crate::{
    confidential::{BalanceTransition, EnvelopeCipherSpec},
    errors::Result,
};
use alloy_primitives::{Address, U256};

pub const PROMIS_SPEC: EnvelopeCipherSpec = EnvelopeCipherSpec {
    marker: b"PRO2",
    binding_tag: b"outbe/promis/transition-binding/v2",
    key_info: b"outbe/promis/amount-key/v2",
    nonce_info: b"outbe/promis/amount-nonce/v2",
};

pub fn read_amount(
    key: &[u8; 32],
    account: Address,
    field: u8,
    blob: &[u8],
) -> Result<(u64, U256)> {
    PROMIS_SPEC.read_amount(key, account, field, blob)
}

pub fn write_amount(key: &[u8; 32], transition: BalanceTransition) -> Result<Vec<u8>> {
    PROMIS_SPEC.write_amount(key, transition)
}
