//! `#[contract_view]` and `#[contract_payable]` on the same method must fail.

#![allow(unused_imports)]

use outbe_macros::{contract_dispatch, contract_payable, contract_public, contract_view};

#[allow(dead_code)]
struct Fake<'storage>(std::marker::PhantomData<&'storage ()>);

#[contract_dispatch]
impl Fake<'_> {
    #[contract_public("foo()")]
    #[contract_view]
    #[contract_payable]
    fn _abi_foo(&mut self) -> Result<(), ()> {
        Ok(())
    }
}

fn main() {}
