//! Typed errors for `outbe-consensus`.

pub mod core;
pub mod dkg;
pub mod follow;

pub use self::{core::*, dkg::*, follow::*};
