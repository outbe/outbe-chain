//! Thin local CLI wrapper for `outbe-up host status|setup|nat|reserve|release|publish`.
//!
//! Strictly local: uses only `outbe-operator-vm`, with zero SSH or remote dependencies.

use std::{net::Ipv4Addr, path::PathBuf};

use eyre::{bail, Context, Result};
use outbe_operator_vm::{
    host::{
        evaluate_host_setup, inspect_host_status, plan_host_publish, release_local_vm_dhcp,
        reserve_local_vm_dhcp, run_local_host_nat, HostEnv, DEFAULT_BRIDGE_NAME,
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
    pub dry_run: bool,
    pub force: bool,
    pub vmm_socket: Option<PathBuf>,
    pub vmm_home: Option<PathBuf>,
    pub sys_root: Option<PathBuf>,
    pub json: bool,
}

pub struct ReserveArgs {
    pub vm: String,
    pub ip: Option<Ipv4Addr>,
    pub apply: bool,
    pub bridge: Option<String>,
    pub vmm_socket: Option<PathBuf>,
    pub vmm_home: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub fix_permissions: bool,
    pub sys_root: Option<PathBuf>,
    pub expected_hosts_owner: Option<String>,
    pub json: bool,
}

pub struct ReleaseArgs {
    pub vm: String,
    pub apply: bool,
    pub bridge: Option<String>,
    pub vmm_socket: Option<PathBuf>,
    pub vmm_home: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub fix_permissions: bool,
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
    let report = run_local_host_nat(
        &bridge,
        args.uplink.as_deref(),
        args.remove,
        args.dry_run,
        args.force,
        &env,
    )?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Host NAT ({}, bridge={}, outcome={}):",
            report.action, report.bridge, report.outcome
        );
        println!("  {}", report.detail);
        if let Some(ruleset) = &report.rendered_ruleset {
            if args.dry_run {
                println!("\nPlanned ruleset:\n{ruleset}");
            }
        }
    }
    Ok(())
}

pub fn reserve(args: ReserveArgs) -> Result<()> {
    let (vmm, _) = open_vmm(args.vmm_socket.as_ref(), args.vmm_home.as_ref())?;
    let store = open_store(args.state_dir.as_ref(), args.fix_permissions && args.apply)?;
    let owner_override = parse_owner_pair(args.expected_hosts_owner.as_deref())?;
    let env = HostEnv::from_sys_root(args.sys_root.as_deref(), owner_override);

    let report = reserve_local_vm_dhcp(
        &store,
        &vmm,
        &args.vm,
        args.ip,
        args.bridge.as_deref(),
        args.apply,
        &env,
    )?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    if !report.applied {
        println!(
            "DHCP reservation preview for VM `{}` ({}) [read-only; no files modified]:",
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
        println!(
            "\nRe-run with `--apply` to write `{}` via locked CAS and run `{}`.",
            report.block_marker, report.reload_command
        );
    } else if report.changed {
        println!(
            "Reserved {} -> {} for VM `{}` in {} and reloaded {}.",
            report.mac,
            report.ip,
            report.vm,
            report.hosts_file.display(),
            report.dhcp_unit
        );
    } else {
        println!(
            "Reservation {} -> {} for VM `{}` is already present in {}; reloaded {} (no file change).",
            report.mac,
            report.ip,
            report.vm,
            report.hosts_file.display(),
            report.dhcp_unit
        );
    }
    Ok(())
}

pub fn release(args: ReleaseArgs) -> Result<()> {
    let store = open_store(args.state_dir.as_ref(), args.fix_permissions && args.apply)?;
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

    let report = release_local_vm_dhcp(&store, &args.vm, bridge.as_deref(), args.apply, &env)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    if !report.applied {
        println!(
            "DHCP release preview for VM `{}` [read-only; no files modified]:",
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
            println!(
                "\nRe-run with `--apply` to remove `{}` via locked CAS and run `{}`.",
                report.block_marker, report.reload_command
            );
        } else {
            println!(
                "  status:      no `{}` block present in {}",
                report.block_marker,
                report.hosts_file.display()
            );
        }
    } else if report.changed {
        println!(
            "Removed `{}` from {} and reloaded {}.",
            report.block_marker,
            report.hosts_file.display(),
            report.dhcp_unit
        );
    } else {
        println!(
            "No `{}` block present in {}; reloaded {} (no file change).",
            report.block_marker,
            report.hosts_file.display(),
            report.dhcp_unit
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
