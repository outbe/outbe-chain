//! `outbe-up host nat`: recommend the outbound NAT ruleset, systemd unit, and sysctl drop-in for
//! one bridge.

use eyre::{ensure, Result};

use super::{
    bridge::{detect_bridge, detect_default_uplink},
    firewall::detect_firewall_and_egress,
    sysctl::{probe_managed_nat_files, read_runtime_ip_forward, scan_persistent_ip_forward},
    HostEnv, HostNatLocalReport,
};
use crate::nat::{
    build_apply_command, build_remove_command, render_nat_ruleset, render_systemd_unit,
    validate_iface_name, FileOwnership, NAT_TABLE_FAMILY, NAT_TABLE_NAME, OWNERSHIP_MARKER,
};

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
