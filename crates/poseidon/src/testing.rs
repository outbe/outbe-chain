//! Scoped, thread-local failure injection for integration tests only.
use crate::PoseidonError;
use std::{cell::Cell, marker::PhantomData, rc::Rc};

thread_local! { static REMAINING: Cell<Option<usize>> = const { Cell::new(None) }; }

/// Restores the previous injection scope on drop; cannot move across threads.
pub struct FailureGuard {
    previous: Option<usize>,
    _thread: PhantomData<Rc<()>>,
}

/// Fail after this many successful permutations on the current thread.
pub fn fail_after(successful_permutations: usize) -> FailureGuard {
    let previous = REMAINING.with(|count| count.replace(Some(successful_permutations)));
    FailureGuard {
        previous,
        _thread: PhantomData,
    }
}

impl Drop for FailureGuard {
    fn drop(&mut self) {
        REMAINING.with(|count| count.set(self.previous));
    }
}

pub(crate) fn before_permutation() -> Result<(), PoseidonError> {
    REMAINING.with(|count| match count.get() {
        Some(0) => Err(outbe_barretenberg::RuntimeError::InjectedFailure.into()),
        Some(n) => {
            count.set(Some(n - 1));
            Ok(())
        }
        None => Ok(()),
    })
}
