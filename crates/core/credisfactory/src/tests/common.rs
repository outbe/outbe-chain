//! Shared test fixtures for the credisfactory crate.

use alloy_primitives::{address, Address};

pub const CHAIN_ID: u64 = 1;
pub const CREATED_AT: u64 = 1_700_000_000;
pub const BLOCK_NUMBER: u64 = 42;

pub fn alice() -> Address {
    address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
}

pub fn bob() -> Address {
    address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB")
}

pub fn asset() -> Address {
    address!("0x0000000000000000000000000000000000000888")
}
