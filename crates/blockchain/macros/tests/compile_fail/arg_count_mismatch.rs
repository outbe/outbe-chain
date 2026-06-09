//! Signature declares 2 ABI args (`uint32, uint64`) but the view method has 1.

#![allow(unused_imports)]

use outbe_macros::{contract_dispatch, contract_public, contract_view};

#[allow(dead_code)]
struct Fake<'storage>(std::marker::PhantomData<&'storage ()>);

#[contract_dispatch]
impl Fake<'_> {
    #[contract_public("foo(uint32,uint64)")]
    #[contract_view]
    fn _abi_foo(&mut self, only_one: u32) -> Result<(), ()> {
        let _ = only_one;
        Ok(())
    }
}

fn main() {}
