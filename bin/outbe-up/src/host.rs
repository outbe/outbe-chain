//! Thin local CLI wrapper for `outbe-up host status|setup|nat|reserve|release|publish|check`.
//!
//! Strictly local and strictly read-only: uses only `outbe-operator-vm`, with zero SSH,
//! zero remote dependencies, and zero mutations to host networking/DHCP/firewall configuration.

use std::{net::Ipv4Addr, path::PathBuf};

use eyre::{bail, Context, Result};
use outbe_operator_vm::{
    host::{
        check_local_vm_host, evaluate_host_setup, inspect_host_status, plan_host_publish,
        plan_local_host_nat, release_local_vm_dhcp, reserve_local_vm_dhcp, HostEnv,
        DEFAULT_BRIDGE_NAME,
    },
    local::{default_state_dir, expand_tilde, resolve_vmm_paths, LocalVmStore},
    vmm::{LocalUnixVmmTransport, VmmClient},
};

pub struct StatusArgs {
    pub bridge: Option<String>,
    pub vmm_socket: Option<PathBuf>,
    pub vmm_home: Option<PathBuf>,
    pub sys_root: Option<PathBuf>,
    pub json: bool,
}

pub struct SetupArgs {
    pub bridge: Option<String>,
    pub vmm_socket: Option<PathBuf>,
    pub vmm_home: Option<PathBuf>,
    pub dry_run: bool,
    pub sys_root: Option<PathBuf>,
    pub json: bool,
}

pub struct NatArgs {
    pub bridge: Option<String>,
    pub uplink: Option<String>,
    pub remove: bool,
    pub vmm_socket: Option<PathBuf>,
    pub vmm_home: Option<PathBuf>,
    pub sys_root: Option<PathBuf>,
    pub json: bool,
}

pub struct ReserveArgs {
    pub vm: String,
    pub ip: Option<Ipv4Addr>,
    pub bridge: Option<String>,
    pub vmm_socket: Option<PathBuf>,
    pub vmm_home: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub sys_root: Option<PathBuf>,
    pub expected_hosts_owner: Option<String>,
    pub json: bool,
}

pub struct ReleaseArgs {
    pub vm: String,
    pub bridge: Option<String>,
    pub vmm_socket: Option<PathBuf>,
    pub vmm_home: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub sys_root: Option<PathBuf>,
    pub expected_hosts_owner: Option<String>,
    pub json: bool,
}

pub struct CheckArgs {
    pub vm: String,
    pub ip: Option<Ipv4Addr>,
    pub ports: Vec<String>,
    pub sources: Vec<String>,
    pub public_ip: Option<Ipv4Addr>,
    pub bridge: Option<String>,
    pub vmm_socket: Option<PathBuf>,
    pub vmm_home: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub sys_root: Option<PathBuf>,
    pub expected_hosts_owner: Option<String>,
    pub json: bool,
}

pub struct PublishArgs {
    pub vm: String,
    pub ports: Vec<String>,
    pub sources: Vec<String>,
    pub public_ip: Option<Ipv4Addr>,
    pub bridge: Option<String>,
    pub vmm_socket: Option<PathBuf>,
    pub vmm_home: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub sys_root: Option<PathBuf>,
    pub json: bool,
}

fn open_vmm(
    vmm_socket: Option<&PathBuf>,
    vmm_home: Option<&PathBuf>,
) -> Result<(VmmClient<LocalUnixVmmTransport>, PathBuf)> {
    let (sock, home) = resolve_vmm_paths(
        vmm_socket.map(PathBuf::as_path),
        vmm_home.map(PathBuf::as_path),
    )?;
    let transport = LocalUnixVmmTransport::new(&sock, &home);
    let client = VmmClient::new(transport, sock.display().to_string());
    Ok((client, sock))
}

fn open_store(state_dir: Option<&PathBuf>, fix_permissions: bool) -> Result<LocalVmStore> {
    let dir = match state_dir {
        Some(path) => expand_tilde(path)?,
        None => default_state_dir()?,
    };
    Ok(LocalVmStore::new(dir).with_fix_permissions(fix_permissions))
}

