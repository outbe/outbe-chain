//! Default-mutating (no `#[contract_view]`) method must take `caller: Address`
//! as the first parameter after `&mut self`. Using `u32` instead must fail.

#![allow(unused_imports)]

use outbe_macros::{contract_dispatch, contract_public};

#[allow(dead_code)]
struct Fake<'storage>(std::marker::PhantomData<&'storage ()>);

#[contract_dispatch]
impl Fake<'_> {
    #[contract_public("foo(uint32)")]
    fn _abi_foo(&mut self, sender: u32) -> Result<(), ()> {
        let _ = sender;
        Ok(())
    }
}

fn main() {}
