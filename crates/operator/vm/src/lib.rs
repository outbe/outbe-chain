//! Outbe VM disk creation, dstack VMM client, guest image staging, local VM lifecycle,
//! and host network helpers.

pub mod block_splice;
mod container;
pub mod disk;
pub mod dnsmasq;
pub mod host;
pub mod image;
pub mod local;
pub mod nat;
pub mod vmm;

/// Single-quote a string for POSIX shells.
pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
