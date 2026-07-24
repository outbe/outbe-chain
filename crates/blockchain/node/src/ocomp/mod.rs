//! Node-local OCOMP retention and finalized-input authority.
//!
//! Nothing in this module changes block validity. It decides only whether this
//! validator has enough durable, authenticated input to advertise, vote for,
//! export, execute, or sign one PoC job.

pub mod control;
pub mod finality;
mod openings;
pub mod retention;

#[cfg(test)]
mod tests;
