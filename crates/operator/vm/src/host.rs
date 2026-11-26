//! Strictly read-only local host inspection, verification, and configuration recommendation
//! helpers (`outbe-up host status|setup|nat|reserve|release|publish|check`).
//!
//! Strictly local and strictly read-only: zero SSH, zero remote orchestration dependencies, and
//! zero writes or reloads to host networking, DHCP, sysctl, systemd, or nftables state.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::Ipv4Addr,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
};

use eyre::{bail, ensure, Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::{
    dnsmasq::{
        check_dnsmasq_ip_conflicts, check_dnsmasq_lease_conflicts, check_dnsmasq_mac_conflicts,
        check_dnsmasq_reservation_status, count_dnsmasq_leases, lookup_lease_ip_by_mac,
        merge_dnsmasq_hosts_checked, render_vm_dnsmasq_block, strip_dnsmasq_block_checked,
        validate_dnsmasq_hosts_format,
    },
    local::{validate_vm_name, LocalVmPhase, LocalVmStore},
    nat::{
        build_apply_command, build_remove_command, inspect_nft_json, parse_persistent_sysctl,
        render_nat_ruleset, render_systemd_unit, validate_iface_name, ExistingEgress,
        FileOwnership, ForeignForwardChain, HostNatPaths, Ipv4Subnet, ManagedFileStates,
        PartialEgress, PersistentSysctlState, NAT_TABLE_FAMILY, NAT_TABLE_NAME, OWNERSHIP_MARKER,
    },
    vmm::{extract_primary_mac, StatusRequest, VmmClient, VmmTransport},
};

pub const HOST_STATUS_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_BRIDGE_NAME: &str = "obr0";
const VM_BRIDGE_SERVICE: &str = "vm-bridge.service";
const VALIDATOR_DHCP_SERVICE: &str = "validator-vm-dhcp.service";
const DSTACK_VMM_SERVICE: &str = "dstack-vmm.service";
const QGSD_SERVICE: &str = "qgsd.service";
const PCCS_SERVICE: &str = "pccs.service";
const HOST_FIREWALL_NFT_PATH: &str = "/etc/host-firewall.nft";
const HOST_FIREWALL_SERVICE: &str = "host-firewall.service";
const STATUS_PAGE_SIZE: u32 = 100;
const MAX_STATUS_PAGES: u32 = 100;

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
pub struct HostReserveReport {
    pub vm: String,
    pub vm_id: String,
    pub mac: String,
    pub ip: Ipv4Addr,
    pub bridge: String,
    pub dhcp_unit: String,
    pub hosts_file: PathBuf,
    pub block_marker: String,
    pub block_content: String,
    pub reload_command: String,
    pub already_present: bool,
    pub service_active: bool,
    pub writes_performed: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct HostReleaseReport {
    pub vm: String,
    pub bridge: String,
    pub dhcp_unit: String,
    pub hosts_file: PathBuf,
    pub block_marker: String,
    pub existing_block: Option<String>,
    pub reload_command: String,
    pub block_present: bool,
    pub service_active: bool,
    pub writes_performed: usize,
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

/// Inspect the local host and return a stable v1 [`HostStatusReport`]. Strictly read-only.
pub fn inspect_host_status<T: VmmTransport>(
    vmm: &VmmClient<T>,
    vmm_socket: &Path,
    bridge_override: Option<&str>,
    env: &HostEnv,
) -> Result<HostStatusReport> {
    let mut notes = Vec::new();
    let vmm_status = detect_vmm_status(vmm, vmm_socket);
    if !vmm_status.reachable {
        notes.push(format!(
            "local VMM socket {} is not reachable",
            vmm_socket.display()
        ));
    }

    let bridge_name = match bridge_override {
        Some(name) => {
            validate_iface_name(name, "bridge")?;
            name.to_owned()
        }
        None => vmm_status
            .default_bridge
            .as_deref()
            .filter(|b| !b.is_empty() && validate_iface_name(b, "bridge").is_ok())
            .unwrap_or(DEFAULT_BRIDGE_NAME)
            .to_owned(),
    };

    let (bridge, bridge_subnet) = detect_bridge(&bridge_name);
    let dhcp = detect_dhcp(&bridge_name, env);
    let uplink = detect_default_uplink();
    let ip_forward_runtime = read_runtime_ip_forward(env);
    let ip_forward_persistent = scan_persistent_ip_forward(&env.nat_paths);
    let managed_files = probe_managed_nat_files(&env.nat_paths);

    let (firewall, existing_egress, partial_egress) =
        detect_firewall_and_egress(&bridge_name, bridge_subnet, uplink.as_deref());
    if !firewall.nft_accessible {
        notes.push(
            "nftables ruleset unavailable (requires CAP_NET_ADMIN / passwordless `sudo -n nft`)"
                .to_owned(),
        );
    } else if !firewall.unparsed_dnat_rules.is_empty() {
        notes.push(format!(
            "live nftables ruleset contains {} unparseable DNAT rule(s); `outbe-up host publish` will fail closed until resolved",
            firewall.unparsed_dnat_rules.len()
        ));
    }

    let egress_status = classify_egress_status(
        firewall.nft_accessible,
        firewall.managed_nat_table_present,
        &existing_egress,
        &partial_egress,
    );

    let nat = NatStatus {
        uplink,
        ip_forward_runtime,
        ip_forward_persistent,
        egress_status: egress_status.to_owned(),
        existing_egress,
        partial_egress,
        managed_files,
    };

    let platform = detect_platform(env);
    if platform.qgs.required && !platform.qgs.unit_active {
        notes.push("Intel TDX detected but qgsd.service is not active".to_owned());
    }

    let ready_for_vm = bridge.present
        && bridge.up
        && bridge.cidr.is_some()
        && dhcp.detected
        && dhcp.unit_active
        && vmm_status.reachable
        && nat.ip_forward_runtime == Some(1)
        && matches!(nat.egress_status.as_str(), "foreign_provided" | "managed");

    Ok(HostStatusReport {
        version: HOST_STATUS_SCHEMA_VERSION,
        bridge,
        dhcp,
        firewall,
        nat,
        vmm: vmm_status,
        platform,
        ready_for_vm,
        notes,
    })
}

/// Evaluate `outbe-up host setup`: pure read-only no-op when bridge + DHCP + egress are already
/// configured; on an unconfigured host prints exact steps and refuses to write without owner
/// design approval.
pub fn evaluate_host_setup<T: VmmTransport>(
    vmm: &VmmClient<T>,
    vmm_socket: &Path,
    bridge_override: Option<&str>,
    dry_run: bool,
    env: &HostEnv,
) -> Result<HostSetupReport> {
    let status = inspect_host_status(vmm, vmm_socket, bridge_override, env)?;
    ensure!(
        status.firewall.nft_accessible,
        "cannot verify host firewall/egress state: `nft -j list ruleset` is inaccessible (requires root or passwordless `sudo -n nft`)"
    );

    let mut missing_components = Vec::new();
    let mut planned_steps = Vec::new();

    if !status.bridge.present || !status.bridge.up || status.bridge.cidr.is_none() {
        missing_components.push(format!("bridge `{}`", status.bridge.name));
        planned_steps.push(format!(
            "Create and persist VM bridge `{}` (e.g. `10.77.0.1/24`) via systemd unit `vm-bridge.service`.",
            status.bridge.name
        ));
    }
    if !status.dhcp.detected || !status.dhcp.unit_active {
        missing_components.push(format!("DHCP service on bridge `{}`", status.bridge.name));
        planned_steps.push(format!(
            "Configure and enable a dnsmasq DHCP service on `{}` with `--dhcp-range` and `--dhcp-leasefile`.",
            status.bridge.name
        ));
    }
    let has_egress = status.nat.ip_forward_runtime == Some(1)
        && matches!(
            status.nat.egress_status.as_str(),
            "foreign_provided" | "managed"
        );
    if !has_egress {
        missing_components.push("outbound NAT / IPv4 forwarding".to_owned());
        planned_steps.push(format!(
            "Enable persistent `net.ipv4.ip_forward=1` and outbound masquerade for `{}` (e.g. `outbe-up host nat --bridge {}`).",
            status.bridge.name, status.bridge.name
        ));
    }

    let already_configured = missing_components.is_empty();
    let approval_note = if already_configured {
        None
    } else {
        Some(
            "Refusing to provision missing host components automatically: owner design approval is required before outbe-up creates host bridge/DHCP services (zero changes made)."
                .to_owned(),
        )
    };

    Ok(HostSetupReport {
        already_configured,
        dry_run,
        writes_performed: 0,
        missing_components,
        planned_steps,
        approval_note,
        status,
    })
}

/// Inspect local host NAT / IPv4 forwarding state and render recommended ruleset, systemd unit,
/// sysctl drop-in, and operator apply/remove commands. Strictly read-only (zero writes).
pub fn plan_local_host_nat(
    bridge: &str,
    uplink_override: Option<&str>,
    remove: bool,
    env: &HostEnv,
) -> Result<HostNatLocalReport> {
    validate_iface_name(bridge, "bridge")?;
    let paths = &env.nat_paths;
    let uplink = match uplink_override {
        Some(u) => {
            validate_iface_name(u, "uplink")?;
            Some(u.to_owned())
        }
        None => detect_default_uplink(),
    };
    let ip_forward_runtime = read_runtime_ip_forward(env);
    let ip_forward_persistent = scan_persistent_ip_forward(paths);
    let managed_files = probe_managed_nat_files(paths);

    if remove {
        let remove_cmd = build_remove_command(paths);
        return Ok(HostNatLocalReport {
            action: "remove".to_owned(),
            bridge: bridge.to_owned(),
            uplink,
            outcome: "print_only".to_owned(),
            writes_performed: 0,
            ip_forward_runtime,
            ip_forward_persistent: Some(ip_forward_persistent),
            existing_egress: Vec::new(),
            partial_egress: Vec::new(),
            foreign_forward_chains: Vec::new(),
            managed_files: Some(managed_files),
            rendered_ruleset: None,
            rendered_unit: None,
            rendered_sysctl: None,
            recommended_commands: vec![remove_cmd],
            warnings: Vec::new(),
            detail: "Read-only removal recommendation (zero writes performed). Run the recommended command manually to remove outbe-up managed NAT artifacts while preserving foreign files and runtime net.ipv4.ip_forward.".to_owned(),
        });
    }

    let (_, bridge_subnet) = detect_bridge(bridge);
    let subnet = bridge_subnet.ok_or_else(|| {
        eyre::eyre!("bridge `{bridge}` is missing or has no IPv4 CIDR configured")
    })?;
    let uplink_name = uplink.clone().ok_or_else(|| {
        eyre::eyre!("could not detect default IPv4 uplink interface; pass `--uplink <iface>`")
    })?;

    let (firewall, existing_egress, partial_egress) =
        detect_firewall_and_egress(bridge, Some(subnet), Some(&uplink_name));
    ensure!(
        firewall.nft_accessible,
        "cannot inspect live nftables ruleset: `nft -j list ruleset` is inaccessible (requires root or passwordless `sudo -n nft`)"
    );

    if ip_forward_runtime == Some(1) && !existing_egress.is_empty() {
        let providers = existing_egress
            .iter()
            .map(|e| format!("{} {} {}", e.family, e.table, e.chain))
            .collect::<Vec<_>>()
            .join(", ");
        return Ok(HostNatLocalReport {
            action: "recommend".to_owned(),
            bridge: bridge.to_owned(),
            uplink: Some(uplink_name),
            outcome: "no_op_existing_egress".to_owned(),
            writes_performed: 0,
            ip_forward_runtime,
            ip_forward_persistent: Some(ip_forward_persistent),
            existing_egress,
            partial_egress,
            foreign_forward_chains: firewall.forward_chains,
            managed_files: Some(managed_files),
            rendered_ruleset: None,
            rendered_unit: None,
            rendered_sysctl: None,
            recommended_commands: Vec::new(),
            warnings: Vec::new(),
            detail: format!(
                "egress already provided by {providers} with net.ipv4.ip_forward=1; no NAT changes required (zero writes)"
            ),
        });
    }

    let mut warnings = Vec::new();
    if existing_egress.is_empty() && !partial_egress.is_empty() {
        let rules = partial_egress
            .iter()
            .map(|p| {
                format!(
                    "{} {}::{} ({}, narrowed by: {})",
                    p.family,
                    p.table,
                    p.chain,
                    p.kind,
                    p.narrowing.join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        warnings.push(format!(
            "partial foreign masquerade/snat rule(s) detected: {rules}; inspect before applying"
        ));
    }

    let need_sysctl_dropin = ip_forward_persistent.effective_value != Some(1);
    if need_sysctl_dropin && ip_forward_persistent.overridden_by_later_zero {
        let overriding = ip_forward_persistent
            .effective_source
            .as_deref()
            .unwrap_or("unknown sysctl file");
        warnings.push(format!(
            "`{overriding}` sets net.ipv4.ip_forward=0 and overrides {} on reboot",
            paths.sysctl_path.display()
        ));
    }
    let mut foreign_collisions = Vec::new();
    if managed_files.rules == FileOwnership::Foreign {
        foreign_collisions.push(paths.rules_path.display().to_string());
    }
    if managed_files.service == FileOwnership::Foreign {
        foreign_collisions.push(paths.service_path.display().to_string());
    }
    if need_sysctl_dropin && managed_files.sysctl == FileOwnership::Foreign {
        foreign_collisions.push(paths.sysctl_path.display().to_string());
    }
    if !foreign_collisions.is_empty() {
        warnings.push(format!(
            "unmanaged file(s) already exist (missing `{OWNERSHIP_MARKER}` marker): {}",
            foreign_collisions.join(", ")
        ));
    }
    for chain in &firewall.forward_chains {
        if !chain.reasons.is_empty() {
            warnings.push(format!(
                "foreign forward base chain {} {}::{} (policy {}) may restrict bridge `{bridge}` forwarding: {}",
                chain.family,
                chain.table,
                chain.chain,
                chain.policy,
                chain.reasons.join("; ")
            ));
        }
    }

    let ruleset = render_nat_ruleset(bridge, subnet, &uplink_name)?;
    let unit = render_systemd_unit(&paths.rules_path);
    let rendered_sysctl =
        need_sysctl_dropin.then(|| format!("{OWNERSHIP_MARKER}\nnet.ipv4.ip_forward = 1\n"));
    let enable_runtime_ip_forward = ip_forward_runtime != Some(1);
    let apply_cmd = build_apply_command(
        &ruleset,
        &unit,
        need_sysctl_dropin,
        enable_runtime_ip_forward,
        paths,
    );

    Ok(HostNatLocalReport {
        action: "recommend".to_owned(),
        bridge: bridge.to_owned(),
        uplink: Some(uplink_name),
        outcome: "print_only".to_owned(),
        writes_performed: 0,
        ip_forward_runtime,
        ip_forward_persistent: Some(ip_forward_persistent),
        existing_egress,
        partial_egress,
        foreign_forward_chains: firewall.forward_chains,
        managed_files: Some(managed_files),
        rendered_ruleset: Some(ruleset),
        rendered_unit: Some(unit),
        rendered_sysctl,
        recommended_commands: vec![apply_cmd],
        warnings,
        detail: format!(
            "Outbound NAT egress for bridge `{bridge}` is not yet configured. Apply the recommended nftables ruleset (`table {NAT_TABLE_FAMILY} {NAT_TABLE_NAME}`), systemd unit, and sysctl setting manually (read-only; zero writes)."
        ),
    })
}

/// Validate conflicts and render a per-VM static DHCP reservation (`# outbe-up:vm:<name>:begin..end`)
/// plus `sudo systemctl reload <unit>`. Strictly read-only (zero writes).
pub fn reserve_local_vm_dhcp<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    vm_name: &str,
    explicit_ip: Option<Ipv4Addr>,
    bridge_override: Option<&str>,
    env: &HostEnv,
) -> Result<HostReserveReport> {
    validate_vm_name(vm_name)?;

    let path = store.state_path(vm_name)?;
    let raw = fs::read_to_string(&path).wrap_err_with(|| {
        format!(
            "local VM `{vm_name}` is not recorded in {} (create it first with `outbe-up vm create`)",
            path.display()
        )
    })?;
    let state: crate::local::LocalVmState = serde_json::from_str(&raw)
        .wrap_err_with(|| format!("parse local VM state {}", path.display()))?;

    ensure!(
        !state.vm_id.is_empty() && state.phase != LocalVmPhase::Removing,
        "VM `{vm_name}` is in phase `{:?}` and cannot be reserved",
        state.phase
    );

    let info_resp = vmm.get_info(&state.vm_id)?;
    ensure!(
        !info_resp.is_gone(),
        "VM `{vm_name}` ({}) is missing from local VMM",
        state.vm_id
    );
    let info = info_resp.info.unwrap();
    ensure!(
        info.name == state.name,
        "VMM VM {} has unexpected name `{}` (expected `{}`)",
        state.vm_id,
        info.name,
        state.name
    );
    let live_mac = extract_primary_mac(&info)?;
    if !state.mac.is_empty() {
        ensure!(
            state.mac.eq_ignore_ascii_case(&live_mac),
            "VM `{vm_name}` MAC mismatch: recorded state has `{}`, live VMM reports `{live_mac}`",
            state.mac
        );
    }

    let ip = match (explicit_ip, state.intended_ip) {
        (Some(given), Some(recorded)) => {
            ensure!(
                given == recorded,
                "VM `{vm_name}` already has recorded intended_ip {recorded}, which disagrees with --ip {given}"
            );
            given
        }
        (Some(given), None) => given,
        (None, Some(recorded)) => recorded,
        (None, None) => {
            bail!(
                "no IPv4 address for VM `{vm_name}`: pass `--ip <ipv4>` or record one during `outbe-up vm create --ip <ipv4>`"
            );
        }
    };

    let bridge_name = bridge_override.unwrap_or(&state.bridge).to_owned();
    validate_iface_name(&bridge_name, "bridge")?;
    let (bridge_status, bridge_subnet) = detect_bridge(&bridge_name);
    let subnet = bridge_subnet.ok_or_else(|| {
        eyre::eyre!("bridge `{bridge_name}` is missing or has no IPv4 CIDR configured")
    })?;
    ensure!(
        subnet.contains(ip),
        "requested IP {ip} is outside bridge `{bridge_name}` subnet {}",
        subnet.cidr_string()
    );
    ensure!(
        ip != subnet.network && ip != subnet.broadcast(),
        "requested IP {ip} is the network or broadcast address of {}",
        subnet.cidr_string()
    );
    if let Some(gw) = bridge_status.gateway_ip {
        ensure!(
            ip != gw,
            "requested IP {ip} is the bridge `{bridge_name}` gateway address"
        );
    }

    let dhcp = detect_dhcp(&bridge_name, env);
    ensure!(
        dhcp.detected && dhcp.unit_active,
        "no active DHCP service detected on bridge `{bridge_name}`"
    );
    let dhcp_unit = dhcp.unit_name.clone().ok_or_else(|| {
        eyre::eyre!(
            "running dnsmasq on `{bridge_name}` is not managed by a detected systemd unit; refusing to guess reload mechanism"
        )
    })?;
    ensure!(
        dhcp.reload_mode == "systemctl-reload",
        "DHCP service `{dhcp_unit}` does not advertise ExecReload; refusing unsafe reload"
    );
    let hosts_file = dhcp.hosts_file.clone().ok_or_else(|| {
        eyre::eyre!("DHCP service `{dhcp_unit}` does not specify `--dhcp-hostsfile`")
    })?;

    // Verify hosts file is a regular file with expected owner and parseable dnsmasq format.
    let existing_hosts = read_and_validate_hosts_file(&hosts_file, env.expected_hosts_owner)?;
    let block_key = format!("vm:{vm_name}");
    let requested = vec![(live_mac.clone(), ip)];
    check_dnsmasq_ip_conflicts(&existing_hosts, &requested, &block_key)?;
    check_dnsmasq_mac_conflicts(&existing_hosts, &requested, &block_key)?;

    if let Some(leases_file) = &dhcp.leases_file {
        if let Ok(leases_content) = fs::read_to_string(leases_file) {
            check_dnsmasq_lease_conflicts(&leases_content, &requested)?;
        }
    }

    // Check other locally tracked VMs in store for IP/MAC collisions.
    for other_name in store.list_vm_names()? {
        if other_name == vm_name {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(store.state_path(&other_name)?) {
            if let Ok(other) = serde_json::from_str::<crate::local::LocalVmState>(&raw) {
                ensure!(
                    other.intended_ip != Some(ip),
                    "requested IP {ip} is already recorded for local VM `{other_name}`"
                );
                ensure!(
                    !other.mac.eq_ignore_ascii_case(&live_mac),
                    "MAC {live_mac} is already recorded for local VM `{other_name}`"
                );
            }
        }
    }

    let splice = merge_dnsmasq_hosts_checked(&existing_hosts, &requested, &block_key)?;
    let block_content = render_vm_dnsmasq_block(vm_name, &live_mac, ip);
    let already_present = splice.exact_match;
    let reload_command = format!("sudo systemctl reload {dhcp_unit}");

    Ok(HostReserveReport {
        vm: vm_name.to_owned(),
        vm_id: state.vm_id,
        mac: live_mac,
        ip,
        bridge: bridge_name,
        dhcp_unit,
        hosts_file,
        block_marker: format!("# outbe-up:vm:{vm_name}:begin..end"),
        block_content,
        reload_command,
        already_present,
        service_active: dhcp.unit_active,
        writes_performed: 0,
    })
}

/// Inspect and print the exact `# outbe-up:vm:<name>:begin..end` block to remove from the
/// discovered DHCP hosts file, plus `sudo systemctl reload <unit>`. Works even if `<name>` has
/// already been removed from `LocalVmStore`. Strictly read-only (zero writes).
pub fn release_local_vm_dhcp(
    store: &LocalVmStore,
    vm_name: &str,
    bridge_override: Option<&str>,
    env: &HostEnv,
) -> Result<HostReleaseReport> {
    validate_vm_name(vm_name)?;

    let recorded_state = store
        .state_path(vm_name)
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str::<crate::local::LocalVmState>(&raw).ok());

    let bridge_name = match bridge_override {
        Some(b) => {
            validate_iface_name(b, "bridge")?;
            b.to_owned()
        }
        None => recorded_state
            .as_ref()
            .map(|st| st.bridge.clone())
            .unwrap_or_else(|| DEFAULT_BRIDGE_NAME.to_owned()),
    };

    let dhcp = detect_dhcp(&bridge_name, env);
    ensure!(
        dhcp.detected && dhcp.unit_active,
        "no active DHCP service detected on bridge `{bridge_name}`"
    );
    let dhcp_unit = dhcp.unit_name.clone().ok_or_else(|| {
        eyre::eyre!(
            "running dnsmasq on `{bridge_name}` is not managed by a detected systemd unit; refusing to guess reload mechanism"
        )
    })?;
    ensure!(
        dhcp.reload_mode == "systemctl-reload",
        "DHCP service `{dhcp_unit}` does not advertise ExecReload; refusing unsafe reload"
    );
    let hosts_file = dhcp.hosts_file.clone().ok_or_else(|| {
        eyre::eyre!("DHCP service `{dhcp_unit}` does not specify `--dhcp-hostsfile`")
    })?;

    let existing_hosts = read_and_validate_hosts_file(&hosts_file, env.expected_hosts_owner)?;
    let block_key = format!("vm:{vm_name}");
    let expected_mac = recorded_state
        .as_ref()
        .map(|st| st.mac.as_str())
        .filter(|m| !m.is_empty());
    let splice = strip_dnsmasq_block_checked(&existing_hosts, &block_key, expected_mac)?;
    let block_present = splice.exact_match;
    let existing_block = splice.existing_block;
    let reload_command = format!("sudo systemctl reload {dhcp_unit}");

    Ok(HostReleaseReport {
        vm: vm_name.to_owned(),
        bridge: bridge_name,
        dhcp_unit,
        hosts_file,
        block_marker: format!("# outbe-up:vm:{vm_name}:begin..end"),
        existing_block,
        reload_command,
        block_present,
        service_active: dhcp.unit_active,
        writes_performed: 0,
    })
}

/// Strictly read-only verifier checking that host networking (bridge, IPv4 forwarding, outbound
/// NAT), the VM's static internal IP (inside the bridge subnet, outside any host DHCP pool, and
/// unused), and any requested published ports (both prerouting DNAT and forward accept rules in
/// live nftables) are configured with zero conflicts.
///
/// DHCP is not required on the host for static VMs (Decision 10, revised); if a host DHCP service
/// is present, pool exclusion and reservation/lease conflict checks are still enforced.
#[allow(clippy::too_many_arguments)]
pub fn check_local_vm_host<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    vm_name: &str,
    explicit_ip: Option<Ipv4Addr>,
    raw_ports: &[String],
    raw_sources: &[String],
    public_ip_override: Option<Ipv4Addr>,
    bridge_override: Option<&str>,
    env: &HostEnv,
) -> Result<HostCheckReport> {
    validate_vm_name(vm_name)?;

    let mut ports = Vec::with_capacity(raw_ports.len());
    let mut seen_host_ports = BTreeSet::new();
    for raw in raw_ports {
        let spec = parse_port_spec(raw)?;
        ensure!(
            seen_host_ports.insert((spec.proto.as_str(), spec.host_port)),
            "duplicate host port {}/{} in --port arguments",
            spec.proto.as_str(),
            spec.host_port
        );
        ports.push(spec);
    }

    let explicit_sources = raw_sources
        .iter()
        .map(|s| normalize_ipv4_cidr(s))
        .collect::<Result<Vec<_>>>()?;
    let recommended_sources = if explicit_sources.is_empty() {
        vec!["0.0.0.0/0".to_owned()]
    } else {
        explicit_sources.clone()
    };

    let state_path = store.state_path(vm_name)?;
    let raw_state = fs::read_to_string(&state_path).wrap_err_with(|| {
        format!(
            "local VM `{vm_name}` is not recorded in {} (create it first with `outbe-up vm create`)",
            state_path.display()
        )
    })?;
    let state: crate::local::LocalVmState = serde_json::from_str(&raw_state)
        .wrap_err_with(|| format!("parse local VM state {}", state_path.display()))?;
    ensure!(
        !state.vm_id.is_empty() && state.phase != LocalVmPhase::Removing,
        "VM `{vm_name}` is in phase `{:?}` and cannot be checked",
        state.phase
    );

    let info_resp = vmm.get_info(&state.vm_id)?;
    ensure!(
        !info_resp.is_gone(),
        "VM `{vm_name}` ({}) is missing from local VMM",
        state.vm_id
    );
    let info = info_resp.info.unwrap();
    ensure!(
        info.name == state.name,
        "VMM VM {} has unexpected name `{}` (expected `{}`)",
        state.vm_id,
        info.name,
        state.name
    );
    let live_mac = extract_primary_mac(&info)?;
    if !state.mac.is_empty() {
        ensure!(
            state.mac.eq_ignore_ascii_case(&live_mac),
            "VM `{vm_name}` MAC mismatch: recorded state has `{}`, live VMM reports `{live_mac}`",
            state.mac
        );
    }

    let ip = match (explicit_ip, state.intended_ip) {
        (Some(given), Some(recorded)) => {
            ensure!(
                given == recorded,
                "VM `{vm_name}` already has recorded intended_ip {recorded}, which disagrees with --ip {given}"
            );
            given
        }
        (Some(given), None) => given,
        (None, Some(recorded)) => recorded,
        (None, None) => {
            bail!(
                "no IPv4 address for VM `{vm_name}`: pass `--ip <ipv4>` or record one during `outbe-up vm create --ip <ipv4>`"
            );
        }
    };

    let bridge_name = bridge_override.unwrap_or(&state.bridge).to_owned();
    validate_iface_name(&bridge_name, "bridge")?;

    let mut missing = Vec::new();
    let mut conflicts = Vec::new();
    let mut port_conflicts = Vec::new();

    // 1. Host networking checks (bridge, active DHCP, IPv4 forwarding, outbound NAT egress).
    let (bridge_status, bridge_subnet) = detect_bridge(&bridge_name);
    let bridge_ok = bridge_status.present && bridge_status.up && bridge_subnet.is_some();
    let mut bridge_ip_ok = false;
    if !bridge_ok {
        missing.push(format!(
            "bridge `{bridge_name}` is missing, down, or has no IPv4 CIDR configured"
        ));
    } else if let Some(subnet) = bridge_subnet {
        if !subnet.contains(ip) {
            conflicts.push(format!(
                "VM service IP {ip} is outside bridge `{bridge_name}` subnet {}",
                subnet.cidr_string()
            ));
        } else if ip == subnet.network || ip == subnet.broadcast() {
            conflicts.push(format!(
                "VM service IP {ip} is the network or broadcast address of {}",
                subnet.cidr_string()
            ));
        } else if bridge_status.gateway_ip == Some(ip) {
            conflicts.push(format!(
                "VM service IP {ip} is the bridge `{bridge_name}` gateway address"
            ));
        } else {
            bridge_ip_ok = true;
        }
    }

    let dhcp = detect_dhcp(&bridge_name, env);

    let ip_forward_runtime = read_runtime_ip_forward(env);
    let ip_forward_ok = ip_forward_runtime == Some(1);
    if !ip_forward_ok {
        missing.push(format!(
            "net.ipv4.ip_forward is not enabled (runtime value: {})",
            ip_forward_runtime.map_or_else(|| "unknown".to_owned(), |v| v.to_string())
        ));
    }

    let uplink = detect_default_uplink();
    let (firewall, existing_egress, partial_egress) =
        detect_firewall_and_egress(&bridge_name, bridge_subnet, uplink.as_deref());
    let egress_status = classify_egress_status(
        firewall.nft_accessible,
        firewall.managed_nat_table_present,
        &existing_egress,
        &partial_egress,
    );
    let egress_ok =
        firewall.nft_accessible && matches!(egress_status, "foreign_provided" | "managed");
    if !firewall.nft_accessible {
        missing.push(
            "nftables ruleset is inaccessible (requires root or passwordless `sudo -n nft`)"
                .to_owned(),
        );
    } else if !egress_ok {
        missing.push(format!(
            "outbound NAT egress for bridge `{bridge_name}` is not configured (status={egress_status}); run `outbe-up host nat --bridge {bridge_name}` for recommended rules"
        ));
    }

    let host_network_ok = bridge_ok && ip_forward_ok && egress_ok;

    // 2. Static internal IP safety checks (Decision 10, revised):
    //    - Must be inside the bridge subnet (not network, broadcast, or gateway).
    //    - Must be outside any host DHCP pool (`pool_start..=pool_end`) when DHCP is present.
    //    - Must be unused: not assigned to another local VM, any existing reservation in
    //      `dhcp-hosts`, or any active DHCP lease for another MAC.
    let mut pool_overlap = false;
    if let (Some(pool_start), Some(pool_end)) = (dhcp.pool_start, dhcp.pool_end) {
        let ip_u32 = u32::from(ip);
        let lo = u32::from(pool_start).min(u32::from(pool_end));
        let hi = u32::from(pool_start).max(u32::from(pool_end));
        if ip_u32 >= lo && ip_u32 <= hi {
            pool_overlap = true;
            conflicts.push(format!(
                "VM service IP {ip} overlaps host DHCP pool {pool_start}..={pool_end} on bridge `{bridge_name}`; choose a service IP outside the DHCP pool"
            ));
        }
    }

    let mut local_store_conflicts = Vec::new();
    for other_name in store.list_vm_names()? {
        if other_name == vm_name {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(store.state_path(&other_name)?) {
            if let Ok(other) = serde_json::from_str::<crate::local::LocalVmState>(&raw) {
                if other.intended_ip == Some(ip) {
                    local_store_conflicts.push(format!(
                        "IP {ip} is already recorded for local VM `{other_name}`"
                    ));
                }
                if other.mac.eq_ignore_ascii_case(&live_mac) {
                    local_store_conflicts.push(format!(
                        "MAC {live_mac} is already recorded for local VM `{other_name}`"
                    ));
                }
            }
        }
    }
    let has_local_store_conflict = !local_store_conflicts.is_empty();
    conflicts.extend(local_store_conflicts);

    let mut dhcp_reserved = false;
    let mut hosts_conflict = false;
    let mut lease_conflict = false;

    if let Some(leases_file) = &dhcp.leases_file {
        if let Ok(leases_content) = fs::read_to_string(leases_file) {
            if let Err(err) =
                check_dnsmasq_lease_conflicts(&leases_content, &[(live_mac.clone(), ip)])
            {
                lease_conflict = true;
                conflicts.push(err.to_string());
            }
        }
    }

    if let Some(hosts_file) = &dhcp.hosts_file {
        match read_and_validate_hosts_file(hosts_file, env.expected_hosts_owner) {
            Ok(existing_hosts) => {
                match check_dnsmasq_reservation_status(&existing_hosts, &live_mac, ip) {
                    Ok(res_check) => {
                        if !res_check.conflicts.is_empty() {
                            hosts_conflict = true;
                            conflicts.extend(res_check.conflicts);
                        } else if res_check.exact_match
                            && !lease_conflict
                            && !has_local_store_conflict
                        {
                            dhcp_reserved = true;
                        }
                    }
                    Err(err) => {
                        hosts_conflict = true;
                        conflicts.push(err.to_string());
                    }
                }
            }
            Err(err) => {
                hosts_conflict = true;
                conflicts.push(err.to_string());
            }
        }
    }

    let service_ip_ok = bridge_ip_ok
        && !pool_overlap
        && !has_local_store_conflict
        && !hosts_conflict
        && !lease_conflict;
    let recommended_dhcp_block = None;
    let recommended_dhcp_reload = None;

    // 3. Optional published-port verification (DNAT + forward accept, fail closed on unparsed DNAT).
    let mut resolved_public_ip = public_ip_override;
    let mut recommended_forward_rules = Vec::new();
    let mut recommended_prerouting_rules = Vec::new();
    let mut recommended_firewall_commands = Vec::new();
    let mut published_ports_ok = ports.is_empty();

    if !ports.is_empty() {
        if !firewall.nft_accessible {
            published_ports_ok = false;
        } else if !firewall.unparsed_dnat_rules.is_empty() {
            let details = firewall
                .unparsed_dnat_rules
                .iter()
                .map(|u| {
                    format!(
                        "{} {}::{} (handle={:?}, comment={}): {}",
                        u.family,
                        u.table,
                        u.chain,
                        u.handle,
                        u.comment.as_deref().unwrap_or("-"),
                        u.reason
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            conflicts.push(format!(
                "live nftables ruleset contains {} unparseable DNAT rule(s); refusing to verify published ports: {details}",
                firewall.unparsed_dnat_rules.len()
            ));
            published_ports_ok = false;
        } else {
            match resolve_public_ip(public_ip_override, &firewall.dnat_rules, uplink.as_deref()) {
                Err(err) => {
                    missing.push(err.to_string());
                    published_ports_ok = false;
                }
                Ok(pub_ip) => {
                    resolved_public_ip = Some(pub_ip);
                    let (forward_table, _forward_policy) = firewall
                        .forward_chains
                        .iter()
                        .find(|c| c.table == "host_firewall" && c.chain == "forward")
                        .or_else(|| firewall.forward_chains.iter().find(|c| c.policy == "drop"))
                        .map(|c| (format!("{} {}", c.family, c.table), c.policy.clone()))
                        .unwrap_or_else(|| ("inet host_firewall".to_owned(), "unknown".to_owned()));
                    let nat_table = firewall
                        .tables
                        .iter()
                        .find(|t| t.name == "host_firewall_nat")
                        .map(|t| format!("{} {}", t.family, t.name))
                        .unwrap_or_else(|| "ip host_firewall_nat".to_owned());

                    let sources_set = format!("{{ {} }}", recommended_sources.join(", "));
                    let comment = format!("outbe-up:{vm_name}");
                    let mut all_ports_present = true;

                    for spec in &ports {
                        let mut dnat_matched = false;
                        let mut spec_has_conflict = false;

                        for rule in &firewall.dnat_rules {
                            if rule.proto != spec.proto.as_str()
                                || !rule.covers_host_port(spec.host_port)
                            {
                                continue;
                            }
                            if let Some(rule_pub) = rule.public_ip {
                                if rule_pub != pub_ip {
                                    continue;
                                }
                            }
                            if rule.host_port_end.is_none()
                                && rule.dest_ip == ip
                                && rule.guest_port == spec.guest_port
                            {
                                if nft_sources_cover_requested(&rule.sources, &explicit_sources) {
                                    dnat_matched = true;
                                }
                            } else {
                                spec_has_conflict = true;
                                conflicts.push(format!(
                                    "host port {}/{} conflicts with existing DNAT rule -> {}:{} in {} {}::{} (comment={}, handle={:?})",
                                    rule.proto,
                                    spec.host_port,
                                    rule.dest_ip,
                                    rule.guest_port,
                                    rule.family,
                                    rule.table,
                                    rule.chain,
                                    rule.comment.as_deref().unwrap_or("-"),
                                    rule.handle
                                ));
                                port_conflicts.push(PublishPortConflict {
                                    proto: rule.proto.clone(),
                                    host_port: spec.host_port,
                                    requested_dest_ip: ip,
                                    requested_guest_port: spec.guest_port,
                                    existing_table: format!("{} {}", rule.family, rule.table),
                                    existing_chain: rule.chain.clone(),
                                    existing_dest_ip: rule.dest_ip,
                                    existing_guest_port: rule.guest_port,
                                    existing_sources: rule.sources.clone(),
                                    existing_comment: rule.comment.clone(),
                                    existing_handle: rule.handle,
                                });
                            }
                        }

                        let forward_matched = firewall.forward_allows.iter().any(|rule| {
                            rule.dest_ip == ip
                                && rule.proto == spec.proto.as_str()
                                && rule.dest_port == spec.guest_port
                                && nft_sources_cover_requested(&rule.sources, &explicit_sources)
                        });

                        if !dnat_matched && !spec_has_conflict {
                            all_ports_present = false;
                            missing.push(format!(
                                "missing nftables prerouting DNAT rule for {}/{} ({pub_ip}:{} -> {}:{})",
                                spec.proto.as_str(),
                                spec.host_port,
                                spec.host_port,
                                ip,
                                spec.guest_port
                            ));
                            let dnat_target = if spec.host_port == spec.guest_port {
                                ip.to_string()
                            } else {
                                format!("{ip}:{}", spec.guest_port)
                            };
                            recommended_prerouting_rules.push(format!(
                                "ip daddr {pub_ip} ip saddr {sources_set} {} dport {} dnat to {dnat_target} comment \"{comment}\";",
                                spec.proto.as_str(),
                                spec.host_port
                            ));
                        }

                        if !forward_matched {
                            all_ports_present = false;
                            missing.push(format!(
                                "missing nftables forward accept rule for {}/{} -> {}:{}",
                                spec.proto.as_str(),
                                spec.guest_port,
                                ip,
                                spec.guest_port
                            ));
                            recommended_forward_rules.push(format!(
                                "ip saddr {sources_set} ip daddr {ip} ct status dnat {} dport {} accept comment \"{comment}\";",
                                spec.proto.as_str(),
                                spec.guest_port
                            ));
                        }
                    }

                    if !recommended_forward_rules.is_empty()
                        || !recommended_prerouting_rules.is_empty()
                    {
                        recommended_firewall_commands = vec![
                            format!(
                                "Add the forward rule(s) into `table {forward_table} {{ chain forward {{ ... }} }}` and the prerouting rule(s) into `table {nat_table} {{ chain prerouting {{ ... }} }}` inside `{HOST_FIREWALL_NFT_PATH}`"
                            ),
                            format!("sudo nft --check --file {HOST_FIREWALL_NFT_PATH}"),
                            format!("sudo systemctl reload {HOST_FIREWALL_SERVICE}"),
                        ];
                    }

                    published_ports_ok = all_ports_present && port_conflicts.is_empty();
                }
            }
        }
    }

    let ready = host_network_ok
        && service_ip_ok
        && published_ports_ok
        && missing.is_empty()
        && conflicts.is_empty();

    Ok(HostCheckReport {
        vm: vm_name.to_owned(),
        vm_id: state.vm_id,
        mac: live_mac,
        ip,
        bridge: bridge_name,
        public_ip: resolved_public_ip,
        ports,
        ready,
        host_network_ok,
        service_ip_ok,
        dhcp_reserved,
        published_ports_ok,
        missing,
        conflicts,
        port_conflicts,
        hosts_file: dhcp.hosts_file,
        dhcp_unit: dhcp.unit_name,
        recommended_dhcp_block,
        recommended_dhcp_reload,
        recommended_forward_rules,
        recommended_prerouting_rules,
        recommended_firewall_commands,
        writes_performed: 0,
    })
}

fn nft_sources_cover_requested(rule_sources: &[String], requested: &[String]) -> bool {
    if requested.is_empty() || rule_sources.is_empty() {
        return true;
    }
    if rule_sources.iter().any(|s| s == "0.0.0.0/0") {
        return true;
    }
    requested
        .iter()
        .all(|req| rule_sources.iter().any(|s| s == req))
}

/// Compute and render `/etc/host-firewall.nft` DNAT + forward-accept rules for one local VM,
/// detecting conflicts against the live nftables ruleset. Strictly read-only (never writes).
#[allow(clippy::too_many_arguments)]
pub fn plan_host_publish<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    vm_name: &str,
    raw_ports: &[String],
    raw_sources: &[String],
    public_ip_override: Option<Ipv4Addr>,
    bridge_override: Option<&str>,
    env: &HostEnv,
) -> Result<HostPublishReport> {
    validate_vm_name(vm_name)?;
    ensure!(
        !raw_ports.is_empty(),
        "at least one `--port <guest>[:<host>][/(tcp|udp)]` specification is required"
    );

    let mut ports = Vec::with_capacity(raw_ports.len());
    let mut seen_host_ports = BTreeSet::new();
    for raw in raw_ports {
        let spec = parse_port_spec(raw)?;
        ensure!(
            seen_host_ports.insert((spec.proto.as_str(), spec.host_port)),
            "duplicate host port {}/{} in --port arguments",
            spec.proto.as_str(),
            spec.host_port
        );
        ports.push(spec);
    }

    let sources = if raw_sources.is_empty() {
        vec!["0.0.0.0/0".to_owned()]
    } else {
        let mut normalized = Vec::with_capacity(raw_sources.len());
        for raw in raw_sources {
            normalized.push(normalize_ipv4_cidr(raw)?);
        }
        normalized
    };

    let state_path = store.state_path(vm_name)?;
    let raw_state = fs::read_to_string(&state_path).wrap_err_with(|| {
        format!(
            "local VM `{vm_name}` is not recorded in {} (create it first with `outbe-up vm create`)",
            state_path.display()
        )
    })?;
    let state: crate::local::LocalVmState = serde_json::from_str(&raw_state)
        .wrap_err_with(|| format!("parse local VM state {}", state_path.display()))?;
    ensure!(
        !state.vm_id.is_empty() && state.phase != LocalVmPhase::Removing,
        "VM `{vm_name}` is in phase `{:?}` and cannot be published",
        state.phase
    );

    let info_resp = vmm.get_info(&state.vm_id)?;
    ensure!(
        !info_resp.is_gone(),
        "VM `{vm_name}` ({}) is missing from local VMM",
        state.vm_id
    );
    let info = info_resp.info.unwrap();
    ensure!(
        info.name == state.name,
        "VMM VM {} has unexpected name `{}` (expected `{}`)",
        state.vm_id,
        info.name,
        state.name
    );
    let live_mac = extract_primary_mac(&info).unwrap_or_else(|_| state.mac.clone());

    let bridge_name = bridge_override.unwrap_or(&state.bridge).to_owned();
    validate_iface_name(&bridge_name, "bridge")?;
    let (_, bridge_subnet) = detect_bridge(&bridge_name);
    let dhcp = detect_dhcp(&bridge_name, env);

    let guest_ip = if let Some(ip) = state.intended_ip {
        ip
    } else if let Some(leases_file) = &dhcp.leases_file {
        let content = fs::read_to_string(leases_file).unwrap_or_default();
        lookup_lease_ip_by_mac(&content, &live_mac).ok_or_else(|| {
            eyre::eyre!(
                "VM `{vm_name}` has no recorded intended_ip and no active DHCP lease for MAC `{live_mac}`; run `outbe-up host reserve --vm {vm_name} --ip <ipv4>` first"
            )
        })?
    } else {
        bail!(
            "VM `{vm_name}` has no recorded intended_ip and no DHCP lease file was detected; run `outbe-up host reserve --vm {vm_name} --ip <ipv4>` first"
        );
    };

    let uplink = detect_default_uplink();
    let (firewall, _, _) =
        detect_firewall_and_egress(&bridge_name, bridge_subnet, uplink.as_deref());
    ensure!(
        firewall.nft_accessible,
        "cannot inspect live firewall rules: `nft -j list ruleset` is inaccessible (requires root or passwordless `sudo -n nft`)"
    );
    if !firewall.unparsed_dnat_rules.is_empty() {
        let details = firewall
            .unparsed_dnat_rules
            .iter()
            .map(|u| {
                format!(
                    "{} {}::{} (handle={:?}, comment={}): {}",
                    u.family,
                    u.table,
                    u.chain,
                    u.handle,
                    u.comment.as_deref().unwrap_or("-"),
                    u.reason
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        bail!(
            "refusing to plan port publish: live nftables ruleset contains {} unparseable DNAT rule(s) so port conflicts cannot be verified safely: {details}",
            firewall.unparsed_dnat_rules.len()
        );
    }

    let public_ip = resolve_public_ip(public_ip_override, &firewall.dnat_rules, uplink.as_deref())?;

    let mut conflicts = Vec::new();
    let mut already_published_ports = Vec::new();
    for spec in &ports {
        for rule in &firewall.dnat_rules {
            if rule.proto != spec.proto.as_str() || !rule.covers_host_port(spec.host_port) {
                continue;
            }
            if let Some(rule_pub) = rule.public_ip {
                if rule_pub != public_ip {
                    continue;
                }
            }
            if rule.host_port_end.is_none()
                && rule.dest_ip == guest_ip
                && rule.guest_port == spec.guest_port
            {
                already_published_ports.push(*spec);
            } else {
                conflicts.push(PublishPortConflict {
                    proto: rule.proto.clone(),
                    host_port: spec.host_port,
                    requested_dest_ip: guest_ip,
                    requested_guest_port: spec.guest_port,
                    existing_table: format!("{} {}", rule.family, rule.table),
                    existing_chain: rule.chain.clone(),
                    existing_dest_ip: rule.dest_ip,
                    existing_guest_port: rule.guest_port,
                    existing_sources: rule.sources.clone(),
                    existing_comment: rule.comment.clone(),
                    existing_handle: rule.handle,
                });
            }
        }
    }

    let (forward_table, forward_policy) = firewall
        .forward_chains
        .iter()
        .find(|c| c.table == "host_firewall" && c.chain == "forward")
        .or_else(|| firewall.forward_chains.iter().find(|c| c.policy == "drop"))
        .map(|c| (format!("{} {}", c.family, c.table), c.policy.clone()))
        .unwrap_or_else(|| ("inet host_firewall".to_owned(), "unknown".to_owned()));

    let nat_table = firewall
        .tables
        .iter()
        .find(|t| t.name == "host_firewall_nat")
        .map(|t| format!("{} {}", t.family, t.name))
        .unwrap_or_else(|| "ip host_firewall_nat".to_owned());

    let sources_set = format!("{{ {} }}", sources.join(", "));
    let comment = format!("outbe-up:{vm_name}");
    let mut forward_rules = Vec::with_capacity(ports.len());
    let mut prerouting_rules = Vec::with_capacity(ports.len());

    for spec in &ports {
        forward_rules.push(format!(
            "ip saddr {sources_set} ip daddr {guest_ip} ct status dnat {} dport {} accept comment \"{comment}\";",
            spec.proto.as_str(),
            spec.guest_port
        ));
        let dnat_target = if spec.host_port == spec.guest_port {
            guest_ip.to_string()
        } else {
            format!("{guest_ip}:{}", spec.guest_port)
        };
        prerouting_rules.push(format!(
            "ip daddr {public_ip} ip saddr {sources_set} {} dport {} dnat to {dnat_target} comment \"{comment}\";",
            spec.proto.as_str(),
            spec.host_port
        ));
    }

    let apply_commands = vec![
        format!(
            "Add the forward rule(s) into `table {forward_table} {{ chain forward {{ ... }} }}` and the prerouting rule(s) into `table {nat_table} {{ chain prerouting {{ ... }} }}` inside `{HOST_FIREWALL_NFT_PATH}`"
        ),
        format!("sudo nft --check --file {HOST_FIREWALL_NFT_PATH}"),
        format!("sudo systemctl reload {HOST_FIREWALL_SERVICE}"),
    ];
    let remove_commands = vec![
        format!("Remove the lines tagged `comment \"{comment}\"` from `{HOST_FIREWALL_NFT_PATH}`"),
        format!("sudo nft --check --file {HOST_FIREWALL_NFT_PATH}"),
        format!("sudo systemctl reload {HOST_FIREWALL_SERVICE}"),
    ];

    Ok(HostPublishReport {
        vm: vm_name.to_owned(),
        vm_id: state.vm_id,
        guest_ip,
        public_ip,
        sources,
        ports,
        firewall_file: PathBuf::from(HOST_FIREWALL_NFT_PATH),
        firewall_service: HOST_FIREWALL_SERVICE.to_owned(),
        forward_table,
        forward_policy,
        nat_table,
        forward_rules,
        prerouting_rules,
        apply_commands,
        remove_commands,
        conflicts,
        already_published_ports,
        writes_performed: 0,
    })
}

/// Parse `<guest>[:<host>][/(tcp|udp)]` (defaults to `host == guest` and `proto == tcp`).
pub fn parse_port_spec(raw: &str) -> Result<PublishPortSpec> {
    let trimmed = raw.trim();
    ensure!(!trimmed.is_empty(), "port specification must not be empty");
    let (ports_part, proto) = match trimmed.split_once('/') {
        Some((p, proto_str)) => {
            let proto = match proto_str.to_ascii_lowercase().as_str() {
                "tcp" => TransportProto::Tcp,
                "udp" => TransportProto::Udp,
                other => {
                    bail!("unsupported protocol '{other}' in --port '{raw}' (expected tcp or udp)");
                }
            };
            (p, proto)
        }
        None => (trimmed, TransportProto::Tcp),
    };
    let (guest_port, host_port) = match ports_part.split_once(':') {
        Some((g, h)) => {
            let guest: u16 = g
                .parse()
                .wrap_err_with(|| format!("invalid guest port in --port '{raw}'"))?;
            let host: u16 = h
                .parse()
                .wrap_err_with(|| format!("invalid host port in --port '{raw}'"))?;
            (guest, host)
        }
        None => {
            let port: u16 = ports_part
                .parse()
                .wrap_err_with(|| format!("invalid port in --port '{raw}'"))?;
            (port, port)
        }
    };
    ensure!(
        guest_port > 0 && host_port > 0,
        "ports must be between 1 and 65535 in --port '{raw}'"
    );
    Ok(PublishPortSpec {
        guest_port,
        host_port,
        proto,
    })
}

/// Normalize an IPv4 address or CIDR string (`1.2.3.4` -> `1.2.3.4/32`, `10.77.0.0/24` -> `10.77.0.0/24`).
pub fn normalize_ipv4_cidr(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if let Some((ip_str, prefix_str)) = trimmed.split_once('/') {
        let addr: Ipv4Addr = ip_str
            .parse()
            .wrap_err_with(|| format!("invalid IPv4 address in --source '{raw}'"))?;
        let prefix: u8 = prefix_str
            .parse()
            .wrap_err_with(|| format!("invalid CIDR prefix in --source '{raw}'"))?;
        ensure!(
            prefix <= 32,
            "CIDR prefix must be 0..=32 in --source '{raw}'"
        );
        let mask = if prefix == 0 {
            0u32
        } else {
            !0u32 << (32 - prefix)
        };
        let network = Ipv4Addr::from(u32::from(addr) & mask);
        Ok(format!("{network}/{prefix}"))
    } else {
        let addr: Ipv4Addr = trimmed
            .parse()
            .wrap_err_with(|| format!("invalid IPv4 address/CIDR in --source '{raw}'"))?;
        Ok(format!("{addr}/32"))
    }
}

fn read_and_validate_hosts_file(hosts_file: &Path, expected_owner: (u32, u32)) -> Result<String> {
    let meta = fs::symlink_metadata(hosts_file).wrap_err_with(|| {
        format!(
            "discovered DHCP hosts file does not exist: {}",
            hosts_file.display()
        )
    })?;
    ensure!(
        !meta.file_type().is_symlink() && meta.file_type().is_file(),
        "refusing DHCP hosts file {}: must be a regular file (not a symlink)",
        hosts_file.display()
    );
    let actual_owner = (meta.uid(), meta.gid());
    ensure!(
        actual_owner == expected_owner,
        "refusing DHCP hosts file {}: expected owner {}:{}, found {}:{}",
        hosts_file.display(),
        expected_owner.0,
        expected_owner.1,
        actual_owner.0,
        actual_owner.1
    );
    let content = fs::read_to_string(hosts_file)
        .wrap_err_with(|| format!("read DHCP hosts file {}", hosts_file.display()))?;
    validate_dnsmasq_hosts_format(&content).wrap_err_with(|| {
        format!(
            "refusing DHCP hosts file {}: unrecognized file format",
            hosts_file.display()
        )
    })?;
    Ok(content)
}

fn run_capture(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

fn run_privileged_capture(program: &str, args: &[&str]) -> Option<String> {
    let out = if rustix::process::geteuid().as_raw() == 0 {
        Command::new(program).args(args).output().ok()?
    } else {
        Command::new("sudo")
            .arg("-n")
            .arg(program)
            .args(args)
            .output()
            .ok()?
    };
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

#[derive(Default)]
struct SystemdUnitProps {
    load_state: String,
    active_state: String,
    unit_file_state: String,
    main_pid: Option<u32>,
    exec_start: String,
    exec_reload: String,
}

impl SystemdUnitProps {
    fn is_loaded(&self) -> bool {
        self.load_state == "loaded"
    }

    fn is_enabled(&self) -> bool {
        matches!(
            self.unit_file_state.as_str(),
            "enabled" | "enabled-runtime" | "static"
        )
    }

    fn is_active(&self) -> bool {
        self.active_state == "active"
    }
}

fn query_systemd_unit(unit: &str) -> SystemdUnitProps {
    let Some(raw) = run_capture(
        "systemctl",
        &[
            "show",
            "-p",
            "LoadState,ActiveState,UnitFileState,MainPID,ExecStart,ExecReload",
            unit,
        ],
    ) else {
        return SystemdUnitProps::default();
    };
    parse_systemd_show_props(&raw)
}

fn parse_systemd_show_props(raw: &str) -> SystemdUnitProps {
    let mut props = SystemdUnitProps::default();
    for line in raw.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let val = v.trim();
        match k.trim() {
            "LoadState" => props.load_state = val.to_owned(),
            "ActiveState" => props.active_state = val.to_owned(),
            "UnitFileState" => props.unit_file_state = val.to_owned(),
            "MainPID" => {
                props.main_pid = val.parse::<u32>().ok().filter(|&pid| pid > 0);
            }
            "ExecStart" => props.exec_start = val.to_owned(),
            "ExecReload" => props.exec_reload = val.to_owned(),
            _ => {}
        }
    }
    props
}

/// Extract command tokens from a systemd `ExecStart=` value (handling both structured
/// `{ path=... ; argv[]=... ; ... }` output from `systemctl show` and raw command lines).
pub(crate) fn extract_systemd_exec_argv(exec_start: &str) -> Vec<String> {
    let trimmed = exec_start.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let cmd_str = if let Some(pos) = trimmed.find("argv[]=") {
        let rest = &trimmed[pos + "argv[]=".len()..];
        rest.split(" ; ").next().unwrap_or(rest).trim()
    } else {
        trimmed
    };
    cmd_str.split_whitespace().map(str::to_owned).collect()
}

fn detect_bridge(bridge_name: &str) -> (BridgeStatus, Option<Ipv4Subnet>) {
    let unit = query_systemd_unit(VM_BRIDGE_SERVICE);
    let raw_ip = run_capture("ip", &["-j", "addr", "show", "dev", bridge_name]);
    let parsed = raw_ip.as_deref().and_then(parse_ip_json_bridge_addr);

    let Some((up, gateway_ip, subnet)) = parsed else {
        return (
            BridgeStatus {
                name: bridge_name.to_owned(),
                present: false,
                up: false,
                cidr: None,
                gateway_ip: None,
                management: "absent".to_owned(),
                unit_name: unit.is_loaded().then(|| VM_BRIDGE_SERVICE.to_owned()),
                unit_enabled: unit.is_enabled(),
                unit_active: unit.is_active(),
            },
            None,
        );
    };

    let management = if unit.is_loaded()
        && (unit.is_active() || unit.exec_start.contains(bridge_name))
    {
        format!("systemd:{VM_BRIDGE_SERVICE}")
    } else if let Some(nctl) = run_capture("networkctl", &["status", bridge_name, "--no-pager"]) {
        if nctl
            .lines()
            .any(|l| l.contains("Network File:") && !l.contains("n/a"))
        {
            "systemd-networkd".to_owned()
        } else {
            "manual".to_owned()
        }
    } else {
        "manual".to_owned()
    };

    (
        BridgeStatus {
            name: bridge_name.to_owned(),
            present: true,
            up,
            cidr: subnet.map(Ipv4Subnet::cidr_string),
            gateway_ip,
            management,
            unit_name: unit.is_loaded().then(|| VM_BRIDGE_SERVICE.to_owned()),
            unit_enabled: unit.is_enabled(),
            unit_active: unit.is_active(),
        },
        subnet,
    )
}

pub(crate) fn parse_ip_json_bridge_addr(
    json_str: &str,
) -> Option<(bool, Option<Ipv4Addr>, Option<Ipv4Subnet>)> {
    let val: Value = serde_json::from_str(json_str).ok()?;
    let iface = val.as_array()?.first()?;
    let operstate = iface.get("operstate").and_then(Value::as_str).unwrap_or("");
    let flags_up = iface
        .get("flags")
        .and_then(Value::as_array)
        .is_some_and(|arr| arr.iter().any(|f| f.as_str() == Some("UP")));
    let up = operstate == "UP" || flags_up;

    let mut gateway_ip = None;
    let mut subnet = None;
    if let Some(addrs) = iface.get("addr_info").and_then(Value::as_array) {
        for addr in addrs {
            if addr.get("family").and_then(Value::as_str) != Some("inet") {
                continue;
            }
            if let (Some(local_str), Some(prefix)) = (
                addr.get("local").and_then(Value::as_str),
                addr.get("prefixlen").and_then(Value::as_u64),
            ) {
                if let (Ok(ip), Ok(prefix_u8)) =
                    (local_str.parse::<Ipv4Addr>(), u8::try_from(prefix))
                {
                    gateway_ip = Some(ip);
                    subnet = Ipv4Subnet::from_addr_prefix(ip, prefix_u8).ok();
                    break;
                }
            }
        }
    }
    Some((up, gateway_ip, subnet))
}

#[derive(Default)]
pub(crate) struct ParsedDnsmasqArgs {
    pub(crate) interface: Option<String>,
    pub(crate) hosts_file: Option<PathBuf>,
    pub(crate) leases_file: Option<PathBuf>,
    pub(crate) pid_file: Option<PathBuf>,
    pub(crate) pool_start: Option<Ipv4Addr>,
    pub(crate) pool_end: Option<Ipv4Addr>,
    pub(crate) netmask: Option<Ipv4Addr>,
    pub(crate) lease_duration: Option<String>,
}

pub(crate) fn parse_dnsmasq_argv(argv: &[String]) -> ParsedDnsmasqArgs {
    let mut out = ParsedDnsmasqArgs::default();
    let mut i = 0usize;
    while i < argv.len() {
        let arg = argv[i].as_str();
        let next = argv.get(i + 1).map(String::as_str);
        if let Some(v) = arg.strip_prefix("--interface=") {
            out.interface = Some(v.to_owned());
        } else if arg == "--interface" || arg == "-i" {
            if let Some(v) = next {
                out.interface = Some(v.to_owned());
                i += 1;
            }
        } else if let Some(v) = arg.strip_prefix("--dhcp-hostsfile=") {
            out.hosts_file = Some(PathBuf::from(v));
        } else if arg == "--dhcp-hostsfile" {
            if let Some(v) = next {
                out.hosts_file = Some(PathBuf::from(v));
                i += 1;
            }
        } else if let Some(v) = arg.strip_prefix("--dhcp-leasefile=") {
            out.leases_file = Some(PathBuf::from(v));
        } else if arg == "--dhcp-leasefile" {
            if let Some(v) = next {
                out.leases_file = Some(PathBuf::from(v));
                i += 1;
            }
        } else if let Some(v) = arg.strip_prefix("--pid-file=") {
            out.pid_file = Some(PathBuf::from(v));
        } else if arg == "--pid-file" || arg == "-x" {
            if let Some(v) = next {
                out.pid_file = Some(PathBuf::from(v));
                i += 1;
            }
        } else if let Some(v) = arg.strip_prefix("--dhcp-range=") {
            parse_dhcp_range(v, &mut out);
        } else if arg == "--dhcp-range" || arg == "-F" {
            if let Some(v) = next {
                parse_dhcp_range(v, &mut out);
                i += 1;
            }
        }
        i += 1;
    }
    out
}

fn parse_dhcp_range(raw: &str, out: &mut ParsedDnsmasqArgs) {
    let parts: Vec<&str> = raw.split(',').map(str::trim).collect();
    let mut ips = Vec::new();
    let mut tail = None;
    for part in &parts {
        if let Ok(ip) = part.parse::<Ipv4Addr>() {
            ips.push(ip);
        } else if !part.is_empty() && !part.starts_with("set:") && !part.starts_with("tag:") {
            tail = Some((*part).to_owned());
        }
    }
    if ips.len() >= 2 {
        out.pool_start = Some(ips[0]);
        out.pool_end = Some(ips[1]);
        if ips.len() >= 3 {
            out.netmask = Some(ips[2]);
        }
    }
    if let Some(lease) = tail {
        out.lease_duration = Some(lease);
    }
}

fn detect_dhcp(bridge_name: &str, env: &HostEnv) -> DhcpStatus {
    let unit = query_systemd_unit(VALIDATOR_DHCP_SERVICE);
    if unit.is_loaded() {
        let argv = extract_systemd_exec_argv(&unit.exec_start);
        let parsed = parse_dnsmasq_argv(&argv);
        let matches_bridge = parsed
            .interface
            .as_deref()
            .is_none_or(|iface| iface == bridge_name);
        if matches_bridge && (!argv.is_empty() || unit.is_active()) {
            let reservations_count = parsed
                .hosts_file
                .as_deref()
                .and_then(|p| fs::read_to_string(p).ok())
                .map(|c| {
                    validate_dnsmasq_hosts_format(&c).unwrap_or_else(|_| {
                        c.lines()
                            .filter(|l| {
                                let t = l.split('#').next().unwrap_or("").trim();
                                !t.is_empty()
                            })
                            .count()
                    })
                })
                .unwrap_or(0);
            let active_leases_count = parsed
                .leases_file
                .as_deref()
                .and_then(|p| fs::read_to_string(p).ok())
                .map(|c| count_dnsmasq_leases(&c))
                .unwrap_or(0);
            let reload_mode = if !unit.exec_reload.trim().is_empty() {
                "systemctl-reload".to_owned()
            } else {
                "unsupported".to_owned()
            };
            return DhcpStatus {
                detected: true,
                unit_name: Some(VALIDATOR_DHCP_SERVICE.to_owned()),
                unit_enabled: unit.is_enabled(),
                unit_active: unit.is_active(),
                pid: unit.main_pid,
                hosts_file: parsed.hosts_file,
                leases_file: parsed.leases_file,
                pid_file: parsed.pid_file,
                pool_start: parsed.pool_start,
                pool_end: parsed.pool_end,
                netmask: parsed.netmask,
                lease_duration: parsed.lease_duration,
                reload_mode,
                reservations_count,
                active_leases_count,
            };
        }
    }

    // Fallback: scan `<sys_root>/proc/<pid>/cmdline` for a running dnsmasq bound to `bridge_name`.
    if let Some((pid, parsed)) = scan_proc_for_dnsmasq(bridge_name, env) {
        let reservations_count = parsed
            .hosts_file
            .as_deref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|c| validate_dnsmasq_hosts_format(&c).ok())
            .unwrap_or(0);
        let active_leases_count = parsed
            .leases_file
            .as_deref()
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|c| count_dnsmasq_leases(&c))
            .unwrap_or(0);
        return DhcpStatus {
            detected: true,
            unit_name: None,
            unit_enabled: false,
            unit_active: true,
            pid: Some(pid),
            hosts_file: parsed.hosts_file,
            leases_file: parsed.leases_file,
            pid_file: parsed.pid_file,
            pool_start: parsed.pool_start,
            pool_end: parsed.pool_end,
            netmask: parsed.netmask,
            lease_duration: parsed.lease_duration,
            reload_mode: "unsupported".to_owned(),
            reservations_count,
            active_leases_count,
        };
    }

    DhcpStatus {
        detected: false,
        unit_name: None,
        unit_enabled: false,
        unit_active: false,
        pid: None,
        hosts_file: None,
        leases_file: None,
        pid_file: None,
        pool_start: None,
        pool_end: None,
        netmask: None,
        lease_duration: None,
        reload_mode: "none".to_owned(),
        reservations_count: 0,
        active_leases_count: 0,
    }
}

fn scan_proc_for_dnsmasq(bridge_name: &str, env: &HostEnv) -> Option<(u32, ParsedDnsmasqArgs)> {
    let proc_dir = env.path("/proc");
    let entries = fs::read_dir(proc_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(raw) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let argv: Vec<String> = raw
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        let is_dnsmasq = argv.first().is_some_and(|exe| {
            Path::new(exe).file_name().and_then(|n| n.to_str()) == Some("dnsmasq")
        });
        if !is_dnsmasq {
            continue;
        }
        let parsed = parse_dnsmasq_argv(&argv);
        if parsed.interface.as_deref() == Some(bridge_name) {
            return Some((pid, parsed));
        }
    }
    None
}

fn detect_default_uplink() -> Option<String> {
    let raw = run_capture("ip", &["-4", "-o", "route", "show", "default"])?;
    for line in raw.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if let Some(pos) = tokens.iter().position(|&t| t == "dev") {
            if let Some(&iface) = tokens.get(pos + 1) {
                if validate_iface_name(iface, "uplink").is_ok() {
                    return Some(iface.to_owned());
                }
            }
        }
    }
    None
}

fn resolve_public_ip(
    explicit: Option<Ipv4Addr>,
    dnat_rules: &[FirewallDnatRule],
    uplink: Option<&str>,
) -> Result<Ipv4Addr> {
    if let Some(ip) = explicit {
        return Ok(ip);
    }
    let mut existing_public_ips = BTreeSet::new();
    for rule in dnat_rules {
        if let Some(ip) = rule.public_ip {
            existing_public_ips.insert(ip);
        }
    }
    if existing_public_ips.len() == 1 {
        return Ok(*existing_public_ips.iter().next().unwrap());
    }
    if let Some(iface) = uplink {
        if let Some(raw) = run_capture("ip", &["-j", "-4", "addr", "show", "dev", iface]) {
            if let Some((_, Some(ip), _)) = parse_ip_json_bridge_addr(&raw) {
                return Ok(ip);
            }
        }
    }
    bail!("could not determine host public IPv4 address automatically; pass `--public-ip <ipv4>`");
}

fn read_runtime_ip_forward(env: &HostEnv) -> Option<u8> {
    let raw = fs::read_to_string(env.path("/proc/sys/net/ipv4/ip_forward")).ok()?;
    let val = raw.trim().parse::<u8>().ok()?;
    (val == 0 || val == 1).then_some(val)
}

fn scan_persistent_ip_forward(paths: &HostNatPaths) -> PersistentSysctlState {
    let mut lines = Vec::new();
    for (dir_idx, dir) in paths.sysctl_scan_dirs.iter().enumerate() {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut conf_paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("conf"))
            .collect();
        conf_paths.sort();
        for path in conf_paths {
            lines.push(format!("FILE {dir_idx} {}", path.display()));
            if let Ok(content) = fs::read_to_string(&path) {
                for line in content.lines() {
                    if is_ip_forward_sysctl_line(line) {
                        lines.push(format!("SET {}:{}", path.display(), line.trim()));
                    }
                }
            }
        }
    }
    if paths.sysctl_conf_path.is_file() {
        lines.push(format!("CONF {}", paths.sysctl_conf_path.display()));
        if let Ok(content) = fs::read_to_string(&paths.sysctl_conf_path) {
            for line in content.lines() {
                if is_ip_forward_sysctl_line(line) {
                    lines.push(format!(
                        "SET {}:{}",
                        paths.sysctl_conf_path.display(),
                        line.trim()
                    ));
                }
            }
        }
    }
    parse_persistent_sysctl(&lines.join("\n"), "90-outbe-up-nat.conf")
}

fn is_ip_forward_sysctl_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
        return false;
    }
    let Some((lhs, _)) = trimmed.split_once('=') else {
        return false;
    };
    let key = lhs.trim();
    key == "net.ipv4.ip_forward" || key == "net/ipv4/ip_forward"
}

fn probe_managed_nat_files(paths: &HostNatPaths) -> ManagedFileStates {
    ManagedFileStates {
        rules: probe_single_owned_file(&paths.rules_path),
        service: probe_single_owned_file(&paths.service_path),
        sysctl: probe_single_owned_file(&paths.sysctl_path),
    }
}

fn probe_single_owned_file(path: &Path) -> FileOwnership {
    match fs::read_to_string(path) {
        Ok(content) if content.contains(OWNERSHIP_MARKER) => FileOwnership::Owned,
        Ok(_) => FileOwnership::Foreign,
        Err(_) if fs::symlink_metadata(path).is_ok() => FileOwnership::Foreign,
        Err(_) => FileOwnership::Absent,
    }
}

fn classify_egress_status(
    nft_accessible: bool,
    managed_nat_table_present: bool,
    existing_egress: &[ExistingEgress],
    partial_egress: &[PartialEgress],
) -> &'static str {
    if !nft_accessible {
        "unknown"
    } else if !existing_egress.is_empty() {
        "foreign_provided"
    } else if managed_nat_table_present {
        "managed"
    } else if !partial_egress.is_empty() {
        "partial"
    } else {
        "missing"
    }
}

fn detect_firewall_and_egress(
    bridge_name: &str,
    bridge_subnet: Option<Ipv4Subnet>,
    uplink: Option<&str>,
) -> (FirewallStatus, Vec<ExistingEgress>, Vec<PartialEgress>) {
    let Some(json_str) = run_privileged_capture("nft", &["-j", "list", "ruleset"]) else {
        return (
            FirewallStatus {
                nft_accessible: false,
                tables: Vec::new(),
                forward_chains: Vec::new(),
                forward_allows: Vec::new(),
                dnat_rules: Vec::new(),
                unparsed_dnat_rules: Vec::new(),
                managed_nat_table_present: false,
            },
            Vec::new(),
            Vec::new(),
        );
    };

    match parse_firewall_and_egress_json(&json_str, bridge_name, bridge_subnet, uplink) {
        Ok(res) => res,
        Err(_) => (
            FirewallStatus {
                nft_accessible: false,
                tables: Vec::new(),
                forward_chains: Vec::new(),
                forward_allows: Vec::new(),
                dnat_rules: Vec::new(),
                unparsed_dnat_rules: Vec::new(),
                managed_nat_table_present: false,
            },
            Vec::new(),
            Vec::new(),
        ),
    }
}

/// Maximum inclusive port range size expanded into individual [`FirewallDnatRule`] /
/// [`FirewallForwardAllow`] entries; larger ranges are kept compact via `host_port_end`.
const MAX_EXPANDED_PORT_RANGE: u16 = 1024;

/// Parse `nft -j list ruleset` JSON into [`FirewallStatus`] plus egress inspection results.
pub fn parse_firewall_and_egress_json(
    json_str: &str,
    bridge_name: &str,
    bridge_subnet: Option<Ipv4Subnet>,
    uplink: Option<&str>,
) -> Result<(FirewallStatus, Vec<ExistingEgress>, Vec<PartialEgress>)> {
    let root: Value = serde_json::from_str(json_str.trim())
        .wrap_err("parse `nft -j list ruleset` JSON output")?;
    let items = root
        .get("nftables")
        .and_then(Value::as_array)
        .ok_or_else(|| eyre::eyre!("`nft -j list ruleset` JSON missing `nftables` array"))?;

    let mut tables = Vec::new();
    let mut seen_tables = BTreeSet::new();
    let mut managed_nat_table_present = false;
    let mut chain_hooks: BTreeMap<(String, String, String), String> = BTreeMap::new();

    for item in items {
        if let Some(tbl) = item.get("table").and_then(Value::as_object) {
            if let (Some(family), Some(name)) = (
                tbl.get("family").and_then(Value::as_str),
                tbl.get("name").and_then(Value::as_str),
            ) {
                if family == NAT_TABLE_FAMILY && name == NAT_TABLE_NAME {
                    managed_nat_table_present = true;
                }
                if seen_tables.insert((family.to_owned(), name.to_owned())) {
                    tables.push(FirewallTableRef {
                        family: family.to_owned(),
                        name: name.to_owned(),
                    });
                }
            }
        } else if let Some(chain) = item.get("chain").and_then(Value::as_object) {
            if let (Some(family), Some(table), Some(name), Some(hook)) = (
                chain.get("family").and_then(Value::as_str),
                chain.get("table").and_then(Value::as_str),
                chain.get("name").and_then(Value::as_str),
                chain.get("hook").and_then(Value::as_str),
            ) {
                chain_hooks.insert(
                    (family.to_owned(), table.to_owned(), name.to_owned()),
                    hook.to_owned(),
                );
            }
        }
    }

    let mut forward_allows = Vec::new();
    let mut dnat_rules = Vec::new();
    let mut unparsed_dnat_rules = Vec::new();

    for item in items {
        let Some(rule) = item.get("rule").and_then(Value::as_object) else {
            continue;
        };
        let (Some(family), Some(table), Some(chain)) = (
            rule.get("family").and_then(Value::as_str),
            rule.get("table").and_then(Value::as_str),
            rule.get("chain").and_then(Value::as_str),
        ) else {
            continue;
        };
        let Some(exprs) = rule.get("expr").and_then(Value::as_array) else {
            continue;
        };
        let comment = rule
            .get("comment")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let handle = rule.get("handle").and_then(Value::as_u64);
        let hook = chain_hooks
            .get(&(family.to_owned(), table.to_owned(), chain.to_owned()))
            .map(String::as_str);

        if hook == Some("forward") {
            if let Some(mut parsed) =
                parse_forward_allow_rule(family, table, chain, exprs, comment.as_deref())
            {
                forward_allows.append(&mut parsed);
            }
        }
        if exprs.iter().any(value_contains_dnat) {
            match parse_dnat_rule(family, table, chain, exprs, comment.as_deref(), handle) {
                Ok(mut parsed) => dnat_rules.append(&mut parsed),
                Err(reason) => unparsed_dnat_rules.push(UnparsedDnatRule {
                    family: family.to_owned(),
                    table: table.to_owned(),
                    chain: chain.to_owned(),
                    handle,
                    comment,
                    reason,
                }),
            }
        }
    }

    // Reuse `nat::inspect_nft_json` for exact forward chain classification & egress detection.
    let probe_subnet = bridge_subnet.unwrap_or(Ipv4Subnet {
        network: Ipv4Addr::new(10, 77, 0, 0),
        prefix_len: 24,
    });
    let probe_uplink = uplink.unwrap_or("eth0");
    let nat_insp = inspect_nft_json(json_str, bridge_name, probe_subnet, probe_uplink)?;

    let (existing_egress, partial_egress) = if bridge_subnet.is_some() && uplink.is_some() {
        (nat_insp.existing_egress, nat_insp.partial_egress)
    } else {
        (Vec::new(), Vec::new())
    };

    Ok((
        FirewallStatus {
            nft_accessible: true,
            tables,
            forward_chains: nat_insp.foreign_forward_chains,
            forward_allows,
            dnat_rules,
            unparsed_dnat_rules,
            managed_nat_table_present,
        },
        existing_egress,
        partial_egress,
    ))
}

fn value_contains_dnat(val: &Value) -> bool {
    match val {
        Value::Object(map) => {
            if map.contains_key("dnat") {
                return true;
            }
            map.values().any(value_contains_dnat)
        }
        Value::Array(arr) => arr.iter().any(value_contains_dnat),
        _ => false,
    }
}

fn parse_forward_allow_rule(
    family: &str,
    table: &str,
    chain: &str,
    exprs: &[Value],
    comment: Option<&str>,
) -> Option<Vec<FirewallForwardAllow>> {
    let has_accept = exprs.iter().any(|e| e.get("accept").is_some());
    if !has_accept {
        return None;
    }

    let mut dest_ip: Option<Ipv4Addr> = None;
    let mut proto: Option<String> = None;
    let mut dest_ports: Vec<u16> = Vec::new();
    let mut sources: Vec<String> = Vec::new();

    for expr in exprs {
        let Some(m) = expr.get("match").and_then(Value::as_object) else {
            continue;
        };
        let op = m.get("op").and_then(Value::as_str).unwrap_or("==");
        if !matches!(op, "==" | "in") {
            continue;
        }
        let Some(left) = m.get("left") else {
            continue;
        };
        let Some(right) = m.get("right") else {
            continue;
        };
        if let Some(payload) = left.get("payload").and_then(Value::as_object) {
            let p = payload
                .get("protocol")
                .and_then(Value::as_str)
                .unwrap_or("");
            let f = payload.get("field").and_then(Value::as_str).unwrap_or("");
            match (p, f) {
                ("ip", "daddr") => {
                    if let Some(s) = right.as_str() {
                        dest_ip = s.parse::<Ipv4Addr>().ok();
                    }
                }
                ("ip", "saddr") => {
                    sources.extend(extract_nft_ipv4_sources(right));
                }
                ("tcp" | "udp", "dport") => {
                    proto = Some(p.to_owned());
                    if let Ok(ranges) = extract_nft_port_ranges_checked(right) {
                        for (start, end) in ranges {
                            if end - start <= MAX_EXPANDED_PORT_RANGE {
                                dest_ports.extend(start..=end);
                            } else {
                                dest_ports.push(start);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let dest_ip = dest_ip?;
    let proto = proto?;
    if dest_ports.is_empty() {
        return None;
    }

    Some(
        dest_ports
            .into_iter()
            .map(|dest_port| FirewallForwardAllow {
                family: family.to_owned(),
                table: table.to_owned(),
                chain: chain.to_owned(),
                dest_ip,
                proto: proto.clone(),
                dest_port,
                sources: sources.clone(),
                comment: comment.map(str::to_owned),
            })
            .collect(),
    )
}

fn parse_dnat_rule(
    family: &str,
    table: &str,
    chain: &str,
    exprs: &[Value],
    comment: Option<&str>,
    handle: Option<u64>,
) -> std::result::Result<Vec<FirewallDnatRule>, String> {
    let mut dnat_addr: Option<Ipv4Addr> = None;
    let mut dnat_port: Option<u16> = None;
    let mut dnat_count = 0usize;
    let mut public_ip: Option<Ipv4Addr> = None;
    let mut daddr_ambiguous = false;
    let mut proto: Option<String> = None;
    let mut host_port_ranges: Vec<(u16, u16)> = Vec::new();
    let mut sources: Vec<String> = Vec::new();

    for expr in exprs {
        if let Some(dnat_val) = expr.get("dnat") {
            dnat_count += 1;
            if dnat_count > 1 {
                return Err("rule contains multiple dnat statements".to_owned());
            }
            let Some(dnat) = dnat_val.as_object() else {
                return Err(format!("non-object dnat statement: {dnat_val}"));
            };
            let Some(addr_val) = dnat.get("addr") else {
                return Err("dnat statement missing `addr`".to_owned());
            };
            let Some(addr_str) = addr_val.as_str() else {
                return Err(format!(
                    "dynamic or non-static dnat target `addr`: {addr_val}"
                ));
            };
            let parsed_ip = addr_str
                .parse::<Ipv4Addr>()
                .map_err(|_| format!("non-IPv4 dnat target `addr`: `{addr_str}`"))?;
            dnat_addr = Some(parsed_ip);

            if let Some(port_val) = dnat.get("port") {
                let Some(p_u64) = port_val.as_u64() else {
                    return Err(format!(
                        "dynamic or non-numeric dnat target `port`: {port_val}"
                    ));
                };
                let p = u16::try_from(p_u64)
                    .ok()
                    .filter(|&p| p > 0)
                    .ok_or_else(|| format!("out-of-range dnat target `port`: {p_u64}"))?;
                dnat_port = Some(p);
            }
            continue;
        }
        if value_contains_dnat(expr) {
            return Err(format!("nested/map dnat expression: {expr}"));
        }

        let Some(m) = expr.get("match").and_then(Value::as_object) else {
            continue;
        };
        let op = m.get("op").and_then(Value::as_str).unwrap_or("");
        let (Some(left), Some(right)) = (m.get("left"), m.get("right")) else {
            continue;
        };
        if let Some(payload) = left.get("payload").and_then(Value::as_object) {
            let p = payload
                .get("protocol")
                .and_then(Value::as_str)
                .unwrap_or("");
            let f = payload.get("field").and_then(Value::as_str).unwrap_or("");
            match (p, f) {
                ("ip", "daddr") => {
                    if op == "==" {
                        if let Some(ip) = right.as_str().and_then(|s| s.parse::<Ipv4Addr>().ok()) {
                            if public_ip.is_none() && !daddr_ambiguous {
                                public_ip = Some(ip);
                            } else {
                                public_ip = None;
                                daddr_ambiguous = true;
                            }
                        } else {
                            // Set, prefix, or named set on ip daddr: conservatively treat as wildcard.
                            public_ip = None;
                            daddr_ambiguous = true;
                        }
                    } else {
                        public_ip = None;
                        daddr_ambiguous = true;
                    }
                }
                ("ip", "saddr") => {
                    sources.extend(extract_nft_ipv4_sources(right));
                }
                ("tcp" | "udp", "dport") => {
                    if !matches!(op, "==" | "in") {
                        return Err(format!(
                            "non-equality operator `{op}` on `{p} dport` in DNAT rule"
                        ));
                    }
                    if let Some(ref existing_proto) = proto {
                        if existing_proto != p {
                            return Err(format!(
                                "conflicting transport protocols `{existing_proto}` and `{p}` in DNAT rule"
                            ));
                        }
                    } else {
                        proto = Some(p.to_owned());
                    }
                    let ranges = extract_nft_port_ranges_checked(right)?;
                    host_port_ranges.extend(ranges);
                }
                ("th", "dport") => {
                    return Err("generic transport-header `th dport` match in DNAT rule".to_owned());
                }
                _ => {}
            }
        }
    }

    let Some(dest_ip) = dnat_addr else {
        return Err("DNAT rule missing top-level static `dnat` statement".to_owned());
    };
    let Some(proto) = proto else {
        return Err(
            "wildcard or non-standard DNAT rule without static tcp/udp `dport` match".to_owned(),
        );
    };
    if host_port_ranges.is_empty() {
        return Err("DNAT rule has empty `dport` match".to_owned());
    }

    let mut rules = Vec::new();
    for (start, end) in host_port_ranges {
        if end - start <= MAX_EXPANDED_PORT_RANGE {
            for host_port in start..=end {
                rules.push(FirewallDnatRule {
                    family: family.to_owned(),
                    table: table.to_owned(),
                    chain: chain.to_owned(),
                    public_ip,
                    proto: proto.clone(),
                    host_port,
                    host_port_end: None,
                    dest_ip,
                    guest_port: dnat_port.unwrap_or(host_port),
                    sources: sources.clone(),
                    comment: comment.map(str::to_owned),
                    handle,
                });
            }
        } else {
            rules.push(FirewallDnatRule {
                family: family.to_owned(),
                table: table.to_owned(),
                chain: chain.to_owned(),
                public_ip,
                proto: proto.clone(),
                host_port: start,
                host_port_end: Some(end),
                dest_ip,
                guest_port: dnat_port.unwrap_or(start),
                sources: sources.clone(),
                comment: comment.map(str::to_owned),
                handle,
            });
        }
    }
    Ok(rules)
}

fn extract_nft_ipv4_sources(val: &Value) -> Vec<String> {
    match val {
        Value::String(s) => {
            if let Ok(ip) = s.parse::<Ipv4Addr>() {
                vec![format!("{ip}/32")]
            } else {
                vec![s.clone()]
            }
        }
        Value::Object(map) => {
            if let Some(prefix) = map.get("prefix").and_then(Value::as_object) {
                if let (Some(addr), Some(len)) = (
                    prefix.get("addr").and_then(Value::as_str),
                    prefix.get("len").and_then(Value::as_u64),
                ) {
                    return vec![format!("{addr}/{len}")];
                }
            }
            if let Some(set) = map.get("set").and_then(Value::as_array) {
                let mut out = Vec::new();
                for item in set {
                    out.extend(extract_nft_ipv4_sources(item));
                }
                return out;
            }
            Vec::new()
        }
        Value::Array(arr) => {
            let mut out = Vec::new();
            for item in arr {
                out.extend(extract_nft_ipv4_sources(item));
            }
            out
        }
        _ => Vec::new(),
    }
}

/// Strictly parse an nftables `dport` RHS into inclusive `(start, end)` port ranges.
///
/// Supports single port numbers, `{"range": [start, end]}`, `{"set": [...]}` and arrays of those.
/// Fails closed on named sets (`"@ports"`), maps, concatenations, or malformed values (F4).
fn extract_nft_port_ranges_checked(val: &Value) -> std::result::Result<Vec<(u16, u16)>, String> {
    match val {
        Value::Number(n) => {
            let p_u64 = n
                .as_u64()
                .ok_or_else(|| format!("non-integer port expression: {val}"))?;
            let p = u16::try_from(p_u64)
                .ok()
                .filter(|&p| p > 0)
                .ok_or_else(|| format!("out-of-range port number: {p_u64}"))?;
            Ok(vec![(p, p)])
        }
        Value::Object(map) => {
            if let Some(range_arr) = map.get("range").and_then(Value::as_array) {
                if range_arr.len() != 2 {
                    return Err(format!("malformed port range expression: {val}"));
                }
                let start = range_arr[0]
                    .as_u64()
                    .and_then(|p| u16::try_from(p).ok())
                    .filter(|&p| p > 0)
                    .ok_or_else(|| format!("invalid port range start in {val}"))?;
                let end = range_arr[1]
                    .as_u64()
                    .and_then(|p| u16::try_from(p).ok())
                    .filter(|&p| p > 0)
                    .ok_or_else(|| format!("invalid port range end in {val}"))?;
                if start > end {
                    return Err(format!("inverted port range {start}..={end} in {val}"));
                }
                return Ok(vec![(start, end)]);
            }
            if let Some(set) = map.get("set").and_then(Value::as_array) {
                if set.is_empty() {
                    return Err("empty port set expression".to_owned());
                }
                let mut out = Vec::new();
                for item in set {
                    out.extend(extract_nft_port_ranges_checked(item)?);
                }
                return Ok(out);
            }
            Err(format!("unsupported nftables dport expression: {val}"))
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                return Err("empty port array expression".to_owned());
            }
            let mut out = Vec::new();
            for item in arr {
                out.extend(extract_nft_port_ranges_checked(item)?);
            }
            Ok(out)
        }
        other => Err(format!("unparseable nftables dport expression: {other}")),
    }
}

fn detect_vmm_status<T: VmmTransport>(vmm: &VmmClient<T>, socket_path: &Path) -> VmmStatus {
    let unit = query_systemd_unit(DSTACK_VMM_SERVICE);
    let version_res = vmm.version().ok();
    let meta_res = vmm.get_meta_typed().ok();
    let vms_res = list_all_vms_best_effort(vmm);
    let reachable = version_res.is_some() || meta_res.is_some() || vms_res.is_some();

    let (running_vms, total_vms) = match vms_res {
        Some(vms) => {
            let running = vms.iter().filter(|v| v.running).count();
            (Some(running), Some(vms.len()))
        }
        None => (None, None),
    };

    let mut drift_notes = Vec::new();
    if reachable && unit.is_enabled() && !unit.is_active() {
        drift_notes.push(
            "dstack-vmm.service is enabled/inactive while live VMM responds on UDS; do not start/restart dstack-vmm.service"
                .to_owned(),
        );
    }
    if meta_res
        .as_ref()
        .is_some_and(|m| m.networking.default_mode == "user")
    {
        drift_notes.push(
            "live VMM reports default_mode=user; pass --bridge <name> explicitly to outbe-up vm create"
                .to_owned(),
        );
    }

    VmmStatus {
        socket_path: socket_path.to_path_buf(),
        reachable,
        version: version_res
            .as_ref()
            .map(|v| v.version.clone())
            .filter(|s| !s.is_empty()),
        rev: version_res
            .as_ref()
            .map(|v| v.rev.clone())
            .filter(|s| !s.is_empty()),
        default_mode: meta_res
            .as_ref()
            .map(|m| m.networking.default_mode.clone())
            .filter(|s| !s.is_empty()),
        supported_modes: meta_res
            .as_ref()
            .map(|m| m.networking.supported_modes.clone())
            .unwrap_or_default(),
        default_bridge: meta_res
            .as_ref()
            .map(|m| m.networking.default_bridge.clone())
            .filter(|s| !s.is_empty()),
        max_cvm_number: meta_res.as_ref().map(|m| m.resources.max_cvm_number),
        max_allocable_vcpu: meta_res.as_ref().map(|m| m.resources.max_allocable_vcpu),
        max_allocable_memory_in_mb: meta_res
            .as_ref()
            .map(|m| m.resources.max_allocable_memory_in_mb),
        running_vms,
        total_vms,
        systemd_unit_enabled: unit.is_enabled(),
        systemd_unit_active: unit.is_active(),
        drift_notes,
    }
}

fn list_all_vms_best_effort<T: VmmTransport>(
    vmm: &VmmClient<T>,
) -> Option<Vec<crate::vmm::VmInfo>> {
    let mut all = Vec::new();
    for page in 1..=MAX_STATUS_PAGES {
        let resp = vmm
            .status(&StatusRequest {
                ids: Vec::new(),
                brief: false,
                keyword: String::new(),
                page,
                page_size: STATUS_PAGE_SIZE,
                status: String::new(),
            })
            .ok()?;
        let count = resp.vms.len();
        all.extend(resp.vms);
        if count < STATUS_PAGE_SIZE as usize || (resp.total > 0 && all.len() >= resp.total as usize)
        {
            return Some(all);
        }
    }
    Some(all)
}

fn detect_platform(env: &HostEnv) -> PlatformStatus {
    let kvm_path = env.path("/dev/kvm");
    let kvm_available = kvm_path.exists();
    let kvm_accessible = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&kvm_path)
        .is_ok();
    let vhost_vsock_available = env.path("/dev/vhost-vsock").exists();

    let sev_snp_enabled = read_sysfs_bool(&env.path("/sys/module/kvm_amd/parameters/sev_snp"));
    let tdx_enabled = read_sysfs_bool(&env.path("/sys/module/kvm_intel/parameters/tdx"));

    let tee_kind = if sev_snp_enabled {
        "amd-sev-snp".to_owned()
    } else if tdx_enabled {
        "intel-tdx".to_owned()
    } else {
        "none".to_owned()
    };

    let qgs_unit = query_systemd_unit(QGSD_SERVICE);
    let pccs_unit = query_systemd_unit(PCCS_SERVICE);
    let qgs = QgsStatus {
        required: tdx_enabled,
        service_name: QGSD_SERVICE.to_owned(),
        unit_enabled: qgs_unit.is_enabled(),
        unit_active: qgs_unit.is_active(),
        pccs_unit_active: pccs_unit.is_active(),
    };

    PlatformStatus {
        kvm_available,
        kvm_accessible,
        vhost_vsock_available,
        tee_kind,
        sev_snp_enabled,
        tdx_enabled,
        qgs,
    }
}

fn read_sysfs_bool(path: &Path) -> bool {
    fs::read_to_string(path)
        .map(|raw| matches!(raw.trim(), "Y" | "y" | "1" | "true"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_AMD_NFT: &str = r#"{
      "nftables": [
        {"metainfo": {"version": "1.1.6", "json_schema_version": 1}},
        {"table": {"family": "inet", "name": "host_firewall", "handle": 66}},
        {"chain": {"family": "inet", "table": "host_firewall", "name": "forward", "handle": 2, "type": "filter", "hook": "forward", "prio": 0, "policy": "drop"}},
        {"rule": {"family": "inet", "table": "host_firewall", "chain": "forward", "handle": 700, "comment": "outbound:validator_vms", "expr": [
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
          {"accept": null}
        ]}},
        {"rule": {"family": "inet", "table": "host_firewall", "chain": "forward", "handle": 711, "comment": "validator-0-hyperlane", "expr": [
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": "198.51.100.30"}},
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": "10.77.0.11"}},
          {"match": {"op": "in", "left": {"ct": {"key": "status"}}, "right": "dnat"}},
          {"match": {"op": "==", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": 9000}},
          {"accept": null}
        ]}},
        {"table": {"family": "ip", "name": "host_firewall_nat", "handle": 67}},
        {"chain": {"family": "ip", "table": "host_firewall_nat", "name": "prerouting", "handle": 1, "type": "nat", "hook": "prerouting", "prio": -100, "policy": "accept"}},
        {"chain": {"family": "ip", "table": "host_firewall_nat", "name": "postrouting", "handle": 2, "type": "nat", "hook": "postrouting", "prio": 100, "policy": "accept"}},
        {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 456, "comment": "validator-0-hyperlane", "expr": [
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": "198.51.100.10"}},
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": "198.51.100.30"}},
          {"match": {"op": "==", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": 9000}},
          {"dnat": {"addr": "10.77.0.11"}}
        ]}},
        {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 466, "comment": "validator-1-hyperlane", "expr": [
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": "198.51.100.10"}},
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": "198.51.100.30"}},
          {"match": {"op": "==", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": 9001}},
          {"dnat": {"addr": "10.77.0.12", "port": 9000}}
        ]}},
        {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "postrouting", "handle": 468, "comment": "outbound:validator_vms", "expr": [
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
          {"match": {"op": "!=", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
          {"masquerade": null}
        ]}}
      ]
    }"#;

    #[test]
    fn parse_firewall_json_extracts_dnat_forward_allows_and_egress() {
        let subnet = Ipv4Subnet::from_addr_prefix(Ipv4Addr::new(10, 77, 0, 1), 24).unwrap();
        let (fw, egress, partial) =
            parse_firewall_and_egress_json(SAMPLE_AMD_NFT, "obr0", Some(subnet), Some("eno1"))
                .unwrap();
        assert!(fw.nft_accessible);
        assert!(!fw.managed_nat_table_present);
        assert_eq!(fw.tables.len(), 2);
        assert_eq!(fw.forward_chains.len(), 1);
        assert_eq!(fw.forward_chains[0].policy, "drop");
        assert_eq!(fw.forward_allows.len(), 1);
        assert_eq!(fw.forward_allows[0].dest_ip, Ipv4Addr::new(10, 77, 0, 11));
        assert_eq!(fw.forward_allows[0].dest_port, 9000);
        assert_eq!(fw.forward_allows[0].sources, vec!["198.51.100.30/32"]);

        assert_eq!(fw.dnat_rules.len(), 2);
        assert_eq!(fw.dnat_rules[0].host_port, 9000);
        assert_eq!(fw.dnat_rules[0].guest_port, 9000);
        assert_eq!(fw.dnat_rules[0].dest_ip, Ipv4Addr::new(10, 77, 0, 11));
        assert_eq!(
            fw.dnat_rules[0].public_ip,
            Some(Ipv4Addr::new(198, 51, 100, 10))
        );
        assert_eq!(fw.dnat_rules[1].host_port, 9001);
        assert_eq!(fw.dnat_rules[1].guest_port, 9000);
        assert_eq!(fw.dnat_rules[1].dest_ip, Ipv4Addr::new(10, 77, 0, 12));

        assert_eq!(egress.len(), 1);
        assert_eq!(egress[0].table, "host_firewall_nat");
        assert!(partial.is_empty());
    }

    #[test]
    fn parse_systemd_dnsmasq_argv_matches_real_validator_vm_dhcp_unit() {
        let raw = "\
LoadState=loaded
ActiveState=active
UnitFileState=enabled
MainPID=219283
ExecStart={ path=/usr/sbin/dnsmasq ; argv[]=/usr/sbin/dnsmasq --user=root --conf-file=/dev/null --interface=obr0 --bind-interfaces --except-interface=lo --port=0 --dhcp-ignore-clid --dhcp-range=10.77.0.100,10.77.0.200,255.255.255.0,5m --dhcp-hostsfile=/var/lib/validator-vm/dhcp-hosts --dhcp-option=option:router,10.77.0.1 --dhcp-leasefile=/var/lib/validator-vm/dhcp-leases --pid-file=/run/validator-vm-dhcp.pid ; ignore_errors=no }
ExecReload={ path=/bin/kill ; argv[]=/bin/kill -HUP $MAINPID ; ignore_errors=no }
";
        let props = parse_systemd_show_props(raw);
        assert!(props.is_loaded());
        assert!(props.is_active());
        assert!(props.is_enabled());
        assert_eq!(props.main_pid, Some(219283));
        let argv = extract_systemd_exec_argv(&props.exec_start);
        let parsed = parse_dnsmasq_argv(&argv);
        assert_eq!(parsed.interface.as_deref(), Some("obr0"));
        assert_eq!(
            parsed.hosts_file.as_deref(),
            Some(Path::new("/var/lib/validator-vm/dhcp-hosts"))
        );
        assert_eq!(
            parsed.leases_file.as_deref(),
            Some(Path::new("/var/lib/validator-vm/dhcp-leases"))
        );
        assert_eq!(
            parsed.pid_file.as_deref(),
            Some(Path::new("/run/validator-vm-dhcp.pid"))
        );
        assert_eq!(parsed.pool_start, Some(Ipv4Addr::new(10, 77, 0, 100)));
        assert_eq!(parsed.pool_end, Some(Ipv4Addr::new(10, 77, 0, 200)));
        assert_eq!(parsed.netmask, Some(Ipv4Addr::new(255, 255, 255, 0)));
        assert_eq!(parsed.lease_duration.as_deref(), Some("5m"));
    }

    #[test]
    fn port_spec_and_cidr_parsers_validate_inputs() {
        assert_eq!(
            parse_port_spec("9000").unwrap(),
            PublishPortSpec {
                guest_port: 9000,
                host_port: 9000,
                proto: TransportProto::Tcp,
            }
        );
        assert_eq!(
            parse_port_spec("9000:9010").unwrap(),
            PublishPortSpec {
                guest_port: 9000,
                host_port: 9010,
                proto: TransportProto::Tcp,
            }
        );
        assert_eq!(
            parse_port_spec("30303:30313/udp").unwrap(),
            PublishPortSpec {
                guest_port: 30303,
                host_port: 30313,
                proto: TransportProto::Udp,
            }
        );
        assert!(parse_port_spec("0").is_err());
        assert!(parse_port_spec("9000/icmp").is_err());

        assert_eq!(
            normalize_ipv4_cidr("198.51.100.30").unwrap(),
            "198.51.100.30/32"
        );
        assert_eq!(normalize_ipv4_cidr("10.77.0.5/24").unwrap(), "10.77.0.0/24");
        assert_eq!(normalize_ipv4_cidr("0.0.0.0/0").unwrap(), "0.0.0.0/0");
        assert!(normalize_ipv4_cidr("10.77.0.0/33").is_err());
    }

    #[test]
    fn validate_real_amd_and_intel_dhcp_hosts_formats() {
        let amd_hosts = "\
be:6a:63:f8:84:d0,10.77.0.11,validator-0,infinite
5a:9a:a8:0f:91:46,10.77.0.12,validator-1,infinite

# outbe-up:d3-amd-73d044094:begin
ca:ae:de:a3:b1:11,10.77.0.60
# outbe-up:d3-amd-73d044094:end

# outbe-up:final-amd-f36afc309:begin
be:f7:7b:2c:d1:85,10.77.0.68,10m
# outbe-up:final-amd-f36afc309:end
";
        assert_eq!(validate_dnsmasq_hosts_format(amd_hosts).unwrap(), 4);
        assert!(validate_dnsmasq_hosts_format("not a valid dhcp hosts file\n").is_err());
    }

    #[test]
    fn f4_expands_dnat_port_ranges_and_sets_and_records_unparsed_dnat_rules() {
        let nft_json = r#"{
          "nftables": [
            {"metainfo": {"version": "1.1.6", "json_schema_version": 1}},
            {"table": {"family": "ip", "name": "host_firewall_nat", "handle": 67}},
            {"chain": {"family": "ip", "table": "host_firewall_nat", "name": "prerouting", "handle": 1, "type": "nat", "hook": "prerouting", "prio": -100, "policy": "accept"}},
            {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 10, "comment": "range-dnat", "expr": [
              {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": "198.51.100.10"}},
              {"match": {"op": "==", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": {"range": [9000, 9010]}}},
              {"dnat": {"addr": "10.77.0.11"}}
            ]}},
            {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 11, "comment": "set-dnat", "expr": [
              {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": "198.51.100.10"}},
              {"match": {"op": "==", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": {"set": [9020, 9021, {"range": [9030, 9031]}]}}},
              {"dnat": {"addr": "10.77.0.12"}}
            ]}},
            {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 12, "comment": "huge-range", "expr": [
              {"match": {"op": "==", "left": {"payload": {"protocol": "udp", "field": "dport"}}, "right": {"range": [20000, 25000]}}},
              {"dnat": {"addr": "10.77.0.13"}}
            ]}},
            {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 20, "comment": "named-set", "expr": [
              {"match": {"op": "==", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": "@dynamic_ports"}},
              {"dnat": {"addr": "10.77.0.14"}}
            ]}},
            {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 21, "comment": "dnat-map", "expr": [
              {"dnat": {"addr": {"map": {"key": {"payload": {"protocol": "tcp", "field": "dport"}}, "data": "@port_map"}}}}
            ]}},
            {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 22, "comment": "negated-dport", "expr": [
              {"match": {"op": "!=", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": 22}},
              {"dnat": {"addr": "10.77.0.15"}}
            ]}},
            {"rule": {"family": "ip", "table": "host_firewall_nat", "chain": "prerouting", "handle": 23, "comment": "wildcard-dnat", "expr": [
              {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": "198.51.100.10"}},
              {"dnat": {"addr": "10.77.0.16"}}
            ]}}
          ]
        }"#;

        let subnet = Ipv4Subnet::from_addr_prefix(Ipv4Addr::new(10, 77, 0, 1), 24).unwrap();
        let (fw, _, _) =
            parse_firewall_and_egress_json(nft_json, "obr0", Some(subnet), Some("eno1")).unwrap();

        // 11 (9000..=9010) + 4 (9020, 9021, 9030, 9031) + 1 (compact 20000..=25000) = 16
        assert_eq!(fw.dnat_rules.len(), 16);
        assert!(fw.dnat_rules.iter().any(|r| r.proto == "tcp"
            && r.covers_host_port(9005)
            && r.dest_ip == Ipv4Addr::new(10, 77, 0, 11)));
        assert!(fw.dnat_rules.iter().any(|r| r.proto == "tcp"
            && r.covers_host_port(9021)
            && r.dest_ip == Ipv4Addr::new(10, 77, 0, 12)));
        assert!(fw.dnat_rules.iter().any(|r| r.proto == "tcp"
            && r.covers_host_port(9031)
            && r.dest_ip == Ipv4Addr::new(10, 77, 0, 12)));
        assert!(fw.dnat_rules.iter().any(|r| {
            r.proto == "udp"
                && r.host_port == 20000
                && r.host_port_end == Some(25000)
                && r.covers_host_port(22500)
                && !r.covers_host_port(25001)
        }));

        // All 4 unparseable DNAT rules must be recorded explicitly.
        assert_eq!(fw.unparsed_dnat_rules.len(), 4);
        let handles: Vec<Option<u64>> = fw.unparsed_dnat_rules.iter().map(|u| u.handle).collect();
        assert_eq!(handles, vec![Some(20), Some(21), Some(22), Some(23)]);
    }
}
