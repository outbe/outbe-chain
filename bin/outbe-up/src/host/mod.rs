//! Thin local CLI wrapper for `outbe-up host status|setup|nat|publish|check`.
//!
//! Strictly local and strictly read-only: uses only `outbe-operator-vm`, with zero SSH,
//! zero remote dependencies, and zero mutations to host networking/DHCP/firewall configuration.

mod render;

use std::{net::Ipv4Addr, path::PathBuf};

use eyre::{bail, Context, Result};
use outbe_operator_vm::{
    host::{
        check_local_vm_host, evaluate_host_setup, inspect_host_status, plan_host_publish,
        plan_local_host_nat, HostEnv, VmPortsRequest, DEFAULT_BRIDGE_NAME,
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

fn resolve_nat_bridge(
    explicit: Option<String>,
    vmm_socket: Option<&PathBuf>,
    vmm_home: Option<&PathBuf>,
) -> String {
    if let Some(bridge) = explicit {
        return bridge;
    }
    open_vmm(vmm_socket, vmm_home)
        .ok()
        .and_then(|(vmm, _)| vmm.get_meta_typed().ok())
        .map(|meta| meta.networking.default_bridge)
        .filter(|bridge| !bridge.is_empty())
        .unwrap_or_else(|| DEFAULT_BRIDGE_NAME.to_owned())
}

pub fn status(args: StatusArgs) -> Result<()> {
    let (vmm, sock) = open_vmm(args.vmm_socket.as_ref(), args.vmm_home.as_ref())?;
    let env = HostEnv::from_sys_root(args.sys_root.as_deref(), None);
    let report = inspect_host_status(&vmm, &sock, args.bridge.as_deref(), &env)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render::print_status_report(&report);
    }
    Ok(())
}

pub fn setup(args: SetupArgs) -> Result<()> {
    let (vmm, sock) = open_vmm(args.vmm_socket.as_ref(), args.vmm_home.as_ref())?;
    let env = HostEnv::from_sys_root(args.sys_root.as_deref(), None);
    let report = evaluate_host_setup(&vmm, &sock, args.bridge.as_deref(), args.dry_run, &env)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render::print_setup_report(&report);
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
    let bridge = resolve_nat_bridge(
        args.bridge,
        args.vmm_socket.as_ref(),
        args.vmm_home.as_ref(),
    );
    let report = plan_local_host_nat(&bridge, args.uplink.as_deref(), args.remove, &env)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render::print_nat_report(&report);
    }
    Ok(())
}

pub fn check(args: CheckArgs) -> Result<()> {
    let (vmm, _) = open_vmm(args.vmm_socket.as_ref(), args.vmm_home.as_ref())?;
    let store = open_store(args.state_dir.as_ref(), false)?;
    let owner_override = parse_owner_pair(args.expected_hosts_owner.as_deref())?;
    let env = HostEnv::from_sys_root(args.sys_root.as_deref(), owner_override);

    let request = VmPortsRequest {
        vm_name: &args.vm,
        raw_ports: &args.ports,
        raw_sources: &args.sources,
        public_ip: args.public_ip,
        bridge: args.bridge.as_deref(),
    };
    let report = check_local_vm_host(&store, &vmm, &request, args.ip, &env)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render::print_check_report(&report);
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

    let request = VmPortsRequest {
        vm_name: &args.vm,
        raw_ports: &args.ports,
        raw_sources: &args.sources,
        public_ip: args.public_ip,
        bridge: args.bridge.as_deref(),
    };
    let report = plan_host_publish(&store, &vmm, &request, &env)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render::print_publish_report(&report);
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
