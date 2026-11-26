//! Host-side NAT masquerade management for outbound VM internet access.
//!
//! Owns a single nftables table (`ip outbe_up_nat`), a ruleset file
//! (`/etc/outbe-up/nat.nft`), a oneshot systemd unit (`outbe-up-nat.service`),
//! and optionally `/etc/sysctl.d/90-outbe-up-nat.conf` when no persistent
//! sysctl configuration already enables `net.ipv4.ip_forward = 1`.
//! Every managed file carries the `# managed by outbe-up vm host-nat` marker;
//! foreign files and foreign nftables tables (such as `inet outbe_host_fw`) are
//! never modified or deleted.

mod apply;
mod commands;
mod egress;
mod nft;
mod preflight;
mod sysctl;
#[cfg(test)]
mod tests;

use std::{
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

use eyre::{ensure, Result};
use serde::Serialize;

pub use apply::{run_apply_on_host, run_remove_on_host, ApplyOptions};
pub use commands::validate_iface_name;
pub(crate) use commands::{
    build_apply_command, build_remove_command, render_nat_ruleset, render_systemd_unit,
};
pub(crate) use nft::inspect_nft_json;
pub use nft::{
    ExistingEgress, ForeignForwardChain, ForwardChainClass, NftInspection, PartialEgress,
};
pub(crate) use sysctl::parse_persistent_sysctl;
pub use sysctl::PersistentSysctlState;

pub const NAT_TABLE_FAMILY: &str = "ip";
pub const NAT_TABLE_NAME: &str = "outbe_up_nat";
const NAT_RULES_PATH: &str = "/etc/outbe-up/nat.nft";
const NAT_SERVICE_NAME: &str = "outbe-up-nat.service";
const NAT_SERVICE_PATH: &str = "/etc/systemd/system/outbe-up-nat.service";
const NAT_SYSCTL_PATH: &str = "/etc/sysctl.d/90-outbe-up-nat.conf";
pub const OWNERSHIP_MARKER: &str = "# managed by outbe-up vm host-nat";

const SENTINEL_BRIDGE_ADDR: &str = "===OUTBE_NAT_BRIDGE_ADDR===";
const SENTINEL_UPLINK_ROUTE: &str = "===OUTBE_NAT_UPLINK_ROUTE===";
const SENTINEL_IP_FORWARD: &str = "===OUTBE_NAT_IP_FORWARD===";
const SENTINEL_PERSISTENT_SYSCTL: &str = "===OUTBE_NAT_PERSISTENT_SYSCTL===";
const SENTINEL_OWNED_FILES: &str = "===OUTBE_NAT_OWNED_FILES===";
const SENTINEL_NFT_JSON: &str = "===OUTBE_NAT_NFT_JSON===";

/// Configurable file paths on the target host (overridden in unit/integration tests).
#[derive(Clone, Debug)]
pub struct HostNatPaths {
    pub rules_path: PathBuf,
    pub service_path: PathBuf,
    pub sysctl_path: PathBuf,
    pub sysctl_scan_dirs: Vec<PathBuf>,
    pub sysctl_conf_path: PathBuf,
}

impl HostNatPaths {
    /// Resolve standard host NAT paths relative to a filesystem root (default `"/"`).
    pub fn under_root(root: &Path) -> Self {
        if root == Path::new("/") {
            return Self::default();
        }
        Self {
            rules_path: root.join(NAT_RULES_PATH.trim_start_matches('/')),
            service_path: root.join(NAT_SERVICE_PATH.trim_start_matches('/')),
            sysctl_path: root.join(NAT_SYSCTL_PATH.trim_start_matches('/')),
            sysctl_scan_dirs: vec![
                root.join("etc/sysctl.d"),
                root.join("run/sysctl.d"),
                root.join("usr/local/lib/sysctl.d"),
                root.join("usr/lib/sysctl.d"),
                root.join("lib/sysctl.d"),
            ],
            sysctl_conf_path: root.join("etc/sysctl.conf"),
        }
    }
}

impl Default for HostNatPaths {
    fn default() -> Self {
        Self {
            rules_path: PathBuf::from(NAT_RULES_PATH),
            service_path: PathBuf::from(NAT_SERVICE_PATH),
            sysctl_path: PathBuf::from(NAT_SYSCTL_PATH),
            // Exact procps-ng `sysctl --system` directory precedence (highest priority first).
            sysctl_scan_dirs: vec![
                PathBuf::from("/etc/sysctl.d"),
                PathBuf::from("/run/sysctl.d"),
                PathBuf::from("/usr/local/lib/sysctl.d"),
                PathBuf::from("/usr/lib/sysctl.d"),
                PathBuf::from("/lib/sysctl.d"),
            ],
            sysctl_conf_path: PathBuf::from("/etc/sysctl.conf"),
        }
    }
}

/// Injectable command runner so NAT orchestration works locally, over SSH, or in tests.
pub trait HostRunner {
    fn run_raw(&mut self, host: &str, command: &str) -> Result<String>;
}

/// Minimal stage progress callback for NAT orchestration.
pub trait StageReporter {
    fn stage(&mut self, message: &str);
}

/// Normalized IPv4 CIDR block (network address + prefix length).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ipv4Subnet {
    pub network: Ipv4Addr,
    pub prefix_len: u8,
}

impl Ipv4Subnet {
    pub fn from_addr_prefix(addr: Ipv4Addr, prefix_len: u8) -> Result<Self> {
        ensure!(
            (1..=30).contains(&prefix_len),
            "bridge IPv4 prefix length must be between 1 and 30, got /{prefix_len}"
        );
        let mask = !0u32 << (32 - prefix_len);
        let network = Ipv4Addr::from(u32::from(addr) & mask);
        Ok(Self {
            network,
            prefix_len,
        })
    }

    /// Construct a CIDR prefix allowing `/0..=32` (used when parsing nftables rules).
    fn from_nft_prefix(addr: Ipv4Addr, prefix_len: u8) -> Option<Self> {
        if prefix_len > 32 {
            return None;
        }
        let mask = if prefix_len == 0 {
            0u32
        } else {
            !0u32 << (32 - prefix_len)
        };
        let network = Ipv4Addr::from(u32::from(addr) & mask);
        Some(Self {
            network,
            prefix_len,
        })
    }

    pub fn contains(self, ip: Ipv4Addr) -> bool {
        let mask = if self.prefix_len == 0 {
            0u32
        } else {
            !0u32 << (32 - self.prefix_len)
        };
        (u32::from(ip) & mask) == u32::from(self.network)
    }

    pub fn broadcast(self) -> Ipv4Addr {
        let host_bits = if self.prefix_len == 0 {
            !0u32
        } else {
            !(!0u32 << (32 - self.prefix_len))
        };
        Ipv4Addr::from(u32::from(self.network) | host_bits)
    }

    /// Returns true when `self` covers every address in `other`.
    fn covers(self, other: Self) -> bool {
        self.prefix_len <= other.prefix_len && self.contains(other.network)
    }

    pub fn cidr_string(self) -> String {
        format!("{}/{}", self.network, self.prefix_len)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOwnership {
    Absent,
    Owned,
    Foreign,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ManagedFileStates {
    pub rules: FileOwnership,
    pub service: FileOwnership,
    pub sysctl: FileOwnership,
}

#[derive(Clone, Debug)]
pub struct HostTarget {
    pub host: String,
    pub bridge: String,
    pub founder_ips: Vec<Ipv4Addr>,
}

/// Outcome of a single host apply operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyOutcome {
    DryRun,
    NoOpExistingEgress,
    Applied,
}
