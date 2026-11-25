//! Host-side NAT masquerade management for outbound VM internet access.
//!
//! Owns a single nftables table (`ip outbe_up_nat`), a ruleset file
//! (`/etc/outbe-up/nat.nft`), a oneshot systemd unit (`outbe-up-nat.service`),
//! and optionally `/etc/sysctl.d/90-outbe-up-nat.conf` when no persistent
//! sysctl configuration already enables `net.ipv4.ip_forward = 1`.
//! Every managed file carries the `# managed by outbe-up vm host-nat` marker;
//! foreign files and foreign nftables tables (such as `inet outbe_host_fw`) are
//! never modified or deleted.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

use eyre::{bail, ensure, Context, Result};
use serde_json::Value;

use crate::{
    network::{
        build::DeploymentKind,
        config::Ssh,
        remote::{shell_quote, NetworkArtifacts, SshOptions},
    },
    support::progress::Progress,
};

pub const NAT_TABLE_FAMILY: &str = "ip";
pub const NAT_TABLE_NAME: &str = "outbe_up_nat";
pub const NAT_RULES_PATH: &str = "/etc/outbe-up/nat.nft";
pub const NAT_SERVICE_NAME: &str = "outbe-up-nat.service";
pub const NAT_SERVICE_PATH: &str = "/etc/systemd/system/outbe-up-nat.service";
pub const NAT_SYSCTL_PATH: &str = "/etc/sysctl.d/90-outbe-up-nat.conf";
pub const OWNERSHIP_MARKER: &str = "# managed by outbe-up vm host-nat";

const SENTINEL_BRIDGE_ADDR: &str = "===OUTBE_NAT_BRIDGE_ADDR===";
const SENTINEL_UPLINK_ROUTE: &str = "===OUTBE_NAT_UPLINK_ROUTE===";
const SENTINEL_IP_FORWARD: &str = "===OUTBE_NAT_IP_FORWARD===";
const SENTINEL_PERSISTENT_SYSCTL: &str = "===OUTBE_NAT_PERSISTENT_SYSCTL===";
const SENTINEL_OWNED_FILES: &str = "===OUTBE_NAT_OWNED_FILES===";
const SENTINEL_NFT_JSON: &str = "===OUTBE_NAT_NFT_JSON===";

/// Options for `outbe-up vm host-nat`.
pub struct HostNatOptions<'a> {
    pub artifacts: &'a Path,
    pub ssh_key: Option<&'a Path>,
    pub ssh_config: Option<&'a Path>,
    pub uplink: Option<&'a str>,
    pub remove: bool,
    pub dry_run: bool,
    pub force: bool,
}

/// Configurable file paths on the target host (overridden in unit tests).
#[derive(Clone, Debug)]
struct HostNatPaths {
    rules_path: PathBuf,
    service_path: PathBuf,
    sysctl_path: PathBuf,
    sysctl_scan_dirs: Vec<PathBuf>,
    sysctl_conf_path: PathBuf,
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

/// Injectable remote command runner so unit tests can exercise real apply/remove orchestration.
trait HostRunner {
    fn run_raw(&mut self, ssh: &Ssh, host: &str, command: &str) -> Result<String>;
}

struct SshHostRunner<'a> {
    ssh_options: &'a SshOptions,
}