fn parse_owner_pair(raw: Option<&str>) -> Result<Option<(u32, u32)>> {
    let Some(s) = raw else {
        return Ok(None);
    };
    let Some((u, g)) = s.split_once(':') else {
        bail!("invalid --expected-hosts-owner `{s}` (expected uid:gid)");
    };
    let uid: u32 = u
        .parse()
        .wrap_err_with(|| format!("invalid uid in --expected-hosts-owner `{s}`"))?;
    let gid: u32 = g
        .parse()
        .wrap_err_with(|| format!("invalid gid in --expected-hosts-owner `{s}`"))?;
    Ok(Some((uid, gid)))
}

pub fn status(args: StatusArgs) -> Result<()> {
    let (vmm, sock) = open_vmm(args.vmm_socket.as_ref(), args.vmm_home.as_ref())?;
    let env = HostEnv::from_sys_root(args.sys_root.as_deref(), None);
    let report = inspect_host_status(&vmm, &sock, args.bridge.as_deref(), &env)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("Host status (schema v{}):", report.version);
    println!(
        "  Bridge:    {} ({}, cidr={}, gw={}, managed={})",
        report.bridge.name,
        if report.bridge.present && report.bridge.up {
            "UP"
        } else if report.bridge.present {
            "DOWN"
        } else {
            "ABSENT"
        },
        report.bridge.cidr.as_deref().unwrap_or("-"),
        report
            .bridge
            .gateway_ip
            .map_or_else(|| "-".to_owned(), |ip| ip.to_string()),
        report.bridge.management
    );
    println!(
        "  DHCP:      {} (unit={}, hosts={}, leases={}, pool={}..{}, lease={}, reload={})",
        if report.dhcp.detected && report.dhcp.unit_active {
            "active"
        } else if report.dhcp.detected {
            "detected/inactive"
        } else {
            "not detected"
        },
        report.dhcp.unit_name.as_deref().unwrap_or("-"),
        report
            .dhcp
            .hosts_file
            .as_ref()
            .map_or_else(|| "-".to_owned(), |p| p.display().to_string()),
        report
            .dhcp
            .leases_file
            .as_ref()
            .map_or_else(|| "-".to_owned(), |p| p.display().to_string()),
        report
            .dhcp
            .pool_start
            .map_or_else(|| "-".to_owned(), |ip| ip.to_string()),
        report
            .dhcp
            .pool_end
            .map_or_else(|| "-".to_owned(), |ip| ip.to_string()),
        report.dhcp.lease_duration.as_deref().unwrap_or("-"),
        report.dhcp.reload_mode
    );
    println!(
        "  Firewall:  nft={}, tables={}, forward_allows={}, dnat_rules={}, unparsed_dnat={}, managed_nat={}",
        if report.firewall.nft_accessible {
            "accessible"
        } else {
            "unavailable"
        },
        report.firewall.tables.len(),
        report.firewall.forward_allows.len(),
        report.firewall.dnat_rules.len(),
        report.firewall.unparsed_dnat_rules.len(),
        report.firewall.managed_nat_table_present
    );
    for chain in &report.firewall.forward_chains {
        println!(
            "             forward {} {}::{} policy={} ({:?})",
            chain.family, chain.table, chain.chain, chain.policy, chain.class
        );
    }
    println!(
        "  NAT:       egress={}, uplink={}, ip_forward=runtime:{} persistent:{} ({})",
        report.nat.egress_status,
        report.nat.uplink.as_deref().unwrap_or("-"),
        report
            .nat
            .ip_forward_runtime
            .map_or_else(|| "?".to_owned(), |v| v.to_string()),
        report
            .nat
            .ip_forward_persistent
            .effective_value
            .map_or_else(|| "unset".to_owned(), |v| v.to_string()),
        report
            .nat
            .ip_forward_persistent
            .effective_source
            .as_deref()
            .unwrap_or("none")
    );
    println!(
        "  VMM:       {} ({}, version={}, default_mode={}, default_bridge={}, vms={}/{})",
        if report.vmm.reachable {
            "reachable"
        } else {
            "unreachable"
        },
        report.vmm.socket_path.display(),
        report.vmm.version.as_deref().unwrap_or("-"),
        report.vmm.default_mode.as_deref().unwrap_or("-"),
        report.vmm.default_bridge.as_deref().unwrap_or("-"),
        report
            .vmm
            .running_vms
            .map_or_else(|| "-".to_owned(), |n| n.to_string()),
        report
            .vmm
            .total_vms
            .map_or_else(|| "-".to_owned(), |n| n.to_string())
    );
    for note in &report.vmm.drift_notes {
        println!("             drift: {note}");
    }
    println!(
        "  Platform:  kvm={}/{}, vsock={}, tee={}, qgs(required={}, active={}, pccs_active={})",
        report.platform.kvm_available,
        if report.platform.kvm_accessible {
            "rw"
        } else {
            "no-rw"
        },
        report.platform.vhost_vsock_available,
        report.platform.tee_kind,
        report.platform.qgs.required,
        report.platform.qgs.unit_active,
        report.platform.qgs.pccs_unit_active
    );
    println!("  Ready:     {}", report.ready_for_vm);
    for note in &report.notes {
        println!("  Note:      {note}");
    }
    Ok(())
}

