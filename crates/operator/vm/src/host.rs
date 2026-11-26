//! Strictly read-only local host inspection, verification, and configuration recommendation
//! helpers (`outbe-up host status|setup|nat|publish|check`).
//!
//! Strictly local and strictly read-only: zero SSH, zero remote orchestration dependencies, and
//! zero writes or reloads to host networking, DHCP, sysctl, systemd, or nftables state.

use std::{
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::nat::{
    ExistingEgress, ForeignForwardChain, HostNatPaths, ManagedFileStates, PartialEgress,
    PersistentSysctlState,
};

mod bridge;
mod check;
mod dhcp;
mod dnsmasq_argv;
mod firewall;
mod nat_plan;
mod platform;
mod port_rules;
mod probe;
mod publish;
mod request;
mod status;
mod sysctl;
mod vmm_status;

#[cfg(test)]
mod tests;

pub use check::check_local_vm_host;
pub use firewall::parse_firewall_and_egress_json;
pub use nat_plan::plan_local_host_nat;
pub use publish::plan_host_publish;
pub use request::{normalize_ipv4_cidr, parse_port_spec};
pub use status::{evaluate_host_setup, inspect_host_status};

pub const HOST_STATUS_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_BRIDGE_NAME: &str = "obr0";

/// Injectable filesystem root & privilege expectations for local host inspection/tests.
#[derive(Clone, Debug)]
pub struct HostEnv {
    pub sys_root: PathBuf,
    pub expected_hosts_owner: (u32, u32),
    pub nat_paths: HostNatPaths,
}

impl Default for HostEnv {
    fn default() -> Self {
        Self::system()
    }
}

impl HostEnv {
    /// Production host environment rooted at `/` with `root:root` ownership required on
    /// `dhcp-hosts`.
    pub fn system() -> Self {
        Self {
            sys_root: PathBuf::from("/"),
            expected_hosts_owner: (0, 0),
            nat_paths: HostNatPaths::default(),
        }
    }

    /// Build a `HostEnv` rooted at `sys_root` (when `None` or `"/"`, ALWAYS uses production
    /// defaults with `root:root` ownership required on `dhcp-hosts`).
    pub fn from_sys_root(sys_root: Option<&Path>, expected_owner: Option<(u32, u32)>) -> Self {
        match sys_root {
            None => Self::system(),
            Some(root) if root == Path::new("/") => Self::system(),
            Some(root) => {
                let default_owner = (
                    rustix::process::getuid().as_raw(),
                    rustix::process::getgid().as_raw(),
                );
                Self {
                    sys_root: root.to_path_buf(),
                    expected_hosts_owner: expected_owner.unwrap_or(default_owner),
                    nat_paths: HostNatPaths::under_root(root),
                }
            }
        }
    }

    fn path(&self, abs: &str) -> PathBuf {
        if self.sys_root == Path::new("/") {
            PathBuf::from(abs)
        } else {
            self.sys_root.join(abs.trim_start_matches('/'))
        }
    }
}

/// Operator inputs that name one local VM and the host ports to publish for it.
///
/// `outbe-up host check` and `outbe-up host publish` use these inputs.
#[derive(Clone, Copy, Debug)]
pub struct VmPortsRequest<'a> {
    /// Local VM name in the local VM store.
    pub vm_name: &'a str,
    /// Raw `--port <guest>[:<host>][/(tcp|udp)]` values.
    pub raw_ports: &'a [String],
    /// Raw `--source <ipv4>[/<prefix>]` values.
    pub raw_sources: &'a [String],
    /// Host public IPv4 address from `--public-ip`.
    pub public_ip: Option<Ipv4Addr>,
    /// Bridge name from `--bridge`. The recorded VM bridge applies when it is `None`.
    pub bridge: Option<&'a str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HostStatusReport {
    pub version: u32,
    pub bridge: BridgeStatus,
    pub dhcp: DhcpStatus,
    pub firewall: FirewallStatus,
    pub nat: NatStatus,
    pub vmm: VmmStatus,
    pub platform: PlatformStatus,
    pub ready_for_vm: bool,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BridgeStatus {
    pub name: String,
    pub present: bool,
    pub up: bool,
    pub cidr: Option<String>,
    pub gateway_ip: Option<Ipv4Addr>,
    pub management: String,
    pub unit_name: Option<String>,
    pub unit_enabled: bool,
    pub unit_active: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct DhcpStatus {
    pub detected: bool,
    pub unit_name: Option<String>,
    pub unit_enabled: bool,
    pub unit_active: bool,
    pub pid: Option<u32>,
    pub hosts_file: Option<PathBuf>,
    pub leases_file: Option<PathBuf>,
    pub pid_file: Option<PathBuf>,
    pub pool_start: Option<Ipv4Addr>,
    pub pool_end: Option<Ipv4Addr>,
    pub netmask: Option<Ipv4Addr>,
    pub lease_duration: Option<String>,
    pub reload_mode: String,
    pub reservations_count: usize,
    pub active_leases_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FirewallTableRef {
    pub family: String,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FirewallForwardAllow {
    pub family: String,
    pub table: String,
    pub chain: String,
    pub dest_ip: Ipv4Addr,
    pub proto: String,
    pub dest_port: u16,
    pub sources: Vec<String>,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FirewallDnatRule {
    pub family: String,
    pub table: String,
    pub chain: String,
    pub public_ip: Option<Ipv4Addr>,
    pub proto: String,
    pub host_port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_port_end: Option<u16>,
    pub dest_ip: Ipv4Addr,
    pub guest_port: u16,
    pub sources: Vec<String>,
    pub comment: Option<String>,
    pub handle: Option<u64>,
}

impl FirewallDnatRule {
    /// Returns true if this DNAT rule matches `port` (single port or inclusive range).
    pub fn covers_host_port(&self, port: u16) -> bool {
        let end = self.host_port_end.unwrap_or(self.host_port);
        port >= self.host_port && port <= end
    }
}

/// A live nftables DNAT rule whose destination ports or target could not be statically resolved
/// (e.g. named set `@ports`, DNAT map/vmap, non-equality `dport` operator, or wildcard DNAT).
///
/// Recorded explicitly in [`FirewallStatus`] and causes [`plan_host_publish`] to fail closed (F4).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UnparsedDnatRule {
    pub family: String,
    pub table: String,
    pub chain: String,
    pub handle: Option<u64>,
    pub comment: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct FirewallStatus {
    pub nft_accessible: bool,
    pub tables: Vec<FirewallTableRef>,
    pub forward_chains: Vec<ForeignForwardChain>,
    pub forward_allows: Vec<FirewallForwardAllow>,
    pub dnat_rules: Vec<FirewallDnatRule>,
    pub unparsed_dnat_rules: Vec<UnparsedDnatRule>,
    pub managed_nat_table_present: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct NatStatus {
    pub uplink: Option<String>,
    pub ip_forward_runtime: Option<u8>,
    pub ip_forward_persistent: PersistentSysctlState,
    pub egress_status: String,
    pub existing_egress: Vec<ExistingEgress>,
    pub partial_egress: Vec<PartialEgress>,
    pub managed_files: ManagedFileStates,
}

#[derive(Clone, Debug, Serialize)]
pub struct VmmStatus {
    pub socket_path: PathBuf,
    pub reachable: bool,
    pub version: Option<String>,
    pub rev: Option<String>,
    pub default_mode: Option<String>,
    pub supported_modes: Vec<String>,
    pub default_bridge: Option<String>,
    pub max_cvm_number: Option<u32>,
    pub max_allocable_vcpu: Option<u32>,
    pub max_allocable_memory_in_mb: Option<u64>,
    pub running_vms: Option<usize>,
    pub total_vms: Option<usize>,
    pub systemd_unit_enabled: bool,
    pub systemd_unit_active: bool,
    pub drift_notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct QgsStatus {
    pub required: bool,
    pub service_name: String,
    pub unit_enabled: bool,
    pub unit_active: bool,
    pub pccs_unit_active: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlatformStatus {
    pub kvm_available: bool,
    pub kvm_accessible: bool,
    pub vhost_vsock_available: bool,
    pub tee_kind: String,
    pub sev_snp_enabled: bool,
    pub tdx_enabled: bool,
    pub qgs: QgsStatus,
}

#[derive(Clone, Debug, Serialize)]
pub struct HostSetupReport {
    pub already_configured: bool,
    pub dry_run: bool,
    pub writes_performed: usize,
    pub missing_components: Vec<String>,
    pub planned_steps: Vec<String>,
    pub approval_note: Option<String>,
    pub status: HostStatusReport,
}

#[derive(Clone, Debug, Serialize)]
pub struct HostNatLocalReport {
    pub action: String,
    pub bridge: String,
    pub uplink: Option<String>,
    pub outcome: String,
    pub writes_performed: usize,
    pub ip_forward_runtime: Option<u8>,
    pub ip_forward_persistent: Option<PersistentSysctlState>,
    pub existing_egress: Vec<ExistingEgress>,
    pub partial_egress: Vec<PartialEgress>,
    pub foreign_forward_chains: Vec<ForeignForwardChain>,
    pub managed_files: Option<ManagedFileStates>,
    pub rendered_ruleset: Option<String>,
    pub rendered_unit: Option<String>,
    pub rendered_sysctl: Option<String>,
    pub recommended_commands: Vec<String>,
    pub warnings: Vec<String>,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct HostCheckReport {
    pub vm: String,
    pub vm_id: String,
    pub mac: String,
    pub ip: Ipv4Addr,
    pub bridge: String,
    pub public_ip: Option<Ipv4Addr>,
    pub ports: Vec<PublishPortSpec>,
    pub ready: bool,
    pub host_network_ok: bool,
    pub service_ip_ok: bool,
    pub dhcp_reserved: bool,
    pub published_ports_ok: bool,
    pub missing: Vec<String>,
    pub conflicts: Vec<String>,
    pub port_conflicts: Vec<PublishPortConflict>,
    pub hosts_file: Option<PathBuf>,
    pub dhcp_unit: Option<String>,
    pub recommended_dhcp_block: Option<String>,
    pub recommended_dhcp_reload: Option<String>,
    pub recommended_forward_rules: Vec<String>,
    pub recommended_prerouting_rules: Vec<String>,
    pub recommended_firewall_commands: Vec<String>,
    pub writes_performed: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportProto {
    Tcp,
    Udp,
}

impl TransportProto {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PublishPortSpec {
    pub guest_port: u16,
    pub host_port: u16,
    pub proto: TransportProto,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PublishPortConflict {
    pub proto: String,
    pub host_port: u16,
    pub requested_dest_ip: Ipv4Addr,
    pub requested_guest_port: u16,
    pub existing_table: String,
    pub existing_chain: String,
    pub existing_dest_ip: Ipv4Addr,
    pub existing_guest_port: u16,
    pub existing_sources: Vec<String>,
    pub existing_comment: Option<String>,
    pub existing_handle: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HostPublishReport {
    pub vm: String,
    pub vm_id: String,
    pub guest_ip: Ipv4Addr,
    pub public_ip: Ipv4Addr,
    pub sources: Vec<String>,
    pub ports: Vec<PublishPortSpec>,
    pub firewall_file: PathBuf,
    pub firewall_service: String,
    pub forward_table: String,
    pub forward_policy: String,
    pub nat_table: String,
    pub forward_rules: Vec<String>,
    pub prerouting_rules: Vec<String>,
    pub apply_commands: Vec<String>,
    pub remove_commands: Vec<String>,
    pub conflicts: Vec<PublishPortConflict>,
    pub already_published_ports: Vec<PublishPortSpec>,
    pub writes_performed: usize,
}

impl BridgeStatus {
    /// Returns true if the bridge exists, is up, and has an IPv4 CIDR.
    pub(crate) fn is_ready(&self) -> bool {
        self.present && self.up && self.cidr.is_some()
    }
}

impl DhcpStatus {
    /// Returns true if a DHCP service is detected and active.
    pub(crate) fn is_serving(&self) -> bool {
        self.detected && self.unit_active
    }
}

impl NatStatus {
    /// Returns true if runtime IPv4 forwarding is on and outbound egress is provided.
    pub(crate) fn has_egress(&self) -> bool {
        self.ip_forward_runtime == Some(1) && egress_status_provides_egress(&self.egress_status)
    }
}

/// Returns true if an egress status string means that outbound NAT is available.
pub(crate) fn egress_status_provides_egress(egress_status: &str) -> bool {
    matches!(egress_status, "foreign_provided" | "managed")
}
