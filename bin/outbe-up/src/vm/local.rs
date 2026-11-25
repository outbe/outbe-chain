//! Thin CLI wrapper for local single-VM management over the local VMM Unix socket.
//!
//! Uses only `outbe-operator-vm`; contains no SSH or `outbe-operator-remote` dependencies.

use std::{
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use eyre::{Context, Result};
use outbe_operator_vm::{
    dnsmasq::DEFAULT_DHCP_LEASES_FILE,
    local::{
        create_local_vm_cancellable, default_state_dir, expand_tilde, logs_local_vm,
        remove_local_vm, resolve_vmm_paths, start_local_vm, status_all_local_vms, status_local_vm,
        stop_local_vm, CreateLocalVmOptions, LocalVmStatusOutput, LocalVmStore,
        RemoveLocalVmOptions, DEFAULT_CREATE_RECOVERY_GRACE, DEFAULT_LOCAL_DISK_GIB,
        DEFAULT_LOCAL_FLAVOR, DEFAULT_LOCAL_MEMORY_GIB, DEFAULT_LOCAL_VCPU, DEFAULT_POLL_INTERVAL,
    },
    vmm::{LocalUnixVmmTransport, VmmClient},
};
use tokio::signal::unix::{signal, SignalKind};

#[derive(Clone, Debug)]
pub struct LocalConnectionArgs {
    pub vmm_socket: Option<PathBuf>,
    pub vmm_home: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub fix_permissions: bool,
    pub json: bool,
}

#[derive(Clone, Debug)]
pub struct LocalCreateArgs {
    pub name: String,
    pub image: PathBuf,
    pub image_name: Option<String>,
    pub flavor: Option<String>,
    pub disk: PathBuf,
    pub vcpu: Option<u32>,
    pub memory_gib: Option<u32>,
    pub disk_gib: Option<u32>,
    pub bridge: String,
    pub app_compose: Option<PathBuf>,
    pub ip: Option<Ipv4Addr>,
    pub no_tee: bool,
    pub retry_create: bool,
    pub timeout: Option<u64>,
    pub conn: LocalConnectionArgs,
}

fn open_local_context(
    conn: &LocalConnectionArgs,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<(
    LocalVmStore,
    VmmClient<LocalUnixVmmTransport>,
    PathBuf,
    PathBuf,
)> {
    let (vmm_socket, vmm_home) =
        resolve_vmm_paths(conn.vmm_socket.as_deref(), conn.vmm_home.as_deref())?;
    let state_dir = match &conn.state_dir {
        Some(dir) => expand_tilde(dir)?,
        None => default_state_dir()?,
    };
    let store = LocalVmStore::new(state_dir).with_fix_permissions(conn.fix_permissions);
    let mut transport = LocalUnixVmmTransport::new(&vmm_socket, &vmm_home);
    if let Some(flag) = cancel {
        transport = transport.with_cancel(flag);
    }
    let client = VmmClient::new(transport, vmm_socket.display().to_string());
    Ok((store, client, vmm_socket, vmm_home))
}

pub async fn create(args: LocalCreateArgs) -> Result<()> {
    // Install SIGINT/SIGTERM cancellation covering lock wait, pre-RPC checks, HTTP calls, and
    // `qemu-img` process-group termination.
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_signal = Arc::clone(&cancel);
    let mut sigint = signal(SignalKind::interrupt()).wrap_err("register SIGINT handler")?;
    let mut sigterm = signal(SignalKind::terminate()).wrap_err("register SIGTERM handler")?;
    let watcher = tokio::spawn(async move {
        tokio::select! {
            _ = sigint.recv() => { cancel_signal.store(true, Ordering::SeqCst); }
            _ = sigterm.recv() => { cancel_signal.store(true, Ordering::SeqCst); }
        }
    });

    let (store, vmm, vmm_socket, vmm_home) =
        open_local_context(&args.conn, Some(Arc::clone(&cancel)))?;
    let timeout = args.timeout.map(Duration::from_secs);
    let recovery_grace = match timeout {
        Some(t) => DEFAULT_CREATE_RECOVERY_GRACE.min(t),
        None => DEFAULT_CREATE_RECOVERY_GRACE,
    };
    let opts = CreateLocalVmOptions {
        name: args.name,
        image_dir: expand_tilde(&args.image)?,
        image_name: args.image_name,
        flavor: args
            .flavor
            .unwrap_or_else(|| DEFAULT_LOCAL_FLAVOR.to_owned()),
        disk_source: expand_tilde(&args.disk)?,
        vcpu: args.vcpu.unwrap_or(DEFAULT_LOCAL_VCPU),
        memory_gib: args.memory_gib.unwrap_or(DEFAULT_LOCAL_MEMORY_GIB),
        disk_gib: args.disk_gib.unwrap_or(DEFAULT_LOCAL_DISK_GIB),
        bridge: args.bridge,
        app_compose_path: args.app_compose.as_deref().map(expand_tilde).transpose()?,
        intended_ip: args.ip,
        no_tee: args.no_tee,
        vmm_socket,
        vmm_home,
        retry_create: args.retry_create,
        timeout,
        recovery_grace,
        poll_interval: DEFAULT_POLL_INTERVAL,
    };

    let cancel_worker = Arc::clone(&cancel);
    let result = tokio::task::spawn_blocking(move || {
        create_local_vm_cancellable(&store, &vmm, &opts, &cancel_worker)
    })
    .await
    .wrap_err("local VM create task panicked")?;
    watcher.abort();
    let out = result?;

    if args.conn.json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("Created stopped VM `{}` ({})", out.name, out.vm_id);
        println!(
            "  image:   {} ({})",
            out.image_name,
            if out.image_reused {
                "reused"
            } else {
                "published"
            }
        );
        println!("  bridge:  {} (mac {})", out.bridge, out.mac);
        if let Some(ip) = out.intended_ip {
            println!("  ip:      {ip} (service IP; configured in-guest by vm-boot)");
        }
        println!("  state:   {}", out.state_file.display());
    }
    Ok(())
}

pub fn start(name: &str, conn: LocalConnectionArgs) -> Result<()> {
    let (store, vmm, _, _) = open_local_context(&conn, None)?;
    let out = start_local_vm(&store, &vmm, name)?;
    if conn.json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else if out.already_running {
        println!(
            "VM `{}` ({}) is already running (status: {})",
            out.name, out.vm_id, out.vmm_status
        );
    } else {
        println!(
            "Started VM `{}` ({}) (status: {})",
            out.name, out.vm_id, out.vmm_status
        );
    }
    Ok(())
}

pub fn stop(name: &str, force: bool, timeout: Duration, conn: LocalConnectionArgs) -> Result<()> {
    let (store, vmm, _, _) = open_local_context(&conn, None)?;
    let out = stop_local_vm(&store, &vmm, name, force, timeout, DEFAULT_POLL_INTERVAL)?;
    if let Some(warning) = &out.warning {
        eprintln!("{warning}");
    }
    if conn.json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else if out.already_stopped {
        println!(
            "VM `{}` ({}) is already stopped (status: {})",
            out.name, out.vm_id, out.vmm_status
        );
    } else if out.cleared_started_flag {
        println!(
            "Sent StopVm to exited VM `{}` ({}) to clear dstack started/auto-restart flag (status: {})",
            out.name, out.vm_id, out.vmm_status
        );
    } else if out.forced {
        println!(
            "Force-stopped VM `{}` ({}) (status: {})",
            out.name, out.vm_id, out.vmm_status
        );
    } else {
        println!(
            "Gracefully stopped VM `{}` ({}) (status: {})",
            out.name, out.vm_id, out.vmm_status
        );
    }
    Ok(())
}

pub fn status(
    name: Option<&str>,
    dhcp_leases: Option<&Path>,
    conn: LocalConnectionArgs,
) -> Result<()> {
    let (store, vmm, _, _) = open_local_context(&conn, None)?;
    let leases_path = match dhcp_leases {
        Some(path) => expand_tilde(path)?,
        None => PathBuf::from(DEFAULT_DHCP_LEASES_FILE),
    };
    match name {
        Some(vm_name) => {
            let out = status_local_vm(&store, &vmm, vm_name, &leases_path)?;
            if conn.json {
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                print_status_table(&[out]);
            }
        }
        None => {
            let out = status_all_local_vms(&store, &vmm, &leases_path)?;
            if conn.json {
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else if out.is_empty() {
                println!("No local VMs recorded in {}", store.vms_dir().display());
            } else {
                print_status_table(&out);
            }
        }
    }
    Ok(())
}

pub fn logs(name: &str, lines: usize, conn: LocalConnectionArgs) -> Result<()> {
    let (store, vmm, _, _) = open_local_context(&conn, None)?;
    let out = logs_local_vm(&store, &vmm, name, lines)?;
    if conn.json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print!("{}", out.log);
        if !out.log.is_empty() && !out.log.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

pub fn remove(
    name: &str,
    force: bool,
    forget_intent: bool,
    timeout: Duration,
    conn: LocalConnectionArgs,
) -> Result<()> {
    let (store, vmm, _, _) = open_local_context(&conn, None)?;
    let out = remove_local_vm(
        &store,
        &vmm,
        name,
        RemoveLocalVmOptions {
            force,
            forget_intent,
            timeout,
            recovery_grace: DEFAULT_CREATE_RECOVERY_GRACE.min(timeout),
            poll_interval: DEFAULT_POLL_INTERVAL,
        },
    )?;
    if let Some(warning) = &out.warning {
        eprintln!("{warning}");
    }
    if conn.json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else if out.forgot_intent {
        println!(
            "Discarded unresolved Creating intent for `{}` and deleted {}",
            out.name,
            out.state_file.display()
        );
    } else {
        println!(
            "Removed VM `{}` ({}) and deleted {} (guest images left intact)",
            out.name,
            out.vm_id,
            out.state_file.display()
        );
    }
    Ok(())
}

fn print_status_table(rows: &[LocalVmStatusOutput]) {
    println!(
        "{:<20} {:<10} {:<12} {:<18} {:<16} {:<16} IMAGE",
        "NAME", "STATUS", "UPTIME", "MAC", "SERVICE_IP", "DHCP_IP"
    );
    for row in rows {
        let uptime = if row.uptime.is_empty() {
            "-"
        } else {
            &row.uptime
        };
        let mac = if row.mac.is_empty() { "-" } else { &row.mac };
        let service_ip = row
            .service_ip
            .or(row.intended_ip)
            .map(|addr| addr.to_string())
            .unwrap_or_else(|| "-".to_owned());
        let dhcp_ip = row
            .dhcp_ip
            .map(|addr| addr.to_string())
            .unwrap_or_else(|| "unknown".to_owned());
        println!(
            "{:<20} {:<10} {:<12} {:<18} {:<16} {:<16} {}",
            row.name, row.vmm_status, uptime, mac, service_ip, dhcp_ip, row.image_name
        );
        if let Some(boot_err) = &row.boot_error {
            println!("  boot_error:  {boot_err}");
        }
        if let Some(err) = &row.error {
            println!("  error:       {err}");
        }
    }
}