pub fn setup(args: SetupArgs) -> Result<()> {
    let (vmm, sock) = open_vmm(args.vmm_socket.as_ref(), args.vmm_home.as_ref())?;
    let env = HostEnv::from_sys_root(args.sys_root.as_deref(), None);
    let report = evaluate_host_setup(&vmm, &sock, args.bridge.as_deref(), args.dry_run, &env)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if report.already_configured {
        println!("Host networking is already configured (zero writes performed):");
        println!(
            "  bridge: {} ({}, managed={})",
            report.status.bridge.name,
            report.status.bridge.cidr.as_deref().unwrap_or("-"),
            report.status.bridge.management
        );
        println!(
            "  dhcp:   {} (hosts={})",
            report.status.dhcp.unit_name.as_deref().unwrap_or("-"),
            report
                .status
                .dhcp
                .hosts_file
                .as_ref()
                .map_or_else(|| "-".to_owned(), |p| p.display().to_string())
        );
        println!(
            "  egress: {} (ip_forward={})",
            report.status.nat.egress_status,
            report
                .status
                .nat
                .ip_forward_runtime
                .map_or_else(|| "?".to_owned(), |v| v.to_string())
        );
    } else {
        println!("Host is missing required VM networking component(s):");
        for item in &report.missing_components {
            println!("  - {item}");
        }
        println!("\nRequired setup steps:");
        for (idx, step) in report.planned_steps.iter().enumerate() {
            println!("  {}. {step}", idx + 1);
        }
        if let Some(note) = &report.approval_note {
            println!("\n{note}");
        }
    }

    if !report.already_configured {
        bail!(
            "{}",
            report
                .approval_note
                .as_deref()
                .unwrap_or("host setup refused: missing components require owner design approval")
        );
    }
    Ok(())
}

pub fn nat(args: NatArgs) -> Result<()> {
    let env = HostEnv::from_sys_root(args.sys_root.as_deref(), None);
    let bridge = match args.bridge {
        Some(b) => b,
        None => {
            if let Ok((vmm, _)) = open_vmm(args.vmm_socket.as_ref(), args.vmm_home.as_ref()) {
                vmm.get_meta_typed()
                    .ok()
                    .map(|m| m.networking.default_bridge)
                    .filter(|b| !b.is_empty())
                    .unwrap_or_else(|| DEFAULT_BRIDGE_NAME.to_owned())
            } else {
                DEFAULT_BRIDGE_NAME.to_owned()
            }
        }
    };
    let report = plan_local_host_nat(&bridge, args.uplink.as_deref(), args.remove, &env)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Host NAT ({}, bridge={}, outcome={}) [read-only; zero writes]:",
            report.action, report.bridge, report.outcome
        );
        println!("  {}", report.detail);
        for warning in &report.warnings {
            println!("  Warning: {warning}");
        }
        if let Some(ruleset) = &report.rendered_ruleset {
            println!("\nRecommended nftables ruleset:\n{ruleset}");
        }
        if let Some(unit) = &report.rendered_unit {
            println!("Recommended systemd unit:\n{unit}");
        }
        if let Some(sysctl) = &report.rendered_sysctl {
            println!("Recommended sysctl drop-in:\n{sysctl}");
        }
        if !report.recommended_commands.is_empty() {
            println!("Recommended command(s):");
            for (idx, cmd) in report.recommended_commands.iter().enumerate() {
                println!("  {}. {cmd}", idx + 1);
            }
        }
    }
    Ok(())
}

