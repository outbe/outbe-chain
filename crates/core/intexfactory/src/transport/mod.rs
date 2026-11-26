//! The Intex messages Outbe sends to the target chains: their bytes, their gas and the outbox that
//! sends them through CrosschainBridge. Desis and IntexFactory both send through it.

pub mod gas;
pub mod outbox;
pub mod wire;

#[cfg(test)]
mod tests;
