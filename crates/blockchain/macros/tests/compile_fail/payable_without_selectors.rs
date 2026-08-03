//! A `#[contract_payable]` method requires the module to publish
//! `PAYABLE_SELECTORS`, which the route table binds to the address's value
//! policy. Omitting it must not compile.
#![allow(unused_imports)]

use alloy_primitives::{Address, U256};
use outbe_macros::{contract_dispatch, contract_payable, contract_public};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

#[allow(dead_code)]
struct Vault<'storage>(StorageHandle<'storage>);

impl<'storage> Vault<'storage> {
    fn new(storage: StorageHandle<'storage>) -> Self {
        Self(storage)
    }
}

#[contract_dispatch]
impl Vault<'_> {
    #[contract_public("fund(uint256)")]
    #[contract_payable]
    fn _abi_fund(&mut self, _caller: Address, _value: U256, _amount: U256) -> Result<()> {
        Ok(())
    }
}

fn main() {}