pub fn reserve(args: ReserveArgs) -> Result<()> {
    let (vmm, _) = open_vmm(args.vmm_socket.as_ref(), args.vmm_home.as_ref())?;
    let store = open_store(args.state_dir.as_ref(), false)?;
    let owner_override = parse_owner_pair(args.expected_hosts_owner.as_deref())?;
    let env = HostEnv::from_sys_root(args.sys_root.as_deref(), owner_override);

    let report = reserve_local_vm_dhcp(
        &store,
        &vmm,
        &args.vm,
        args.ip,
        args.bridge.as_deref(),
        &env,
    )?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!(
        "DHCP reservation recommendation for VM `{}` ({}) [read-only; zero writes]:",
        report.vm, report.vm_id
    );
    println!("  hosts file:  {}", report.hosts_file.display());
    println!("  DHCP unit:   {}", report.dhcp_unit);
    println!(
        "  status:      {}",
        if report.already_present {
            "already present in hosts file"
        } else {
            "not yet written"
        }
    );
    println!(
        "\nManaged block to add to {}:\n{}",
        report.hosts_file.display(),
        report.block_content
    );
    println!("Reload command:\n  {}", report.reload_command);
    println!("\nNo files were modified or services reloaded (read-only).");
    Ok(())
}

pub fn release(args: ReleaseArgs) -> Result<()> {
    let store = open_store(args.state_dir.as_ref(), false)?;
    let owner_override = parse_owner_pair(args.expected_hosts_owner.as_deref())?;
    let env = HostEnv::from_sys_root(args.sys_root.as_deref(), owner_override);
    let bridge = match args.bridge {
        Some(b) => Some(b),
        None => {
            if store.state_path(&args.vm).is_ok_and(|p| p.is_file()) {
                None
            } else if let Ok((vmm, _)) = open_vmm(args.vmm_socket.as_ref(), args.vmm_home.as_ref())
            {
                vmm.get_meta_typed()
                    .ok()
                    .map(|m| m.networking.default_bridge)
                    .filter(|b| !b.is_empty())
            } else {
                None
            }
        }
    };

    let report = release_local_vm_dhcp(&store, &args.vm, bridge.as_deref(), &env)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!(
        "DHCP release recommendation for VM `{}` [read-only; zero writes]:",
        report.vm
    );
    println!("  hosts file:  {}", report.hosts_file.display());
    println!("  DHCP unit:   {}", report.dhcp_unit);
    if let Some(block) = &report.existing_block {
        println!(
            "\nManaged block to remove from {}:\n{block}",
            report.hosts_file.display()
        );
        println!("Reload command:\n  {}", report.reload_command);
    } else {
        println!(
            "  status:      no `{}` block present in {}",
            report.block_marker,
            report.hosts_file.display()
        );
    }
    println!("\nNo files were modified or services reloaded (read-only).");
    Ok(())
}

pub fn check(args: CheckArgs) -> Result<()> {
    let (vmm, _) = open_vmm(args.vmm_socket.as_ref(), args.vmm_home.as_ref())?;
    let store = open_store(args.state_dir.as_ref(), false)?;
    let owner_override = parse_owner_pair(args.expected_hosts_owner.as_deref())?;
    let env = HostEnv::from_sys_root(args.sys_root.as_deref(), owner_override);

    let report = check_local_vm_host(
        &store,
        &vmm,
        &args.vm,
        args.ip,
        &args.ports,
        &args.sources,
        args.public_ip,
        args.bridge.as_deref(),
        &env,
    )?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Host check for VM `{}` ({}, mac={}, ip={}, bridge={}) [read-only; zero writes]:",
            report.vm, report.vm_id, report.mac, report.ip, report.bridge
        );
        println!("  host_network_ok:    {}", report.host_network_ok);
        println!("  service_ip_ok:      {}", report.service_ip_ok);
        println!(
            "  dhcp_reserved:      {} (optional; static internal IP)",
            report.dhcp_reserved
        );
        println!("  published_ports_ok: {}", report.published_ports_ok);
        println!("  ready:              {}", report.ready);

        if !report.conflicts.is_empty() {
            println!("\nConflicts:");
            for item in &report.conflicts {
                println!("  - {item}");
            }
        }
        if !report.missing.is_empty() {
            println!("\nMissing prerequisites:");
            for item in &report.missing {
                println!("  - {item}");
            }
        }
        if let Some(block) = &report.recommended_dhcp_block {
            println!(
                "\nRecommended DHCP reservation for {}:\n{block}",
                report
                    .hosts_file
                    .as_ref()
                    .map_or_else(|| "<dhcp-hosts>".to_owned(), |p| p.display().to_string())
            );
            if let Some(reload) = &report.recommended_dhcp_reload {
                println!("Reload DHCP:\n  {reload}");
            }
        }
        if !report.recommended_forward_rules.is_empty() {
            println!("\nRecommended forward accept rule(s):");
            for rule in &report.recommended_forward_rules {
                println!("    {rule}");
            }
        }
        if !report.recommended_prerouting_rules.is_empty() {
            println!("\nRecommended prerouting DNAT rule(s):");
            for rule in &report.recommended_prerouting_rules {
                println!("    {rule}");
            }
        }
        if !report.recommended_firewall_commands.is_empty() {
            println!("\nApply firewall changes:");
            for (idx, cmd) in report.recommended_firewall_commands.iter().enumerate() {
                println!("  {}. {cmd}", idx + 1);
            }
        }
    }

    if !report.ready {
        let mut reasons = Vec::new();
        reasons.extend(report.conflicts.iter().cloned());
        reasons.extend(report.missing.iter().cloned());
        bail!(
            "host prerequisites not satisfied for VM `{}`: {}",
            report.vm,
            reasons.join("; ")
        );
    }
    Ok(())
}