impl HostRunner for SshHostRunner<'_> {
    fn run_raw(&mut self, ssh: &Ssh, host: &str, command: &str) -> Result<String> {
        self.ssh_options.capture_ssh_raw(ssh, host, command)
    }
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

    /// Returns true when `self` covers every address in `other`.
    pub fn covers(self, other: Self) -> bool {
        self.prefix_len <= other.prefix_len && self.contains(other.network)
    }

    pub fn cidr_string(self) -> String {
        format!("{}/{}", self.network, self.prefix_len)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForwardChainClass {
    /// `policy accept` and no rule with `drop`, `reject`, `jump`, `goto`, or `vmap`.
    PureAccept,
    /// `policy drop`, or contains at least one rule with `drop`, `reject`, `jump`, `goto`, or `vmap`.
    BlocksOrUnknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForeignForwardChain {
    pub family: String,
    pub table: String,
    pub chain: String,
    pub priority: String,
    pub policy: String,
    pub class: ForwardChainClass,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExistingEgress {
    pub family: String,
    pub table: String,
    pub chain: String,
    pub kind: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartialEgress {
    pub family: String,
    pub table: String,
    pub chain: String,
    pub kind: String,
    pub narrowing: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NftInspection {
    pub foreign_forward_chains: Vec<ForeignForwardChain>,
    pub blocks_or_unknown_chains: Vec<ForeignForwardChain>,
    pub existing_egress: Vec<ExistingEgress>,
    pub partial_egress: Vec<PartialEgress>,
    pub docker_or_libvirt_chains: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FileOwnership {
    Absent,
    Owned,
    Foreign,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManagedFileStates {
    rules: FileOwnership,
    service: FileOwnership,
    sysctl: FileOwnership,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PersistentSysctlState {
    effective_value: Option<u8>,
    effective_source: Option<String>,
    /// True when `effective_value == Some(0)` and `effective_source` is ordered after
    /// `90-outbe-up-nat.conf` (e.g. `99-*.conf` or `/etc/sysctl.conf`), so writing our
    /// 90- drop-in would still be overridden back to 0 on reboot.
    overridden_by_later_zero: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreflightReport {
    bridge: String,
    bridge_addr: Ipv4Addr,
    subnet: Ipv4Subnet,
    uplink: String,
    ip_forward: u8,
    persistent_sysctl: PersistentSysctlState,
    files: ManagedFileStates,
    nft: NftInspection,
}

#[derive(Clone, Debug)]
struct HostTarget {
    ssh: Ssh,
    bridge: String,
    founder_ips: Vec<Ipv4Addr>,
}

/// Outcome of a single host apply operation (inspected by tests).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApplyOutcome {
    DryRun,
    NoOpExistingEgress,
    Applied,
}

/// Configure or remove host-side NAT masquerade on every distinct VMM host in the network.
pub fn run(options: HostNatOptions<'_>) -> Result<()> {
    if let Some(uplink) = options.uplink {
        validate_iface_name(uplink, "uplink")?;
    }

    let mut progress = Progress::new();
    progress.stage("loading VM network artifacts");
    let build = NetworkArtifacts::load(options.artifacts)?;
    ensure!(
        build.manifest.deployment == DeploymentKind::Vm,
        "network {} is not a VM deployment",
        build.config.name
    );
    let ssh_options = SshOptions::new(options.ssh_key, options.ssh_config)?;
    let mut runner = SshHostRunner {
        ssh_options: &ssh_options,
    };
    let targets = group_host_targets(&build)?;
    let paths = HostNatPaths::default();

    if options.remove {
        for target in &targets {
            run_remove_on_host(target, &mut runner, &paths, options.dry_run, &mut progress)?;
        }
        if options.dry_run {
            progress.done(&format!(
                "dry-run complete: no changes made on {} host(s)",
                targets.len()
            ));
        } else {
            progress.done(&format!(
                "removed managed host NAT artifacts on {} host(s)",
                targets.len()
            ));
        }
        return Ok(());
    }

    let mut applied_count = 0usize;
    let mut noop_count = 0usize;
    for target in &targets {
        match run_apply_on_host(
            target,
            &mut runner,
            options.uplink,
            &paths,
            options.dry_run,
            options.force,
            options.artifacts,
            &mut progress,
        )? {
            ApplyOutcome::DryRun => {}
            ApplyOutcome::NoOpExistingEgress => noop_count += 1,
            ApplyOutcome::Applied => applied_count += 1,
        }
    }

    if options.dry_run {
        progress.done(&format!(
            "dry-run complete: no changes made on {} host(s)",
            targets.len()
        ));
    } else if applied_count == 0 && noop_count > 0 {
        progress.done(&format!(
            "no changes needed on {noop_count} host(s) (outbound egress already provided)"
        ));
    } else {
        progress.done(&format!(
            "host NAT complete: {applied_count} applied, {noop_count} already provided"
        ));
    }
    Ok(())
}

fn group_host_targets(build: &NetworkArtifacts) -> Result<Vec<HostTarget>> {
    let mut grouped: BTreeMap<(String, u16, String), HostTarget> = BTreeMap::new();
    for founder in &build.config.founders {
        let resolved = build.config.resolve_founder_vm(founder)?;
        validate_iface_name(&resolved.bridge, "bridge")?;
        let key = (
            resolved.vmm_host.host.clone(),
            resolved.vmm_host.port,
            resolved.vmm_host.user.clone(),
        );
        match grouped.get_mut(&key) {
            Some(existing) => {
                ensure!(
                    existing.bridge == resolved.bridge,
                    "VMM host {} has founders on multiple bridges ({} and {}); \
                     `outbe-up vm host-nat` manages one bridge per host",
                    resolved.vmm_host.host,
                    existing.bridge,
                    resolved.bridge
                );
                if !existing.founder_ips.contains(&resolved.bridge_ip) {
                    existing.founder_ips.push(resolved.bridge_ip);
                }
            }
            None => {
                grouped.insert(
                    key,
                    HostTarget {
                        ssh: resolved.vmm_host,
                        bridge: resolved.bridge,
                        founder_ips: vec![resolved.bridge_ip],
                    },
                );
            }
        }
    }
    Ok(grouped.into_values().collect())
}

#[allow(clippy::too_many_arguments)]
fn run_apply_on_host<R: HostRunner>(
    target: &HostTarget,
    runner: &mut R,
    uplink_override: Option<&str>,
    paths: &HostNatPaths,
    dry_run: bool,
    force: bool,
    artifacts: &Path,
    progress: &mut Progress,
) -> Result<ApplyOutcome> {
    let host = &target.ssh.host;
    progress.stage(&format!("running read-only NAT preflight on {host}"));
    let preflight_cmd = build_preflight_command(&target.bridge, uplink_override, paths)?;
    let raw = runner
        .run_raw(&target.ssh, host, &preflight_cmd)
        .wrap_err_with(|| format!("run NAT preflight on {host}"))?;
    let report = parse_preflight_output(&raw, &target.bridge, &target.founder_ips)?;

    print_preflight_report(host, &report, &target.founder_ips);

    // 1. Existing full foreign egress detection: no-op unless --force.
    if report.ip_forward == 1 && !report.nft.existing_egress.is_empty() && !force {
        for egress in &report.nft.existing_egress {
            println!(
                "egress already provided by {} {} {}",
                egress.family, egress.table, egress.chain
            );
        }
        println!(
            "Skipping apply on {host} (no changes made; pass --force to install `table {NAT_TABLE_FAMILY} {NAT_TABLE_NAME}` anyway)."
        );
        return Ok(ApplyOutcome::NoOpExistingEgress);
    }

    // 1b. Partial foreign masquerade/SNAT (narrowed by extra matches): cannot decide statically.
    if report.nft.existing_egress.is_empty() && !report.nft.partial_egress.is_empty() && !force {
        let rules = report
            .nft
            .partial_egress
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
        bail!(
            "refusing to apply NAT on {host}: partial foreign masquerade/snat rule(s) found \
             that cannot be decided statically as full egress coverage: {rules}. Inspect those \
             rules and re-run with --force to install `table {NAT_TABLE_FAMILY} {NAT_TABLE_NAME}`"
        );
    }

    // 2. Refuse foreign file collisions and later persistent sysctl=0 overrides before any mutation.
    let mut foreign_collisions = Vec::new();
    if report.files.rules == FileOwnership::Foreign {
        foreign_collisions.push(paths.rules_path.display().to_string());
    }
    if report.files.service == FileOwnership::Foreign {
        foreign_collisions.push(paths.service_path.display().to_string());
    }
    let need_sysctl_dropin = report.persistent_sysctl.effective_value != Some(1);
    if need_sysctl_dropin && report.persistent_sysctl.overridden_by_later_zero {
        let overriding = report
            .persistent_sysctl
            .effective_source
            .as_deref()
            .unwrap_or("unknown sysctl file");
        bail!(
            "cannot persist net.ipv4.ip_forward=1 on {host} via {}: `{overriding}` sets \
             net.ipv4.ip_forward=0 and takes precedence on reboot. Update or remove `{overriding}` \
             first",
            paths.sysctl_path.display()
        );
    }
    if need_sysctl_dropin && report.files.sysctl == FileOwnership::Foreign {
        foreign_collisions.push(paths.sysctl_path.display().to_string());
    }
    ensure!(
        foreign_collisions.is_empty(),
        "refusing to overwrite unmanaged file(s) on {host} (missing `{OWNERSHIP_MARKER}` marker): {}",
        foreign_collisions.join(", ")
    );

    // 3. Refuse when any foreign forward base chain is classified blocks-or-unknown, unless --force.
    let has_blocks_or_unknown = !report.nft.blocks_or_unknown_chains.is_empty();
    if has_blocks_or_unknown && !force {
        let chains = report
            .nft
            .blocks_or_unknown_chains
            .iter()
            .map(|c| {
                format!(
                    "{} {}::{} (priority {}, policy {}, {})",
                    c.family,
                    c.table,
                    c.chain,
                    c.priority,
                    c.policy,
                    c.reasons.join("; ")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "refusing to apply NAT on {host}: foreign forward base chain(s) cannot be verified \
             statically (nftables evaluates every forward base chain independently): {chains}. \
             `outbe-up vm host-nat` never edits foreign nftables tables. Inspect those chains and \
             re-run with --force only if they permit bridge `{}` forwarding, then verify egress \
             from a guest",
            report.bridge
        );
    }

    let ruleset = render_nat_ruleset(&report.bridge, report.subnet, &report.uplink)?;
    let unit = render_systemd_unit(&paths.rules_path);
    let enable_runtime_ip_forward = report.ip_forward == 0;
    let apply_cmd = build_apply_command(
        &ruleset,
        &unit,
        need_sysctl_dropin,
        enable_runtime_ip_forward,
        paths,
    );

    if dry_run {
        println!("\n--- Dry-run plan for {host} (no changes applied) ---");
        println!("Ruleset ({}):\n{ruleset}", paths.rules_path.display());
        println!("Systemd unit ({}):\n{unit}", paths.service_path.display());
        if need_sysctl_dropin {
            println!(
                "Sysctl drop-in ({}): will be written with `{OWNERSHIP_MARKER}` and `net.ipv4.ip_forward = 1` (runtime sysctl -w: {enable_runtime_ip_forward})",
                paths.sysctl_path.display()
            );
        } else {
            println!(
                "Sysctl drop-in ({}): not needed (persistent net.ipv4.ip_forward=1 already set by {})",
                paths.sysctl_path.display(),
                report
                    .persistent_sysctl
                    .effective_source
                    .as_deref()
                    .unwrap_or("existing sysctl config")
            );
        }
        println!("Remote command:\n{apply_cmd}\n");
        return Ok(ApplyOutcome::DryRun);
    }

    progress.stage(&format!(
        "applying atomic nftables table `{NAT_TABLE_FAMILY} {NAT_TABLE_NAME}` on {host}"
    ));
    runner
        .run_raw(&target.ssh, host, &apply_cmd)
        .wrap_err_with(|| format!("apply host NAT on {host}"))?;

    progress.stage(&format!(
        "verifying `{NAT_TABLE_FAMILY} {NAT_TABLE_NAME}` on {host}"
    ));
    let verify_cmd = format!(
        "sudo -n nft list table {} {}",
        shell_quote(NAT_TABLE_FAMILY),
        shell_quote(NAT_TABLE_NAME)
    );
    let live_table = runner
        .run_raw(&target.ssh, host, &verify_cmd)
        .wrap_err_with(|| {
            format!("verify `{NAT_TABLE_FAMILY} {NAT_TABLE_NAME}` table on {host}")
        })?;
    println!(
        "\nVerified live table on {host}:\n{}",
        live_table.trim_end()
    );
    if has_blocks_or_unknown {
        println!(
            "Warning: applied with --force while foreign forward chain(s) could not be verified \
             statically. Verify outbound connectivity from inside a guest VM."
        );
    }
    println!(
        "Note: the host cannot probe guest egress without guest SSH. Verify VM health with:\n  \
         outbe-up vm status --artifacts {}\n\
         To verify guest outbound connectivity on a dev VM, run an HTTPS/DNS probe from inside \
         a guest via the VMM host.",
        artifacts.display()
    );
    Ok(ApplyOutcome::Applied)
}

fn run_remove_on_host<R: HostRunner>(
    target: &HostTarget,
    runner: &mut R,
    paths: &HostNatPaths,
    dry_run: bool,
    progress: &mut Progress,
) -> Result<()> {
    let host = &target.ssh.host;
    let remove_cmd = build_remove_command(paths);
    if dry_run {
        let probe_cmd = build_ownership_probe_command(paths);
        let raw = runner
            .run_raw(&target.ssh, host, &probe_cmd)
            .wrap_err_with(|| format!("probe managed file ownership on {host}"))?;
        let states = parse_owned_files_section(&raw)?;
        println!("\n--- Dry-run remove plan for {host} (no changes applied) ---");
        println!("  {}: {:?}", paths.rules_path.display(), states.rules);
        println!("  {}: {:?}", paths.service_path.display(), states.service);
        println!("  {}: {:?}", paths.sysctl_path.display(), states.sysctl);
        println!(
            "  runtime net.ipv4.ip_forward: left unchanged (remove never writes ip_forward=0)"
        );
        println!("Remote command:\n{remove_cmd}\n");
        return Ok(());
    }

    progress.stage(&format!("removing managed host NAT artifacts on {host}"));
    let output = runner
        .run_raw(&target.ssh, host, &remove_cmd)
        .wrap_err_with(|| format!("remove host NAT on {host}"))?;
    let trimmed = output.trim();
    if !trimmed.is_empty() {
        println!("{trimmed}");
    }
    println!(
        "Note on {host}: runtime net.ipv4.ip_forward was left unchanged so other host workloads \
         are not disrupted."
    );
    Ok(())
}

fn print_preflight_report(host: &str, report: &PreflightReport, founder_ips: &[Ipv4Addr]) {
    let ips = founder_ips
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    println!("\nNAT preflight on {host}:");
    println!(
        "  bridge:            {} (host {}, subnet {})",
        report.bridge,
        report.bridge_addr,
        report.subnet.cidr_string()
    );
    println!("  founder IPs:       {ips} (all within bridge subnet)");
    println!("  uplink:            {}", report.uplink);
    println!(
        "  ip_forward:        runtime={}, persistent={} ({})",
        report.ip_forward,
        report
            .persistent_sysctl
            .effective_value
            .map_or_else(|| "unset".to_owned(), |v| v.to_string()),
        report
            .persistent_sysctl
            .effective_source
            .as_deref()
            .unwrap_or("none")
    );
    if report.nft.existing_egress.is_empty() {
        println!("  existing egress:   none detected");
    } else {
        for egress in &report.nft.existing_egress {
            println!(
                "  existing egress:   {} {} {} ({})",
                egress.family, egress.table, egress.chain, egress.kind
            );
        }
    }
    for partial in &report.nft.partial_egress {
        println!(
            "  partial egress:    {} {} {} ({}, narrowed by: {} — partial, cannot decide statically)",
            partial.family,
            partial.table,
            partial.chain,
            partial.kind,
            partial.narrowing.join(", ")
        );
    }
    if report.nft.foreign_forward_chains.is_empty() {
        println!("  foreign fwd:       none");
    } else {
        for chain in &report.nft.foreign_forward_chains {
            let class_label = match chain.class {
                ForwardChainClass::PureAccept => "pure-accept",
                ForwardChainClass::BlocksOrUnknown => "blocks-or-unknown",
            };
            let detail = if chain.reasons.is_empty() {
                String::new()
            } else {
                format!(" [{}]", chain.reasons.join("; "))
            };
            println!(
                "  foreign fwd:       {} {}::{} (priority {}, policy {}, {class_label}{detail})",
                chain.family, chain.table, chain.chain, chain.priority, chain.policy
            );
        }
        if !report.nft.blocks_or_unknown_chains.is_empty() {
            println!(
                "  fwd verdict:       cannot be decided statically (foreign forward chain(s) \
                 classified blocks-or-unknown; requires --force and guest-side verification)"
            );
        }
    }
    if report.nft.docker_or_libvirt_chains.is_empty() {
        println!("  docker/libvirt:    none detected");
    } else {
        println!(
            "  docker/libvirt:    {}",
            report.nft.docker_or_libvirt_chains.join(", ")
        );
    }
}

/// Strictly validate a Linux network interface name to prevent shell and nftables injection.
///
/// Accepts 1..=15 ASCII characters: starts with alphanumeric, followed by alphanumeric,
/// `.`, `_`, or `-`.
pub fn validate_iface_name(name: &str, label: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= 15,
        "{label} interface name must be 1..=15 characters, got '{name}'"
    );
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    ensure!(
        first.is_ascii_alphanumeric(),
        "{label} interface name must start with an ASCII letter or digit, got '{name}'"
    );
    ensure!(
        chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
        "{label} interface name contains forbidden characters (allowed: A-Z, a-z, 0-9, ., _, -): '{name}'"
    );
    Ok(())
}

/// Render the atomic, idempotent nftables ruleset for `table ip outbe_up_nat`.
///
/// Uses the `ip` family because VM bridge networking and `net.ipv4.ip_forward` are
/// IPv4-only; this avoids touching IPv6 hooks and works on all Linux nftables kernels.
pub fn render_nat_ruleset(bridge: &str, subnet: Ipv4Subnet, uplink: &str) -> Result<String> {
    validate_iface_name(bridge, "bridge")?;
    validate_iface_name(uplink, "uplink")?;
    ensure!(
        bridge != uplink,
        "uplink interface '{uplink}' must differ from bridge interface '{bridge}'"
    );
    let cidr = subnet.cidr_string();
    Ok(format!(
        "{OWNERSHIP_MARKER}\n\
         table {NAT_TABLE_FAMILY} {NAT_TABLE_NAME}\n\
         delete table {NAT_TABLE_FAMILY} {NAT_TABLE_NAME}\n\
         \n\
         table {NAT_TABLE_FAMILY} {NAT_TABLE_NAME} {{\n\
         \tchain postrouting {{\n\
         \t\ttype nat hook postrouting priority srcnat; policy accept;\n\
         \t\tiifname \"{bridge}\" oifname \"{uplink}\" ip saddr {cidr} masquerade\n\
         \t}}\n\
         \n\
         \tchain forward {{\n\
         \t\ttype filter hook forward priority filter; policy accept;\n\
         \t\tiifname \"{bridge}\" oifname \"{uplink}\" ip saddr {cidr} accept\n\
         \t\tiifname \"{uplink}\" oifname \"{bridge}\" ip daddr {cidr} ct state established,related accept\n\
         \t}}\n\
         }}\n"
    ))
}

fn render_systemd_unit(rules_path: &Path) -> String {
    let rules = rules_path.display();
    format!(
        "{OWNERSHIP_MARKER}\n\
         [Unit]\n\
         Description=Outbe VM outbound NAT masquerade\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         RemainAfterExit=yes\n\
         ExecStart=/usr/sbin/nft -f {rules}\n\
         ExecStop=/bin/sh -c '/usr/sbin/nft delete table {NAT_TABLE_FAMILY} {NAT_TABLE_NAME} 2>/dev/null || true'\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    )
}

fn ownership_probe_snippet(paths: &HostNatPaths) -> String {
    let rules_q = shell_quote(&paths.rules_path.to_string_lossy());
    let service_q = shell_quote(&paths.service_path.to_string_lossy());
    let sysctl_q = shell_quote(&paths.sysctl_path.to_string_lossy());
    let marker_q = shell_quote(OWNERSHIP_MARKER);
    format!(
        "probe_file() {{\n\
           if [ ! -e \"$1\" ]; then echo absent;\n\
           elif grep -Fq -- {marker_q} \"$1\" 2>/dev/null; then echo owned;\n\
           else echo foreign; fi\n\
         }}\n\
         printf 'rules=%s\\n' \"$(probe_file {rules_q})\"\n\
         printf 'service=%s\\n' \"$(probe_file {service_q})\"\n\
         printf 'sysctl=%s\\n' \"$(probe_file {sysctl_q})\"\n"
    )
}

fn build_ownership_probe_command(paths: &HostNatPaths) -> String {
    let script = ownership_probe_snippet(paths);
    format!("sudo -n sh -ec {}", shell_quote(&script))
}

fn build_preflight_command(
    bridge: &str,
    uplink_override: Option<&str>,
    paths: &HostNatPaths,
) -> Result<String> {
    validate_iface_name(bridge, "bridge")?;
    let uplink_snippet = match uplink_override {
        Some(uplink) => {
            validate_iface_name(uplink, "uplink")?;
            let q = shell_quote(uplink);
            format!("ip link show dev {q} >/dev/null && printf 'override %s\\n' {q}")
        }
        None => "ip -4 -o route show default".to_owned(),
    };
    let bridge_q = shell_quote(bridge);
    let scan_dirs = paths
        .sysctl_scan_dirs
        .iter()
        .map(|d| shell_quote(&d.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    let sysctl_conf_q = shell_quote(&paths.sysctl_conf_path.to_string_lossy());
    let owned_snippet = ownership_probe_snippet(paths);

    let script = format!(
        "printf '%s\\n' '{SENTINEL_BRIDGE_ADDR}'\n\
         ip -4 -o addr show dev {bridge_q}\n\
         printf '%s\\n' '{SENTINEL_UPLINK_ROUTE}'\n\
         {uplink_snippet}\n\
         printf '%s\\n' '{SENTINEL_IP_FORWARD}'\n\
         sysctl -n net.ipv4.ip_forward\n\
         printf '%s\\n' '{SENTINEL_PERSISTENT_SYSCTL}'\n\
         dir_idx=0\n\
         for d in {scan_dirs}; do\n\
           if [ -d \"$d\" ]; then\n\
             for f in \"$d\"/*.conf; do\n\
               [ -e \"$f\" ] || [ -L \"$f\" ] || continue\n\
               printf 'FILE %s %s\\n' \"$dir_idx\" \"$f\"\n\
               if [ -f \"$f\" ]; then\n\
                 grep -H -E '^[[:space:]]*net[./]ipv4[./]ip_forward[[:space:]]*=' \"$f\" 2>/dev/null | sed 's/^/SET /' || true\n\
               fi\n\
             done\n\
           fi\n\
           dir_idx=$((dir_idx + 1))\n\
         done\n\
         if [ -f {sysctl_conf_q} ]; then\n\
           printf 'CONF %s\\n' {sysctl_conf_q}\n\
           grep -H -E '^[[:space:]]*net[./]ipv4[./]ip_forward[[:space:]]*=' {sysctl_conf_q} 2>/dev/null | sed 's/^/SET /' || true\n\
         fi\n\
         printf '%s\\n' '{SENTINEL_OWNED_FILES}'\n\
         {owned_snippet}\
         printf '%s\\n' '{SENTINEL_NFT_JSON}'\n\
         nft -j list ruleset\n"
    );
    Ok(format!("sudo -n sh -ec {}", shell_quote(&script)))
}

fn build_apply_command(
    ruleset: &str,
    unit: &str,
    write_sysctl_dropin: bool,
    enable_runtime_ip_forward: bool,
    paths: &HostNatPaths,
) -> String {
    let rules_dir = paths
        .rules_path
        .parent()
        .unwrap_or_else(|| Path::new("/etc/outbe-up"));
    let service_dir = paths
        .service_path
        .parent()
        .unwrap_or_else(|| Path::new("/etc/systemd/system"));
    let sysctl_dir = paths
        .sysctl_path
        .parent()
        .unwrap_or_else(|| Path::new("/etc/sysctl.d"));

    let rules_dir_q = shell_quote(&rules_dir.to_string_lossy());
    let rules_path_q = shell_quote(&paths.rules_path.to_string_lossy());
    let service_dir_q = shell_quote(&service_dir.to_string_lossy());
    let service_path_q = shell_quote(&paths.service_path.to_string_lossy());
    let sysctl_dir_q = shell_quote(&sysctl_dir.to_string_lossy());
    let sysctl_path_q = shell_quote(&paths.sysctl_path.to_string_lossy());
    let marker_q = shell_quote(OWNERSHIP_MARKER);

    let mut script = format!(
        "check_owner() {{\n\
           if [ -e \"$1\" ] && ! grep -Fq -- {marker_q} \"$1\" 2>/dev/null; then\n\
             echo \"refusing to overwrite unmanaged file: $1\" >&2\n\
             exit 1\n\
           fi\n\
         }}\n\
         check_owner {rules_path_q}\n\
         check_owner {service_path_q}\n"
    );
    if write_sysctl_dropin {
        script.push_str(&format!("check_owner {sysctl_path_q}\n"));
    }

    // Load atomically via `nft -f -` before writing persistent files so a syntax/kernel
    // rejection leaves no partial files behind.
    script.push_str(&format!(
        "nft -f - <<'OUTBE_NAT_RULES_EOF'\n\
         {ruleset}OUTBE_NAT_RULES_EOF\n\
         mkdir -p {rules_dir_q} {service_dir_q}\n\
         tmp_rules=$(mktemp \"{}.tmp.XXXXXX\")\n\
         trap 'rm -f \"$tmp_rules\" \"${{tmp_unit:-}}\" \"${{tmp_sysctl:-}}\"' EXIT\n\
         cat <<'OUTBE_NAT_RULES_EOF' > \"$tmp_rules\"\n\
         {ruleset}OUTBE_NAT_RULES_EOF\n\
         chmod 0644 \"$tmp_rules\"\n\
         mv -f \"$tmp_rules\" {rules_path_q}\n",
        paths.rules_path.display()
    ));

    if write_sysctl_dropin {
        script.push_str(&format!(
            "mkdir -p {sysctl_dir_q}\n\
             tmp_sysctl=$(mktemp \"{}.tmp.XXXXXX\")\n\
             cat <<'OUTBE_NAT_SYSCTL_EOF' > \"$tmp_sysctl\"\n\
             {OWNERSHIP_MARKER}\n\
             net.ipv4.ip_forward = 1\n\
             OUTBE_NAT_SYSCTL_EOF\n\
             chmod 0644 \"$tmp_sysctl\"\n\
             mv -f \"$tmp_sysctl\" {sysctl_path_q}\n",
            paths.sysctl_path.display()
        ));
    }

    if enable_runtime_ip_forward {
        script.push_str("sysctl -w net.ipv4.ip_forward=1\n");
    }

    script.push_str(&format!(
        "tmp_unit=$(mktemp \"{}.tmp.XXXXXX\")\n\
         cat <<'OUTBE_NAT_UNIT_EOF' > \"$tmp_unit\"\n\
         {unit}OUTBE_NAT_UNIT_EOF\n\
         chmod 0644 \"$tmp_unit\"\n\
         mv -f \"$tmp_unit\" {service_path_q}\n\
         systemctl daemon-reload\n\
         systemctl enable {NAT_SERVICE_NAME}\n",
        paths.service_path.display()
    ));

    format!("sudo -n sh -ec {}", shell_quote(&script))
}

/// Remove only managed artifacts carrying `OWNERSHIP_MARKER` and our nftables table.
/// Never writes `net.ipv4.ip_forward=0` and never deletes foreign files.
fn build_remove_command(paths: &HostNatPaths) -> String {
    let rules_dir = paths
        .rules_path
        .parent()
        .unwrap_or_else(|| Path::new("/etc/outbe-up"));
    let rules_dir_q = shell_quote(&rules_dir.to_string_lossy());
    let rules_path_q = shell_quote(&paths.rules_path.to_string_lossy());
    let service_path_q = shell_quote(&paths.service_path.to_string_lossy());
    let sysctl_path_q = shell_quote(&paths.sysctl_path.to_string_lossy());
    let marker_q = shell_quote(OWNERSHIP_MARKER);

    let script = format!(
        "nft delete table {NAT_TABLE_FAMILY} {NAT_TABLE_NAME} 2>/dev/null || true\n\
         if [ -f {service_path_q} ]; then\n\
           if grep -Fq -- {marker_q} {service_path_q} 2>/dev/null; then\n\
             systemctl disable --now {NAT_SERVICE_NAME} 2>/dev/null || true\n\
             rm -f {service_path_q}\n\
             systemctl daemon-reload\n\
           else\n\
             echo \"skipping unmanaged service file: {}\" >&2\n\
           fi\n\
         fi\n\
         if [ -f {rules_path_q} ]; then\n\
           if grep -Fq -- {marker_q} {rules_path_q} 2>/dev/null; then\n\
             rm -f {rules_path_q}\n\
             rmdir {rules_dir_q} 2>/dev/null || true\n\
           else\n\
             echo \"skipping unmanaged ruleset file: {}\" >&2\n\
           fi\n\
         fi\n\
         if [ -f {sysctl_path_q} ]; then\n\
           if grep -Fq -- {marker_q} {sysctl_path_q} 2>/dev/null; then\n\
             rm -f {sysctl_path_q}\n\
           else\n\
             echo \"skipping unmanaged sysctl file: {}\" >&2\n\
           fi\n\
         fi\n",
        paths.service_path.display(),
        paths.rules_path.display(),
        paths.sysctl_path.display()
    );
    format!("sudo -n sh -ec {}", shell_quote(&script))
}

fn parse_preflight_output(
    raw: &str,
    bridge: &str,
    founder_ips: &[Ipv4Addr],
) -> Result<PreflightReport> {
    let bridge_section = extract_section(raw, SENTINEL_BRIDGE_ADDR, Some(SENTINEL_UPLINK_ROUTE))?;
    let uplink_section = extract_section(raw, SENTINEL_UPLINK_ROUTE, Some(SENTINEL_IP_FORWARD))?;
    let ip_fwd_section =
        extract_section(raw, SENTINEL_IP_FORWARD, Some(SENTINEL_PERSISTENT_SYSCTL))?;
    let persistent_section =
        extract_section(raw, SENTINEL_PERSISTENT_SYSCTL, Some(SENTINEL_OWNED_FILES))?;
    let owned_section = extract_section(raw, SENTINEL_OWNED_FILES, Some(SENTINEL_NFT_JSON))?;
    let nft_json_section = extract_section(raw, SENTINEL_NFT_JSON, None)?;

    let (bridge_addr, subnet) = parse_bridge_cidr(&bridge_section, bridge)?;
    for ip in founder_ips {
        ensure!(
            subnet.contains(*ip),
            "founder bridge_ip {ip} is outside host bridge `{bridge}` subnet {}",
            subnet.cidr_string()
        );
    }

    let uplink = parse_uplink_section(&uplink_section)?;
    validate_iface_name(&uplink, "uplink")?;
    ensure!(
        uplink != bridge,
        "detected uplink '{uplink}' is the same interface as bridge '{bridge}'; pass --uplink"
    );

    let ip_forward: u8 = ip_fwd_section.trim().parse().wrap_err_with(|| {
        format!(
            "invalid net.ipv4.ip_forward value: {:?}",
            ip_fwd_section.trim()
        )
    })?;
    ensure!(
        ip_forward == 0 || ip_forward == 1,
        "unexpected net.ipv4.ip_forward value: {ip_forward}"
    );

    let persistent_sysctl = parse_persistent_sysctl(&persistent_section, "90-outbe-up-nat.conf");
    let files = parse_owned_files_section(&owned_section)?;
    let nft = inspect_nft_json(&nft_json_section, bridge, subnet, &uplink)?;

    Ok(PreflightReport {
        bridge: bridge.to_owned(),
        bridge_addr,
        subnet,
        uplink,
        ip_forward,
        persistent_sysctl,
        files,
        nft,
    })
}

fn parse_owned_files_section(section: &str) -> Result<ManagedFileStates> {
    let mut rules = None;
    let mut service = None;
    let mut sysctl = None;
    for line in section.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some((key, val)) = line.split_once('=') {
            let state = match val {
                "absent" => FileOwnership::Absent,
                "owned" => FileOwnership::Owned,
                "foreign" => FileOwnership::Foreign,
                other => {
                    bail!("unexpected file ownership state '{other}' for '{key}'");
                }
            };
            match key {
                "rules" => rules = Some(state),
                "service" => service = Some(state),
                "sysctl" => sysctl = Some(state),
                _ => {}
            }
        }
    }
    Ok(ManagedFileStates {
        rules: rules.ok_or_else(|| eyre::eyre!("missing rules ownership probe"))?,
        service: service.ok_or_else(|| eyre::eyre!("missing service ownership probe"))?,
        sysctl: sysctl.ok_or_else(|| eyre::eyre!("missing sysctl ownership probe"))?,
    })
}

/// Compute the effective persistent `net.ipv4.ip_forward` value following procps `sysctl --system`
/// and `systemd-sysctl` precedence:
/// 1. Whole-file same-basename masking across `/etc/sysctl.d` > `/run/sysctl.d` >
///    `/usr/local/lib/sysctl.d` > `/usr/lib/sysctl.d` > `/lib/sysctl.d` (a higher-priority file
///    without `net.ipv4.ip_forward` or symlinked to `/dev/null` hides lower-priority files of
///    the same basename).
/// 2. Winning basenames are applied in lexicographical order.
/// 3. `/etc/sysctl.conf` is applied last (matching procps-ng `sysctl --system` on Ubuntu).
fn parse_persistent_sysctl(section: &str, our_dropin_basename: &str) -> PersistentSysctlState {
    // Map basename -> (dir_idx, full_path) where smaller dir_idx = higher priority.
    let mut winner_by_basename: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut sysctl_conf_path: Option<String> = None;
    let mut assignments_by_file: BTreeMap<String, u8> = BTreeMap::new();

    for raw_line in section.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(rest) = raw_line.strip_prefix("FILE ") {
            let Some((idx_str, path_str)) = rest.split_once(' ') else {
                continue;
            };
            let Ok(dir_idx) = idx_str.parse::<usize>() else {
                continue;
            };
            let Some(basename) = Path::new(path_str).file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            match winner_by_basename.get(basename) {
                Some((existing_idx, _)) if *existing_idx <= dir_idx => {}
                _ => {
                    winner_by_basename.insert(basename.to_owned(), (dir_idx, path_str.to_owned()));
                }
            }
        } else if let Some(path_str) = raw_line.strip_prefix("CONF ") {
            sysctl_conf_path = Some(path_str.trim().to_owned());
        } else if let Some(rest) = raw_line.strip_prefix("SET ") {
            let Some((path_str, rhs)) = rest.split_once(':') else {
                continue;
            };
            let Some((_lhs, val_part)) = rhs.split_once('=') else {
                continue;
            };
            let val_token = val_part.split(['#', ';']).next().unwrap_or("").trim();
            let Ok(val) = val_token.parse::<u8>() else {
                continue;
            };
            if val == 0 || val == 1 {
                assignments_by_file.insert(path_str.to_owned(), val);
            }
        }
    }

    let mut effective_value = None;
    let mut effective_source = None;
    let mut effective_is_after_dropin = false;

    for (basename, (_idx, winning_path)) in &winner_by_basename {
        if let Some(&val) = assignments_by_file.get(winning_path) {
            effective_value = Some(val);
            effective_source = Some(winning_path.clone());
            effective_is_after_dropin = basename.as_str() > our_dropin_basename;
        }
    }

    if let Some(conf_path) = sysctl_conf_path {
        if let Some(&val) = assignments_by_file.get(&conf_path) {
            effective_value = Some(val);
            effective_source = Some(conf_path);
            effective_is_after_dropin = true;
        }
    }

    let overridden_by_later_zero = effective_value == Some(0) && effective_is_after_dropin;

    PersistentSysctlState {
        effective_value,
        effective_source,
        overridden_by_later_zero,
    }
}

fn extract_section(raw: &str, start_sentinel: &str, end_sentinel: Option<&str>) -> Result<String> {
    let mut lines = raw.lines();
    let mut found = false;
    for line in &mut lines {
        if line.trim() == start_sentinel {
            found = true;
            break;
        }
    }
    ensure!(found, "preflight output missing section {start_sentinel}");
    let mut body = Vec::new();
    for line in lines {
        if let Some(end) = end_sentinel {
            if line.trim() == end {
                break;
            }
        }
        body.push(line);
    }
    Ok(body.join("\n"))
}

/// Parse `ip -4 -o addr show dev <bridge>` output into `(host_addr, normalized_subnet)`.
pub fn parse_bridge_cidr(output: &str, bridge: &str) -> Result<(Ipv4Addr, Ipv4Subnet)> {
    for line in output.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if let Some(pos) = tokens.iter().position(|&tok| tok == "inet") {
            if let Some(cidr) = tokens.get(pos + 1) {
                let (addr_str, prefix_str) = cidr
                    .split_once('/')
                    .ok_or_else(|| eyre::eyre!("missing CIDR prefix on bridge {bridge}: {cidr}"))?;
                let addr: Ipv4Addr = addr_str.parse().wrap_err_with(|| {
                    format!("invalid IPv4 address on bridge {bridge}: {addr_str}")
                })?;
                let prefix: u8 = prefix_str.parse().wrap_err_with(|| {
                    format!("invalid CIDR prefix on bridge {bridge}: {prefix_str}")
                })?;
                let subnet = Ipv4Subnet::from_addr_prefix(addr, prefix)?;
                return Ok((addr, subnet));
            }
        }
    }
    bail!("no IPv4 address found on bridge interface `{bridge}`");
}

/// Parse either `override <iface>` or `ip -4 -o route show default` output.
pub fn parse_uplink_section(output: &str) -> Result<String> {
    for line in output.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(iface) = line.strip_prefix("override ") {
            return Ok(iface.trim().to_owned());
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if let Some(pos) = tokens.iter().position(|&tok| tok == "dev") {
            if let Some(iface) = tokens.get(pos + 1) {
                return Ok((*iface).to_owned());
            }
        }
    }
    bail!("could not detect IPv4 default-route uplink interface; pass --uplink <iface>");
}

/// Parse structured `nft -j list ruleset` JSON output.
///
/// Classifies every foreign `ip`/`inet` `forward` base chain into `PureAccept` vs
/// `BlocksOrUnknown` (policy drop OR any rule containing `drop`, `reject`, `jump`,
/// `goto`, or `vmap`), and detects existing foreign `postrouting` masquerade/snat
/// rules that already provide egress for `subnet` towards `uplink`.
pub fn inspect_nft_json(
    json_str: &str,
    bridge: &str,
    subnet: Ipv4Subnet,
    uplink: &str,
) -> Result<NftInspection> {
    let root: Value = serde_json::from_str(json_str.trim())
        .wrap_err("parse `nft -j list ruleset` JSON output")?;
    let items = root
        .get("nftables")
        .and_then(Value::as_array)
        .ok_or_else(|| eyre::eyre!("`nft -j list ruleset` JSON missing `nftables` array"))?;

    #[derive(Clone)]
    struct ChainMeta {
        family: String,
        table: String,
        chain: String,
        hook: Option<String>,
        priority: String,
        policy: String,
    }

    let mut chains: BTreeMap<(String, String, String), ChainMeta> = BTreeMap::new();
    let mut rules_by_chain: BTreeMap<(String, String, String), Vec<&Value>> = BTreeMap::new();
    let mut docker_or_libvirt = BTreeSet::new();

    for item in items {
        if let Some(chain) = item.get("chain").and_then(Value::as_object) {
            let Some(family) = chain.get("family").and_then(Value::as_str) else {
                continue;
            };
            let Some(table) = chain.get("table").and_then(Value::as_str) else {
                continue;
            };
            let Some(name) = chain.get("name").and_then(Value::as_str) else {
                continue;
            };
            if is_docker_or_libvirt_chain(name) || (table == "filter" && name == "FORWARD") {
                docker_or_libvirt.insert(format!("{family} {table}::{name}"));
            }
            let hook = chain.get("hook").and_then(Value::as_str).map(str::to_owned);
            let priority = chain
                .get("prio")
                .map(|v| match v {
                    Value::Number(n) => n.to_string(),
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_else(|| "unknown".to_owned());
            let policy = chain
                .get("policy")
                .and_then(Value::as_str)
                .unwrap_or("accept")
                .to_owned();
            chains.insert(
                (family.to_owned(), table.to_owned(), name.to_owned()),
                ChainMeta {
                    family: family.to_owned(),
                    table: table.to_owned(),
                    chain: name.to_owned(),
                    hook,
                    priority,
                    policy,
                },
            );
        } else if let Some(rule) = item.get("rule").and_then(Value::as_object) {
            let Some(family) = rule.get("family").and_then(Value::as_str) else {
                continue;
            };
            let Some(table) = rule.get("table").and_then(Value::as_str) else {
                continue;
            };
            let Some(chain) = rule.get("chain").and_then(Value::as_str) else {
                continue;
            };
            rules_by_chain
                .entry((family.to_owned(), table.to_owned(), chain.to_owned()))
                .or_default()
                .push(item);
        }
    }

    let mut foreign_forward_chains = Vec::new();
    let mut blocks_or_unknown_chains = Vec::new();
    let mut existing_egress = Vec::new();
    let mut partial_egress = Vec::new();
    let mut seen_egress = BTreeSet::new();

    for (key, meta) in &chains {
        let is_own_table = meta.family == NAT_TABLE_FAMILY && meta.table == NAT_TABLE_NAME;
        if is_own_table || !matches!(meta.family.as_str(), "ip" | "inet") {
            continue;
        }
        let rules = rules_by_chain.get(key).map(Vec::as_slice).unwrap_or(&[]);

        if meta.hook.as_deref() == Some("forward") {
            let mut reasons = BTreeSet::new();
            if meta.policy != "accept" {
                reasons.insert(format!("policy {}", meta.policy));
            }
            for rule_item in rules {
                if let Some(exprs) = rule_item
                    .get("rule")
                    .and_then(|r| r.get("expr"))
                    .and_then(Value::as_array)
                {
                    collect_non_accept_verdicts(exprs, &mut reasons);
                }
            }
            let class = if reasons.is_empty() {
                ForwardChainClass::PureAccept
            } else {
                ForwardChainClass::BlocksOrUnknown
            };
            let info = ForeignForwardChain {
                family: meta.family.clone(),
                table: meta.table.clone(),
                chain: meta.chain.clone(),
                priority: meta.priority.clone(),
                policy: meta.policy.clone(),
                class,
                reasons: reasons.into_iter().collect(),
            };
            if class == ForwardChainClass::BlocksOrUnknown {
                blocks_or_unknown_chains.push(info.clone());
            }
            foreign_forward_chains.push(info);
        } else if meta.hook.as_deref() == Some("postrouting") {
            for rule_item in rules {
                if let Some(exprs) = rule_item
                    .get("rule")
                    .and_then(|r| r.get("expr"))
                    .and_then(Value::as_array)
                {
                    match rule_provides_egress(exprs, bridge, subnet, uplink) {
                        EgressMatch::Full(kind) => {
                            let dedup_key =
                                (meta.family.clone(), meta.table.clone(), meta.chain.clone());
                            if seen_egress.insert(dedup_key) {
                                existing_egress.push(ExistingEgress {
                                    family: meta.family.clone(),
                                    table: meta.table.clone(),
                                    chain: meta.chain.clone(),
                                    kind: kind.to_owned(),
                                });
                            }
                        }
                        EgressMatch::Partial(kind, narrowing) => {
                            partial_egress.push(PartialEgress {
                                family: meta.family.clone(),
                                table: meta.table.clone(),
                                chain: meta.chain.clone(),
                                kind: kind.to_owned(),
                                narrowing,
                            });
                        }
                        EgressMatch::None => {}
                    }
                }
            }
        }
    }

    Ok(NftInspection {
        foreign_forward_chains,
        blocks_or_unknown_chains,
        existing_egress,
        partial_egress,
        docker_or_libvirt_chains: docker_or_libvirt.into_iter().collect(),
    })
}

fn collect_non_accept_verdicts(exprs: &[Value], reasons: &mut BTreeSet<String>) {
    for expr in exprs {
        collect_non_accept_verdicts_value(expr, reasons);
    }
}

fn collect_non_accept_verdicts_value(val: &Value, reasons: &mut BTreeSet<String>) {
    match val {
        Value::Object(map) => {
            for (k, v) in map {
                match k.as_str() {
                    "drop" => {
                        reasons.insert("drop rule".to_owned());
                    }
                    "reject" => {
                        reasons.insert("reject rule".to_owned());
                    }
                    "jump" => {
                        let target = v.get("target").and_then(Value::as_str).unwrap_or("unknown");
                        reasons.insert(format!("jump to {target}"));
                    }
                    "goto" => {
                        let target = v.get("target").and_then(Value::as_str).unwrap_or("unknown");
                        reasons.insert(format!("goto {target}"));
                    }
                    "vmap" => {
                        reasons.insert("vmap verdict".to_owned());
                    }
                    _ => collect_non_accept_verdicts_value(v, reasons),
                }
            }
        }
        Value::Array(arr) => {
            for item in arr {
                collect_non_accept_verdicts_value(item, reasons);
            }
        }
        _ => {}
    }
}

enum EgressMatch {
    None,
    Full(&'static str),
    Partial(&'static str, Vec<String>),
}

/// Check whether a foreign `postrouting` rule provides full or partial masquerade/SNAT
/// for `subnet` towards `uplink`.
///
/// Only rules with no extra narrowing expressions (beyond bridge source, uplink oifname,
/// or non-bridge destination, plus benign `counter`/`log`) count as `Full`. Any extra
/// match (L4 protocol, ports, marks, ct state, specific daddr, limits, etc.) is classified
/// as `Partial` so apply never silently no-ops on incomplete egress.
fn rule_provides_egress(
    exprs: &[Value],
    bridge: &str,
    subnet: Ipv4Subnet,
    uplink: &str,
) -> EgressMatch {
    let mut nat_kind: Option<&'static str> = None;
    let mut matches_bridge_source = false;
    let mut narrowing = Vec::new();

    for expr in exprs {
        if expr.get("masquerade").is_some() {
            nat_kind = Some("masquerade");
            continue;
        }
        if expr.get("snat").is_some() {
            nat_kind = Some("snat");
            continue;
        }
        if expr.get("counter").is_some() || expr.get("log").is_some() {
            continue;
        }
        let Some(m) = expr.get("match").and_then(Value::as_object) else {
            let key = expr
                .as_object()
                .and_then(|o| o.keys().next())
                .map(String::as_str)
                .unwrap_or("expr");
            narrowing.push(key.to_owned());
            continue;
        };
        let op = m.get("op").and_then(Value::as_str).unwrap_or("==");
        let Some(left) = m.get("left") else {
            narrowing.push("match".to_owned());
            continue;
        };
        let Some(right) = m.get("right") else {
            narrowing.push("match".to_owned());
            continue;
        };

        if let Some(payload) = left.get("payload").and_then(Value::as_object) {
            let proto = payload.get("protocol").and_then(Value::as_str);
            let field = payload.get("field").and_then(Value::as_str);
            if proto == Some("ip") && field == Some("saddr") {
                let prefixes = extract_ipv4_prefixes(right);
                match op {
                    "==" => {
                        if prefixes.iter().any(|p| p.covers(subnet)) {
                            matches_bridge_source = true;
                        } else {
                            return EgressMatch::None;
                        }
                    }
                    "!=" if prefixes.iter().any(|p| p.covers(subnet)) => {
                        return EgressMatch::None;
                    }
                    _ => narrowing.push(format!("match ip.saddr {op}")),
                }
            } else if proto == Some("ip") && field == Some("daddr") {
                let prefixes = extract_ipv4_prefixes(right);
                if op == "==" && prefixes.iter().any(|p| p.covers(subnet)) {
                    // Hairpin rule targeting the bridge subnet itself: not internet egress.
                    return EgressMatch::None;
                } else if op == "!="
                    && prefixes.iter().all(|p| subnet.covers(*p))
                    && !prefixes.is_empty()
                {
                    // Excluding the bridge subnet itself (`ip daddr != 10.77.0.0/24`) still covers internet egress.
                } else {
                    narrowing.push(format!("match ip.daddr {op}"));
                }
            } else {
                narrowing.push(format!(
                    "match {}.{}",
                    proto.unwrap_or("payload"),
                    field.unwrap_or("?")
                ));
            }
        } else if let Some(meta) = left.get("meta").and_then(Value::as_object) {
            let key = meta.get("key").and_then(Value::as_str);
            if key == Some("iifname") {
                let ifaces = extract_string_values(right);
                match op {
                    "==" => {
                        if ifaces.iter().any(|s| s == bridge) {
                            matches_bridge_source = true;
                        } else {
                            return EgressMatch::None;
                        }
                    }
                    "!=" if ifaces.iter().any(|s| s == bridge) => {
                        return EgressMatch::None;
                    }
                    _ => narrowing.push(format!("match meta.iifname {op}")),
                }
            } else if key == Some("oifname") {
                let ifaces = extract_string_values(right);
                match op {
                    "==" => {
                        if !ifaces.iter().any(|s| s == uplink) {
                            return EgressMatch::None;
                        }
                    }
                    "!=" if ifaces.iter().any(|s| s == uplink) => {
                        return EgressMatch::None;
                    }
                    "!=" if ifaces.iter().all(|s| s == bridge || s == "lo")
                        && !ifaces.is_empty() => {}
                    _ => narrowing.push(format!("match meta.oifname {op}")),
                }
            } else {
                narrowing.push(format!("match meta.{}", key.unwrap_or("?")));
            }
        } else {
            let left_kind = left
                .as_object()
                .and_then(|o| o.keys().next())
                .map(String::as_str)
                .unwrap_or("expr");
            narrowing.push(format!("match {left_kind}"));
        }
    }

    match (matches_bridge_source, nat_kind) {
        (true, Some(kind)) if narrowing.is_empty() => EgressMatch::Full(kind),
        (true, Some(kind)) => EgressMatch::Partial(kind, narrowing),
        _ => EgressMatch::None,
    }
}

fn extract_ipv4_prefixes(val: &Value) -> Vec<Ipv4Subnet> {
    let mut out = Vec::new();
    match val {
        Value::Object(map) => {
            if let Some(prefix) = map.get("prefix").and_then(Value::as_object) {
                if let (Some(addr_s), Some(len)) = (
                    prefix.get("addr").and_then(Value::as_str),
                    prefix.get("len").and_then(Value::as_u64),
                ) {
                    if let (Ok(addr), Ok(len_u8)) = (addr_s.parse::<Ipv4Addr>(), u8::try_from(len))
                    {
                        if let Some(sub) = Ipv4Subnet::from_nft_prefix(addr, len_u8) {
                            out.push(sub);
                        }
                    }
                }
            } else if let Some(set) = map.get("set") {
                out.extend(extract_ipv4_prefixes(set));
            }
        }
        Value::Array(arr) => {
            for item in arr {
                out.extend(extract_ipv4_prefixes(item));
            }
        }
        Value::String(s) => {
            if let Some((addr_s, len_s)) = s.split_once('/') {
                if let (Ok(addr), Ok(len_u8)) = (addr_s.parse::<Ipv4Addr>(), len_s.parse::<u8>()) {
                    if let Some(sub) = Ipv4Subnet::from_nft_prefix(addr, len_u8) {
                        out.push(sub);
                    }
                }
            } else if let Ok(addr) = s.parse::<Ipv4Addr>() {
                if let Some(sub) = Ipv4Subnet::from_nft_prefix(addr, 32) {
                    out.push(sub);
                }
            }
        }
        _ => {}
    }
    out
}

fn extract_string_values(val: &Value) -> Vec<String> {
    let mut out = Vec::new();
    match val {
        Value::String(s) => out.push(s.clone()),
        Value::Array(arr) => {
            for item in arr {
                out.extend(extract_string_values(item));
            }
        }
        Value::Object(map) => {
            if let Some(set) = map.get("set") {
                out.extend(extract_string_values(set));
            }
        }
        _ => {}
    }
    out
}

fn is_docker_or_libvirt_chain(chain: &str) -> bool {
    chain.starts_with("DOCKER") || chain.starts_with("LIBVIRT")
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};

    use super::*;

    #[test]
    fn validate_iface_name_accepts_valid_and_rejects_injection() {
        for ok in [
            "obr0",
            "eth0",
            "enp1s0f0",
            "ens3.100",
            "br-01_a",
            "a12345678901234",
        ] {
            assert!(
                validate_iface_name(ok, "test").is_ok(),
                "should accept {ok}"
            );
        }
        for bad in [
            "",
            "a123456789012345",
            "-obr0",
            ".obr0",
            "obr0; rm -rf /",
            "obr0\" drop",
            "obr0\neth0",
            "obr0*",
            "obr0/1",
            "$(id)",
        ] {
            assert!(
                validate_iface_name(bad, "test").is_err(),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn parse_bridge_cidr_and_subnet_membership() {
        let sample = "4: obr0    inet 10.77.0.1/24 scope global obr0\\       valid_lft forever preferred_lft forever\n";
        let (addr, subnet) = parse_bridge_cidr(sample, "obr0").unwrap();
        assert_eq!(addr, Ipv4Addr::new(10, 77, 0, 1));
        assert_eq!(subnet.cidr_string(), "10.77.0.0/24");
        assert!(subnet.contains(Ipv4Addr::new(10, 77, 0, 68)));
        assert!(!subnet.contains(Ipv4Addr::new(10, 77, 1, 68)));
    }

    #[test]
    fn parse_uplink_section_detects_default_route_and_override() {
        let route = "default via 162.19.107.254 dev eno1 proto static\n";
        assert_eq!(parse_uplink_section(route).unwrap(), "eno1");
        assert_eq!(parse_uplink_section("override enp2s0\n").unwrap(), "enp2s0");
        assert!(parse_uplink_section("").is_err());
    }

    #[test]
    fn render_nat_ruleset_is_atomic_has_marker_and_rejects_invalid_inputs() {
        let subnet = Ipv4Subnet::from_addr_prefix(Ipv4Addr::new(10, 77, 0, 1), 24).unwrap();
        let rules = render_nat_ruleset("obr0", subnet, "eno1").unwrap();
        assert!(rules.starts_with(&format!(
            "{OWNERSHIP_MARKER}\ntable ip outbe_up_nat\ndelete table ip outbe_up_nat\n"
        )));
        assert!(
            rules.contains("iifname \"obr0\" oifname \"eno1\" ip saddr 10.77.0.0/24 masquerade")
        );
        assert!(rules.contains("iifname \"obr0\" oifname \"eno1\" ip saddr 10.77.0.0/24 accept"));
        assert!(rules.contains(
            "iifname \"eno1\" oifname \"obr0\" ip daddr 10.77.0.0/24 ct state established,related accept"
        ));
        assert!(!rules.contains("flush ruleset"));
        assert!(!rules.contains("policy drop"));
        assert!(!rules.contains("hook input"));

        assert!(render_nat_ruleset("obr0", subnet, "obr0").is_err());
        assert!(render_nat_ruleset("obr0;drop", subnet, "eno1").is_err());
        assert!(render_nat_ruleset("obr0", subnet, "eno1\" bad").is_err());
    }

    #[test]
    fn json_inspect_classifies_policy_accept_with_explicit_drop_as_blocks_or_unknown() {
        let subnet = Ipv4Subnet::from_addr_prefix(Ipv4Addr::new(10, 77, 0, 1), 24).unwrap();
        let json = r#"{
          "nftables": [
            {"metainfo": {"json_schema_version": 1}},
            {"table": {"family": "inet", "name": "firewall"}},
            {"chain": {"family": "inet", "table": "firewall", "name": "forward", "type": "filter", "hook": "forward", "prio": 10, "policy": "accept"}},
            {"rule": {"family": "inet", "table": "firewall", "chain": "forward", "expr": [
              {"match": {"op": "==", "left": {"meta": {"key": "iifname"}}, "right": "obr0"}},
              {"drop": null}
            ]}}
          ]
        }"#;
        let res = inspect_nft_json(json, "obr0", subnet, "eno1").unwrap();
        assert_eq!(res.foreign_forward_chains.len(), 1);
        assert_eq!(res.blocks_or_unknown_chains.len(), 1);
        assert_eq!(
            res.blocks_or_unknown_chains[0].class,
            ForwardChainClass::BlocksOrUnknown
        );
        assert!(res.blocks_or_unknown_chains[0]
            .reasons
            .iter()
            .any(|r| r.contains("drop")));
    }

    #[test]
    fn json_inspect_finds_policy_drop_forward_chain_after_brace_in_comment() {
        let subnet = Ipv4Subnet::from_addr_prefix(Ipv4Addr::new(10, 77, 0, 1), 24).unwrap();
        let json = r#"{
          "nftables": [
            {"metainfo": {"json_schema_version": 1}},
            {"table": {"family": "inet", "name": "firewall"}},
            {"chain": {"family": "inet", "table": "firewall", "name": "input", "type": "filter", "hook": "input", "prio": 0, "policy": "accept"}},
            {"rule": {"family": "inet", "table": "firewall", "chain": "input", "comment": "}", "expr": [{"accept": null}]}},
            {"chain": {"family": "inet", "table": "firewall", "name": "forward", "type": "filter", "hook": "forward", "prio": 0, "policy": "drop"}}
          ]
        }"#;
        let res = inspect_nft_json(json, "obr0", subnet, "eno1").unwrap();
        assert_eq!(res.foreign_forward_chains.len(), 1);
        assert_eq!(res.blocks_or_unknown_chains.len(), 1);
        assert_eq!(res.blocks_or_unknown_chains[0].table, "firewall");
        assert_eq!(res.blocks_or_unknown_chains[0].chain, "forward");
        assert_eq!(res.blocks_or_unknown_chains[0].policy, "drop");
    }

    #[test]
    fn json_inspect_detects_existing_foreign_masquerade_and_ignores_hairpin() {
        let subnet = Ipv4Subnet::from_addr_prefix(Ipv4Addr::new(10, 77, 0, 1), 24).unwrap();
        let json = r#"{
          "nftables": [
            {"metainfo": {"json_schema_version": 1}},
            {"table": {"family": "ip", "name": "outbe_mvp"}},
            {"chain": {"family": "ip", "table": "outbe_mvp", "name": "postrouting", "type": "nat", "hook": "postrouting", "prio": 100, "policy": "accept"}},
            {"rule": {"family": "ip", "table": "outbe_mvp", "chain": "postrouting", "expr": [
              {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
              {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
              {"snat": {"addr": "162.19.107.121"}}
            ]}},
            {"rule": {"family": "ip", "table": "outbe_mvp", "chain": "postrouting", "expr": [
              {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
              {"match": {"op": "==", "left": {"meta": {"key": "oifname"}}, "right": "eno1"}},
              {"snat": {"addr": "162.19.107.121"}}
            ]}}
          ]
        }"#;
        let res = inspect_nft_json(json, "obr0", subnet, "eno1").unwrap();
        assert_eq!(res.existing_egress.len(), 1);
        assert_eq!(res.existing_egress[0].family, "ip");
        assert_eq!(res.existing_egress[0].table, "outbe_mvp");
        assert_eq!(res.existing_egress[0].chain, "postrouting");
    }

    #[test]
    fn parse_persistent_sysctl_respects_directory_precedence_masking_and_sysctl_conf() {
        // 1. /etc/sysctl.d overrides /usr/lib/sysctl.d for the same basename.
        let lines = "\
FILE 0 /etc/sysctl.d/50-default.conf
SET /etc/sysctl.d/50-default.conf:net.ipv4.ip_forward = 1
FILE 3 /usr/lib/sysctl.d/50-default.conf
SET /usr/lib/sysctl.d/50-default.conf:net.ipv4.ip_forward = 0
";
        let state = parse_persistent_sysctl(lines, "90-outbe-up-nat.conf");
        assert_eq!(state.effective_value, Some(1));
        assert_eq!(
            state.effective_source.as_deref(),
            Some("/etc/sysctl.d/50-default.conf")
        );
        assert!(!state.overridden_by_later_zero);

        // 2. Whole-file mask without ip_forward key in /etc hides /usr/lib setting of 1.
        let masked = "\
FILE 0 /etc/sysctl.d/50-vendor.conf
FILE 3 /usr/lib/sysctl.d/50-vendor.conf
SET /usr/lib/sysctl.d/50-vendor.conf:net.ipv4.ip_forward = 1
";
        let state_masked = parse_persistent_sysctl(masked, "90-outbe-up-nat.conf");
        assert_eq!(state_masked.effective_value, None);
        assert!(!state_masked.overridden_by_later_zero);

        // 3. Later 99-admin.conf = 0 overrides 90-outbe-up-nat.conf.
        let later_zero = "\
FILE 0 /etc/sysctl.d/99-admin.conf
SET /etc/sysctl.d/99-admin.conf:net.ipv4.ip_forward = 0
";
        let state_later = parse_persistent_sysctl(later_zero, "90-outbe-up-nat.conf");
        assert_eq!(state_later.effective_value, Some(0));
        assert_eq!(
            state_later.effective_source.as_deref(),
            Some("/etc/sysctl.d/99-admin.conf")
        );
        assert!(state_later.overridden_by_later_zero);

        // 4. /etc/sysctl.conf = 0 is applied last by procps `sysctl --system` and overrides 90-.
        let conf_zero = "\
FILE 0 /etc/sysctl.d/90-vm-host.conf
SET /etc/sysctl.d/90-vm-host.conf:net.ipv4.ip_forward = 1
CONF /etc/sysctl.conf
SET /etc/sysctl.conf:net.ipv4.ip_forward = 0
";
        let state_conf = parse_persistent_sysctl(conf_zero, "90-outbe-up-nat.conf");
        assert_eq!(state_conf.effective_value, Some(0));
        assert_eq!(
            state_conf.effective_source.as_deref(),
            Some("/etc/sysctl.conf")
        );
        assert!(state_conf.overridden_by_later_zero);

        let empty = parse_persistent_sysctl("", "90-outbe-up-nat.conf");
        assert_eq!(empty.effective_value, None);
        assert!(!empty.overridden_by_later_zero);
    }

    struct StubHostRunner {
        bin_dir: PathBuf,
    }

    impl HostRunner for StubHostRunner {
        fn run_raw(&mut self, _ssh: &Ssh, _host: &str, command: &str) -> Result<String> {
            let base_path = std::env::var("PATH").unwrap_or_default();
            let path_env = format!("{}:{base_path}", self.bin_dir.display());
            let out = Command::new("sh")
                .arg("-c")
                .arg(command)
                .env("PATH", path_env)
                .output()
                .wrap_err("run stubbed command")?;
            if !out.status.success() {
                bail!(
                    "stubbed command failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        }
    }

    fn write_shim(bin_dir: &Path, name: &str, body: &str) {
        let path = bin_dir.join(name);
        fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    struct StubEnv {
        _temp: tempfile::TempDir,
        bin_dir: PathBuf,
        etc_dir: PathBuf,
        log_file: PathBuf,
        ip_fwd_file: PathBuf,
        nat_table_file: PathBuf,
        paths: HostNatPaths,
        target: HostTarget,
    }

    impl StubEnv {
        fn new(initial_ip_forward: &str, initial_nft_json: &str) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let bin_dir = temp.path().join("bin");
            let state_dir = temp.path().join("state");
            let etc_dir = temp.path().join("etc");
            fs::create_dir_all(&bin_dir).unwrap();
            fs::create_dir_all(&state_dir).unwrap();
            fs::create_dir_all(&etc_dir).unwrap();

            let log_file = state_dir.join("calls.log");
            let ip_fwd_file = state_dir.join("ip_forward");
            let nft_json_file = state_dir.join("nft.json");
            let nat_table_file = state_dir.join("nat_table");

            fs::write(&log_file, "").unwrap();
            fs::write(&ip_fwd_file, format!("{initial_ip_forward}\n")).unwrap();
            fs::write(&nft_json_file, initial_nft_json).unwrap();

            let log_q = shell_quote(&log_file.to_string_lossy());
            let ip_fwd_q = shell_quote(&ip_fwd_file.to_string_lossy());
            let nft_json_q = shell_quote(&nft_json_file.to_string_lossy());
            let nat_table_q = shell_quote(&nat_table_file.to_string_lossy());

            write_shim(
                &bin_dir,
                "sudo",
                "if [ \"${1:-}\" = \"-n\" ]; then shift; fi\nexec \"$@\"",
            );
            write_shim(
                &bin_dir,
                "ip",
                &format!(
                    "echo \"ip $*\" >> {log_q}\n\
                     case \"$*\" in\n\
                       *\"addr show dev obr0\"*) echo '4: obr0 inet 10.77.0.1/24 scope global obr0' ;;\n\
                       *\"route show default\"*) echo 'default via 162.19.107.254 dev eno1 proto static' ;;\n\
                       *\"link show dev\"*) exit 0 ;;\n\
                       *) exit 1 ;;\n\
                     esac"
                ),
            );
            write_shim(
                &bin_dir,
                "sysctl",
                &format!(
                    "echo \"sysctl $*\" >> {log_q}\n\
                     case \"$1\" in\n\
                       -n) cat {ip_fwd_q} ;;\n\
                       -w) val=${{2#*=}}; printf '%s\\n' \"$val\" > {ip_fwd_q} ;;\n\
                       *) exit 1 ;;\n\
                     esac"
                ),
            );
            write_shim(
                &bin_dir,
                "systemctl",
                &format!("echo \"systemctl $*\" >> {log_q}"),
            );
            write_shim(
                &bin_dir,
                "nft",
                &format!(
                    "echo \"nft $*\" >> {log_q}\n\
                     case \"$1\" in\n\
                       -j) cat {nft_json_q} ;;\n\
                       list) [ -f {nat_table_q} ] && cat {nat_table_q} || echo 'table ip outbe_up_nat {{}}' ;;\n\
                       -f) if [ \"$2\" = \"-\" ]; then cat > {nat_table_q}; else cp \"$2\" {nat_table_q}; fi ;;\n\
                       delete) rm -f {nat_table_q} ;;\n\
                       *) exit 1 ;;\n\
                     esac"
                ),
            );

            let paths = HostNatPaths {
                rules_path: etc_dir.join("outbe-up/nat.nft"),
                service_path: etc_dir.join("systemd/system/outbe-up-nat.service"),
                sysctl_path: etc_dir.join("sysctl.d/90-outbe-up-nat.conf"),
                sysctl_scan_dirs: vec![
                    etc_dir.join("sysctl.d"),
                    temp.path().join("usr/lib/sysctl.d"),
                ],
                sysctl_conf_path: etc_dir.join("sysctl.conf"),
            };
            let target = HostTarget {
                ssh: Ssh {
                    host: "vmm-test".to_owned(),
                    user: "ubuntu".to_owned(),
                    port: 22,
                },
                bridge: "obr0".to_owned(),
                founder_ips: vec![Ipv4Addr::new(10, 77, 0, 68), Ipv4Addr::new(10, 77, 0, 69)],
            };

            Self {
                _temp: temp,
                bin_dir,
                etc_dir,
                log_file,
                ip_fwd_file,
                nat_table_file,
                paths,
                target,
            }
        }

        fn runner(&self) -> StubHostRunner {
            StubHostRunner {
                bin_dir: self.bin_dir.clone(),
            }
        }

        fn calls(&self) -> String {
            fs::read_to_string(&self.log_file).unwrap()
        }
    }

    #[test]
    fn orchestration_existing_foreign_masquerade_is_noop_unless_forced() {
        let foreign_masq = r#"{
          "nftables": [
            {"metainfo": {"json_schema_version": 1}},
            {"table": {"family": "ip", "name": "nat"}},
            {"chain": {"family": "ip", "table": "nat", "name": "POSTROUTING", "type": "nat", "hook": "postrouting", "prio": 100, "policy": "accept"}},
            {"rule": {"family": "ip", "table": "nat", "chain": "POSTROUTING", "expr": [
              {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
              {"match": {"op": "==", "left": {"meta": {"key": "oifname"}}, "right": "eno1"}},
              {"masquerade": null}
            ]}}
          ]
        }"#;
        let env = StubEnv::new("1", foreign_masq);
        let mut runner = env.runner();
        let mut progress = Progress::new();

        let outcome = run_apply_on_host(
            &env.target,
            &mut runner,
            None,
            &env.paths,
            false,
            false,
            Path::new("/tmp/artifacts"),
            &mut progress,
        )
        .unwrap();
        assert_eq!(outcome, ApplyOutcome::NoOpExistingEgress);
        assert!(!env.paths.rules_path.exists());
        assert!(!env.paths.service_path.exists());
        assert!(!env.paths.sysctl_path.exists());
        let log = env.calls();
        assert!(!log.contains("nft -f"));
        assert!(!log.contains("sysctl -w"));
        assert!(!log.contains("systemctl"));
    }

    #[test]
    fn orchestration_refuses_foreign_blocks_or_unknown_forward_chain_without_mutation() {
        let foreign_drop = r#"{
          "nftables": [
            {"metainfo": {"json_schema_version": 1}},
            {"table": {"family": "inet", "name": "firewall"}},
            {"chain": {"family": "inet", "table": "firewall", "name": "forward", "type": "filter", "hook": "forward", "prio": 10, "policy": "accept"}},
            {"rule": {"family": "inet", "table": "firewall", "chain": "forward", "expr": [
              {"match": {"op": "==", "left": {"meta": {"key": "iifname"}}, "right": "obr0"}},
              {"drop": null}
            ]}}
          ]
        }"#;
        let env = StubEnv::new("0", foreign_drop);
        let mut runner = env.runner();
        let mut progress = Progress::new();

        let err = run_apply_on_host(
            &env.target,
            &mut runner,
            None,
            &env.paths,
            false,
            false,
            Path::new("/tmp/artifacts"),
            &mut progress,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("firewall::forward"), "{msg}");
        assert!(msg.contains("cannot be verified statically"), "{msg}");
        assert!(!env.paths.rules_path.exists());
        assert!(!env.paths.service_path.exists());
        assert!(!env.paths.sysctl_path.exists());
        let log = env.calls();
        assert!(!log.contains("nft -f"));
        assert!(!log.contains("sysctl -w"));
        assert!(!log.contains("systemctl"));
    }

    #[test]
    fn orchestration_apply_persists_ip_forward_and_remove_preserves_runtime_and_foreign_files() {
        let clean_host_fw = r#"{
          "nftables": [
            {"metainfo": {"json_schema_version": 1}},
            {"table": {"family": "inet", "name": "outbe_host_fw"}},
            {"chain": {"family": "inet", "table": "outbe_host_fw", "name": "input", "type": "filter", "hook": "input", "prio": 0, "policy": "drop"}}
          ]
        }"#;
        // Runtime ip_forward is already 1, but no persistent sysctl file sets it.
        let env = StubEnv::new("1", clean_host_fw);
        let mut runner = env.runner();
        let mut progress = Progress::new();

        // 1. Apply writes marked rules, unit, AND persistent sysctl drop-in even though runtime is 1.
        let outcome1 = run_apply_on_host(
            &env.target,
            &mut runner,
            None,
            &env.paths,
            false,
            false,
            Path::new("/tmp/artifacts"),
            &mut progress,
        )
        .unwrap();
        assert_eq!(outcome1, ApplyOutcome::Applied);
        assert!(env.paths.rules_path.is_file());
        assert!(env.paths.service_path.is_file());
        assert!(env.paths.sysctl_path.is_file());
        assert!(fs::read_to_string(&env.paths.sysctl_path)
            .unwrap()
            .contains(OWNERSHIP_MARKER));
        // Runtime ip_forward was already 1, so `sysctl -w` was not called.
        assert!(!env.calls().contains("sysctl -w"));

        // 2. Second apply is idempotent.
        let outcome2 = run_apply_on_host(
            &env.target,
            &mut runner,
            None,
            &env.paths,
            false,
            false,
            Path::new("/tmp/artifacts"),
            &mut progress,
        )
        .unwrap();
        assert_eq!(outcome2, ApplyOutcome::Applied);

        // 3. Remove deletes our marked files & table, and NEVER writes ip_forward=0.
        run_remove_on_host(&env.target, &mut runner, &env.paths, false, &mut progress).unwrap();
        assert!(!env.paths.rules_path.exists());
        assert!(!env.paths.service_path.exists());
        assert!(!env.paths.sysctl_path.exists());
        assert!(!env.nat_table_file.exists());
        assert_eq!(fs::read_to_string(&env.ip_fwd_file).unwrap().trim(), "1");
        assert!(!env.calls().contains("ip_forward=0"));

        // 4. Foreign sysctl file at our path is untouched by remove, and ip_forward is not written.
        fs::create_dir_all(env.etc_dir.join("sysctl.d")).unwrap();
        let foreign_content = "# admin custom sysctl\nnet.ipv4.ip_forward = 1\n";
        fs::write(&env.paths.sysctl_path, foreign_content).unwrap();
        fs::write(&env.log_file, "").unwrap();
        run_remove_on_host(&env.target, &mut runner, &env.paths, false, &mut progress).unwrap();
        assert_eq!(
            fs::read_to_string(&env.paths.sysctl_path).unwrap(),
            foreign_content
        );
        assert!(!env.calls().contains("sysctl -w"));
    }

    #[test]
    fn orchestration_partial_tcp443_masquerade_is_not_noop_and_requires_force() {
        let tcp443_only = r#"{
          "nftables": [
            {"metainfo": {"json_schema_version": 1}},
            {"table": {"family": "ip", "name": "partial_nat"}},
            {"chain": {"family": "ip", "table": "partial_nat", "name": "postrouting", "type": "nat", "hook": "postrouting", "prio": 100, "policy": "accept"}},
            {"rule": {"family": "ip", "table": "partial_nat", "chain": "postrouting", "expr": [
              {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
              {"match": {"op": "==", "left": {"meta": {"key": "oifname"}}, "right": "eno1"}},
              {"match": {"op": "==", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": 443}},
              {"masquerade": null}
            ]}}
          ]
        }"#;
        let env = StubEnv::new("1", tcp443_only);
        let mut runner = env.runner();
        let mut progress = Progress::new();

        let err = run_apply_on_host(
            &env.target,
            &mut runner,
            None,
            &env.paths,
            false,
            false,
            Path::new("/tmp/artifacts"),
            &mut progress,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("partial_nat::postrouting"), "{msg}");
        assert!(msg.contains("tcp.dport"), "{msg}");
        assert!(msg.contains("--force"), "{msg}");
        assert!(!env.paths.rules_path.exists());
        assert!(!env.calls().contains("nft -f"));
    }

    #[test]
    fn orchestration_mask_without_key_writes_dropin_and_later_overrides_refuse() {
        let clean_host_fw = r#"{
          "nftables": [
            {"metainfo": {"json_schema_version": 1}},
            {"table": {"family": "inet", "name": "outbe_host_fw"}},
            {"chain": {"family": "inet", "table": "outbe_host_fw", "name": "input", "type": "filter", "hook": "input", "prio": 0, "policy": "drop"}}
          ]
        }"#;
        let env = StubEnv::new("1", clean_host_fw);
        let mut runner = env.runner();
        let mut progress = Progress::new();

        let usr_lib_sysctl = &env.paths.sysctl_scan_dirs[1];
        let etc_sysctl = &env.paths.sysctl_scan_dirs[0];
        fs::create_dir_all(usr_lib_sysctl).unwrap();
        fs::create_dir_all(etc_sysctl).unwrap();

        // 1. Mask without key: /etc/sysctl.d/50-vendor.conf hides /usr/lib/sysctl.d/50-vendor.conf=1.
        fs::write(
            usr_lib_sysctl.join("50-vendor.conf"),
            "net.ipv4.ip_forward = 1\n",
        )
        .unwrap();
        fs::write(
            etc_sysctl.join("50-vendor.conf"),
            "# masks vendor file without setting ip_forward\nkernel.printk = 3 4 1 3\n",
        )
        .unwrap();

        let outcome = run_apply_on_host(
            &env.target,
            &mut runner,
            None,
            &env.paths,
            false,
            false,
            Path::new("/tmp/artifacts"),
            &mut progress,
        )
        .unwrap();
        assert_eq!(outcome, ApplyOutcome::Applied);
        assert!(
            env.paths.sysctl_path.is_file(),
            "masked vendor file must not suppress writing 90-outbe-up-nat.conf"
        );

        // Clean up our drop-in before testing refusal cases.
        run_remove_on_host(&env.target, &mut runner, &env.paths, false, &mut progress).unwrap();
        fs::write(&env.log_file, "").unwrap();

        // 2. Later 99-admin.conf = 0 refuses without writing 90-outbe-up-nat.conf.
        fs::write(
            etc_sysctl.join("99-admin.conf"),
            "net.ipv4.ip_forward = 0\n",
        )
        .unwrap();
        let err_99 = run_apply_on_host(
            &env.target,
            &mut runner,
            None,
            &env.paths,
            false,
            false,
            Path::new("/tmp/artifacts"),
            &mut progress,
        )
        .unwrap_err();
        assert!(err_99.to_string().contains("99-admin.conf"), "{err_99}");
        assert!(!env.paths.sysctl_path.exists());
        assert!(!env.calls().contains("nft -f"));

        // 3. /etc/sysctl.conf = 0 overrides 90-vm-host.conf = 1 and refuses without mutation.
        fs::remove_file(etc_sysctl.join("99-admin.conf")).unwrap();
        fs::write(
            etc_sysctl.join("90-vm-host.conf"),
            "net.ipv4.ip_forward = 1\n",
        )
        .unwrap();
        fs::write(&env.paths.sysctl_conf_path, "net.ipv4.ip_forward = 0\n").unwrap();
        fs::write(&env.log_file, "").unwrap();
        let err_conf = run_apply_on_host(
            &env.target,
            &mut runner,
            None,
            &env.paths,
            false,
            false,
            Path::new("/tmp/artifacts"),
            &mut progress,
        )
        .unwrap_err();
        assert!(err_conf.to_string().contains("sysctl.conf"), "{err_conf}");
        assert!(!env.paths.sysctl_path.exists());
        assert!(!env.calls().contains("nft -f"));
    }
}
