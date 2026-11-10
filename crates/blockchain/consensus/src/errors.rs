//! Typed errors for `outbe-consensus`.

pub mod dkg;

pub use self::dkg::*;
pub use outbe_simplex::errors::{core, follow, *};
