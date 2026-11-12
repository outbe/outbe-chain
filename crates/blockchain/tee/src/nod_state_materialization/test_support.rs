//! Scoped request exchanges for real cryptographic engine fixtures.

use super::*;
use std::{cell::Cell, marker::PhantomData, rc::Rc};

pub type Exchange = fn(EnclaveRequest) -> Result<([u8; 32], EnclaveResponse), TransportError>;
thread_local! { static EXCHANGE: Cell<Option<Exchange>> = const { Cell::new(None) }; }

pub struct Guard {
    previous: Option<Exchange>,
    _thread: PhantomData<Rc<()>>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        EXCHANGE.set(self.previous);
    }
}
#[derive(Clone, Copy)]
pub struct Context {
    exchange: Option<Exchange>,
}
impl Context {
    pub fn install(self) -> Guard {
        Guard {
            previous: EXCHANGE.replace(self.exchange),
            _thread: PhantomData,
        }
    }
}
pub fn scope(exchange: Exchange) -> Guard {
    Context {
        exchange: Some(exchange),
    }
    .install()
}
pub fn capture() -> Context {
    Context {
        exchange: EXCHANGE.get(),
    }
}
pub(super) fn current() -> Option<Exchange> {
    EXCHANGE.get()
}
