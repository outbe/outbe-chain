//! Founding-network build and multi-host lifecycle orchestration.

pub mod build;
mod build_genesis;
mod build_joining;
mod build_vm;
pub mod config;
mod feeder_config;
pub mod prepare;
pub(crate) mod remote;
pub mod start;
pub mod status;
pub mod vm;
pub mod vm_state;