pub fn publish(args: PublishArgs) -> Result<()> {
    let (vmm, _) = open_vmm(args.vmm_socket.as_ref(), args.vmm_home.as_ref())?;
    let store = open_store(args.state_dir.as_ref(), false)?;
    let env = HostEnv::from_sys_root(args.sys_root.as_deref(), None);

    let report = plan_host_publish(
        &store,
        &vmm,
        &args.vm,
        &args.ports,
        &args.sources,
        args.public_ip,
        args.bridge.as_deref(),
        &env,
    )?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Host publish plan for VM `{}` ({}, guest_ip={}, public_ip={}) [read-only; zero writes]:",
            report.vm, report.vm_id, report.guest_ip, report.public_ip
        );
        println!(
            "  detected firewall: {} (forward policy={}), {}, file={}",
            report.forward_table,
            report.forward_policy,
            report.nat_table,
            report.firewall_file.display()
        );
        if !report.conflicts.is_empty() {
            println!("\nCONFLICTS DETECTED:");
            for c in &report.conflicts {
                println!(
                    "  - {}/{} is already DNATed to {}:{} in {}::{} (sources={:?}, comment={}, handle={:?})",
                    c.proto,
                    c.host_port,
                    c.existing_dest_ip,
                    c.existing_guest_port,
                    c.existing_table,
                    c.existing_chain,
                    c.existing_sources,
                    c.existing_comment.as_deref().unwrap_or("-"),
                    c.existing_handle
                );
            }
        } else {
            println!(
                "\nRules for `table {} {{ chain forward {{ ... }} }}` in {}:",
                report.forward_table,
                report.firewall_file.display()
            );
            for rule in &report.forward_rules {
                println!("    {rule}");
            }
            println!(
                "\nRules for `table {} {{ chain prerouting {{ ... }} }}` in {}:",
                report.nat_table,
                report.firewall_file.display()
            );
            for rule in &report.prerouting_rules {
                println!("    {rule}");
            }
            println!("\nApply:");
            for (idx, cmd) in report.apply_commands.iter().enumerate() {
                println!("  {}. {cmd}", idx + 1);
            }
            println!("\nRemove:");
            for (idx, cmd) in report.remove_commands.iter().enumerate() {
                println!("  {}. {cmd}", idx + 1);
            }
            println!("\nNo files or nftables rules were modified (read-only).");
        }
    }

    if !report.conflicts.is_empty() {
        let summary = report
            .conflicts
            .iter()
            .map(|c| {
                format!(
                    "{}/{} -> {}:{} (comment={})",
                    c.proto,
                    c.host_port,
                    c.existing_dest_ip,
                    c.existing_guest_port,
                    c.existing_comment.as_deref().unwrap_or("-")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        bail!(
            "port conflict(s) detected in live nftables ruleset: {summary}. Choose a free host port with `--port <guest>:<free_host>`"
        );
    }
    Ok(())
}
