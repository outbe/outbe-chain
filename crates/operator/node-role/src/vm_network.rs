//! Runtime hybrid service-IP configuration for Outbe VMs (Owner Decision 10).
//!
//! Keeps the DHCPv4 lease acquired on the guest bridge interface (for gateway, prefix, DNS, and
//! host-free bring-up) while adding the node's deterministic `OUTBE_RPC_BIND_IP` as a static
//! secondary IPv4 address via a runtime `systemd-networkd` drop-in under `/run/systemd/network/`.
//!
//! Uses `networkctl reload` ONLY (never `networkctl reconfigure`), which in systemd 257 applies
//! the new `.network.d` drop-in seamlessly without tearing down the active DHCPv4 lease.

use std::{
    fs, io,
    io::Write,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};
use rustix::{
    fd::{AsFd, OwnedFd},
    fs::{
        fchmod, mkdirat, openat, renameat, statat, unlinkat, AtFlags, FileType, Mode, OFlags, CWD,
    },
    io::Errno,
};
use serde_json::Value;

pub const DEFAULT_NETWORKD_RUN_DIR: &str = "/run/systemd/network";
pub const SERVICE_IP_DROPIN_NAME: &str = "80-outbe-service-ip.conf";
pub const DEFAULT_DHCP_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_DHCP_POLL_INTERVAL: Duration = Duration::from_millis(500);
pub const DEFAULT_APPLY_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_APPLY_POLL_INTERVAL: Duration = Duration::from_millis(250);
const PREFERRED_SERVICE_METRIC: u32 = 512;
const DEFAULT_DHCP_ROUTE_METRIC: u32 = 1024;

/// Discovered live DHCPv4 parameters for the guest's primary bridge interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredDhcpConfig {
    pub iface: String,
    pub dhcp_ip: Ipv4Addr,
    pub prefix_len: u8,
    pub network: Ipv4Addr,
    pub broadcast: Ipv4Addr,
    pub gateway: Ipv4Addr,
    pub dhcp_metric: u32,
    pub network_file_name: String,
}

/// Summary of the applied hybrid service-IP configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedServiceIp {
    pub iface: String,
    pub dhcp_ip: Ipv4Addr,
    pub service_ip: Ipv4Addr,
    pub prefix_len: u8,
    pub gateway: Ipv4Addr,
    pub service_metric: u32,
    pub dropin_path: PathBuf,
}

/// Runtime timing and filesystem configuration for [`configure_service_ip_with_backend`].
#[derive(Clone, Debug)]
pub struct ServiceIpOptions {
    pub networkd_run_dir: PathBuf,
    pub dhcp_wait_timeout: Duration,
    pub dhcp_poll_interval: Duration,
    pub apply_wait_timeout: Duration,
    pub apply_poll_interval: Duration,
}

impl Default for ServiceIpOptions {
    fn default() -> Self {
        Self {
            networkd_run_dir: PathBuf::from(DEFAULT_NETWORKD_RUN_DIR),
            dhcp_wait_timeout: DEFAULT_DHCP_WAIT_TIMEOUT,
            dhcp_poll_interval: DEFAULT_DHCP_POLL_INTERVAL,
            apply_wait_timeout: DEFAULT_APPLY_WAIT_TIMEOUT,
            apply_poll_interval: DEFAULT_APPLY_POLL_INTERVAL,
        }
    }
}

/// Injectable guest networking backend so all discovery, drop-in rendering, reload, DAD, and
/// preferred-source route readiness logic can be unit-tested offline against real captures.
pub trait GuestNetBackend {
    fn ip_addr_show(&self) -> Result<String>;
    fn ip_route_default(&self) -> Result<String>;
    fn ip_route_show(&self, iface: &str) -> Result<String>;
    fn networkctl_status(&self, iface: &str) -> Result<String>;
    fn networkctl_reload(&self) -> Result<()>;
    fn sleep(&self, duration: Duration);
}

/// Live guest backend executing `ip` and `networkctl` from the measured image.
pub struct LiveGuestNetBackend;

impl GuestNetBackend for LiveGuestNetBackend {
    fn ip_addr_show(&self) -> Result<String> {
        run_capture("ip", &["-j", "-4", "addr", "show"])
    }

    fn ip_route_default(&self) -> Result<String> {
        run_capture("ip", &["-j", "-4", "route", "show", "default"])
    }

    fn ip_route_show(&self, iface: &str) -> Result<String> {
        run_capture("ip", &["-j", "-4", "route", "show", "dev", iface])
    }

    fn networkctl_status(&self, iface: &str) -> Result<String> {
        run_capture("networkctl", &["--json=short", "status", iface])
    }

    fn networkctl_reload(&self) -> Result<()> {
        let output = Command::new("networkctl")
            .arg("reload")
            .output()
            .wrap_err("failed to execute `networkctl reload`")?;
        ensure!(
            output.status.success(),
            "`networkctl reload` failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }

    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

fn run_capture(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .wrap_err_with(|| format!("failed to execute `{program} {}`", args.join(" ")))?;
    ensure!(
        output.status.success(),
        "`{program} {}` failed ({}): {}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8(output.stdout)
        .wrap_err_with(|| format!("non-UTF-8 stdout from `{program} {}`", args.join(" ")))
}

/// Configure `service_ip` (`OUTBE_RPC_BIND_IP`) on the live guest's DHCP bridge interface.
pub fn configure_service_ip(service_ip: Ipv4Addr) -> Result<AppliedServiceIp> {
    configure_service_ip_with_backend(
        service_ip,
        &LiveGuestNetBackend,
        &ServiceIpOptions::default(),
    )
}

/// Configure `service_ip` using an injectable [`GuestNetBackend`] and [`ServiceIpOptions`].
pub fn configure_service_ip_with_backend(
    service_ip: Ipv4Addr,
    backend: &dyn GuestNetBackend,
    options: &ServiceIpOptions,
) -> Result<AppliedServiceIp> {
    ensure!(
        !service_ip.is_loopback() && service_ip.is_private(),
        "VM service IP (OUTBE_RPC_BIND_IP) must be a non-loopback private IPv4 address, got {service_ip}"
    );

    let dhcp = wait_for_dhcp_config(backend, options)?;
    validate_service_ip_against_dhcp(service_ip, &dhcp)?;

    let service_metric = compute_service_metric(dhcp.dhcp_metric)?;
    let dropin_content = render_service_ip_dropin(service_ip, &dhcp, service_metric);
    let dropin_path = write_networkd_dropin_secure(
        &options.networkd_run_dir,
        &dhcp.network_file_name,
        &dropin_content,
    )?;

    backend
        .networkctl_reload()
        .wrap_err("failed to reload systemd-networkd after writing service-IP drop-in")?;

    wait_for_service_ip_ready(backend, service_ip, &dhcp, service_metric, options)?;

    Ok(AppliedServiceIp {
        iface: dhcp.iface,
        dhcp_ip: dhcp.dhcp_ip,
        service_ip,
        prefix_len: dhcp.prefix_len,
        gateway: dhcp.gateway,
        service_metric,
        dropin_path,
    })
}

fn wait_for_dhcp_config(
    backend: &dyn GuestNetBackend,
    options: &ServiceIpOptions,
) -> Result<DiscoveredDhcpConfig> {
    let deadline = Instant::now() + options.dhcp_wait_timeout;

    loop {
        let last_reason = match probe_dhcp_config(backend) {
            Ok(Some(config)) => return Ok(config),
            Ok(None) => {
                "no active en*/eth* interface with a dynamic DHCPv4 lease and default gateway yet"
                    .to_owned()
            }
            Err(err) => format!("{err:#}"),
        };

        if Instant::now() >= deadline {
            bail!(
                "timed out after {}s waiting for guest DHCPv4 lease/gateway/networkd file before configuring VM service IP (refusing to guess subnet prefix): {last_reason}",
                options.dhcp_wait_timeout.as_secs()
            );
        }
        backend.sleep(options.dhcp_poll_interval);
    }
}

fn probe_dhcp_config(backend: &dyn GuestNetBackend) -> Result<Option<DiscoveredDhcpConfig>> {
    let addr_json = backend.ip_addr_show()?;
    let Some((iface, dhcp_ip, prefix_len)) = parse_dhcp_interface_addr(&addr_json)? else {
        return Ok(None);
    };
    let (network, broadcast) = subnet_bounds(dhcp_ip, prefix_len)?;

    let default_route_json = backend.ip_route_default()?;
    let Some((gateway, dhcp_metric)) = parse_default_route(&default_route_json, &iface)? else {
        return Ok(None);
    };

    let status_json = backend.networkctl_status(&iface)?;
    let Some(network_file_name) = parse_network_file_name(&status_json)? else {
        return Ok(None);
    };

    Ok(Some(DiscoveredDhcpConfig {
        iface,
        dhcp_ip,
        prefix_len,
        network,
        broadcast,
        gateway,
        dhcp_metric,
        network_file_name,
    }))
}

fn is_candidate_bridge_iface(name: &str) -> bool {
    name.starts_with("eth") || name.starts_with("en")
}

/// Parse `ip -j -4 addr show` and return the first active `en*`/`eth*` interface carrying a
/// `scope: "global"`, `dynamic: true` IPv4 address `(iface, dhcp_ip, prefix_len)`.
pub fn parse_dhcp_interface_addr(raw_json: &str) -> Result<Option<(String, Ipv4Addr, u8)>> {
    let parsed: Value =
        serde_json::from_str(raw_json).wrap_err("parse `ip -j -4 addr show` JSON")?;
    let ifaces = parsed
        .as_array()
        .ok_or_else(|| eyre::eyre!("`ip -j -4 addr show` JSON root is not an array"))?;

    for iface in ifaces {
        let Some(name) = iface.get("ifname").and_then(Value::as_str) else {
            continue;
        };
        if !is_candidate_bridge_iface(name) {
            continue;
        }
        let operstate = iface.get("operstate").and_then(Value::as_str).unwrap_or("");
        let flags_up = iface
            .get("flags")
            .and_then(Value::as_array)
            .is_some_and(|arr| arr.iter().any(|f| f.as_str() == Some("UP")));
        if operstate != "UP" && !flags_up {
            continue;
        }

        let Some(addrs) = iface.get("addr_info").and_then(Value::as_array) else {
            continue;
        };
        for addr in addrs {
            if addr.get("family").and_then(Value::as_str) != Some("inet") {
                continue;
            }
            if addr.get("scope").and_then(Value::as_str) != Some("global") {
                continue;
            }
            if addr.get("dynamic").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            if addr.get("tentative").and_then(Value::as_bool) == Some(true)
                || addr.get("dadfailed").and_then(Value::as_bool) == Some(true)
            {
                continue;
            }
            let Some(local_str) = addr.get("local").and_then(Value::as_str) else {
                continue;
            };
            let Some(prefix_u64) = addr.get("prefixlen").and_then(Value::as_u64) else {
                continue;
            };
            let ip: Ipv4Addr = local_str
                .parse()
                .wrap_err_with(|| format!("invalid IPv4 address `{local_str}` on {name}"))?;
            let prefix_len = u8::try_from(prefix_u64)
                .wrap_err_with(|| format!("invalid prefixlen `{prefix_u64}` on {name}"))?;
            return Ok(Some((name.to_owned(), ip, prefix_len)));
        }
    }
    Ok(None)
}

/// Parse `ip -j -4 route show default` for `iface`, returning `(gateway, dhcp_metric)`.
///
/// Prefers a `protocol: "dhcp"` default route if multiple default routes exist on `iface`.
pub fn parse_default_route(raw_json: &str, iface: &str) -> Result<Option<(Ipv4Addr, u32)>> {
    let parsed: Value =
        serde_json::from_str(raw_json).wrap_err("parse `ip -j -4 route show default` JSON")?;
    let routes = parsed
        .as_array()
        .ok_or_else(|| eyre::eyre!("`ip -j -4 route show default` JSON root is not an array"))?;

    let mut fallback: Option<(Ipv4Addr, u32)> = None;
    for route in routes {
        if route.get("dst").and_then(Value::as_str) != Some("default") {
            continue;
        }
        if route.get("dev").and_then(Value::as_str) != Some(iface) {
            continue;
        }
        let Some(gw_str) = route.get("gateway").and_then(Value::as_str) else {
            continue;
        };
        let gateway: Ipv4Addr = gw_str
            .parse()
            .wrap_err_with(|| format!("invalid default gateway `{gw_str}` on {iface}"))?;
        let metric = route
            .get("metric")
            .and_then(Value::as_u64)
            .and_then(|m| u32::try_from(m).ok())
            .unwrap_or(DEFAULT_DHCP_ROUTE_METRIC);
        if route.get("protocol").and_then(Value::as_str) == Some("dhcp") {
            return Ok(Some((gateway, metric)));
        }
        if fallback.is_none() {
            fallback = Some((gateway, metric));
        }
    }
    Ok(fallback)
}

/// Parse `networkctl --json=short status <iface>` and extract the validated basename of
/// `NetworkFile` (e.g. `20-wired.network`).
pub fn parse_network_file_name(raw_json: &str) -> Result<Option<String>> {
    let parsed: Value =
        serde_json::from_str(raw_json).wrap_err("parse `networkctl --json=short status` JSON")?;
    let Some(raw_path) = parsed.get("NetworkFile").and_then(Value::as_str) else {
        return Ok(None);
    };
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let file_name = Path::new(trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| eyre::eyre!("networkctl NetworkFile has invalid basename: `{trimmed}`"))?;
    ensure!(
        file_name.ends_with(".network")
            && !file_name.contains('/')
            && !file_name.contains("..")
            && file_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
        "unsafe networkctl NetworkFile basename: `{file_name}`"
    );
    Ok(Some(file_name.to_owned()))
}

fn subnet_bounds(addr: Ipv4Addr, prefix_len: u8) -> Result<(Ipv4Addr, Ipv4Addr)> {
    ensure!(
        (1..=30).contains(&prefix_len),
        "guest DHCPv4 prefix length /{prefix_len} is invalid for hybrid service IP (must be /1../30)"
    );
    let mask = !0u32 << (32 - prefix_len);
    let network = Ipv4Addr::from(u32::from(addr) & mask);
    let broadcast = Ipv4Addr::from(u32::from(network) | !mask);
    Ok((network, broadcast))
}

/// Validate `service_ip` against the discovered DHCP subnet, gateway, and DHCP address.
pub fn validate_service_ip_against_dhcp(
    service_ip: Ipv4Addr,
    dhcp: &DiscoveredDhcpConfig,
) -> Result<()> {
    let mask = !0u32 << (32 - dhcp.prefix_len);
    ensure!(
        (u32::from(service_ip) & mask) == u32::from(dhcp.network),
        "VM service IP {service_ip} is outside guest DHCP subnet {}/{} on {}",
        dhcp.network,
        dhcp.prefix_len,
        dhcp.iface
    );
    ensure!(
        service_ip != dhcp.network && service_ip != dhcp.broadcast,
        "VM service IP {service_ip} cannot be the network ({}) or broadcast ({}) address of {}/{}",
        dhcp.network,
        dhcp.broadcast,
        dhcp.network,
        dhcp.prefix_len
    );
    ensure!(
        service_ip != dhcp.gateway,
        "VM service IP {service_ip} collides with guest DHCP gateway {} on {}",
        dhcp.gateway,
        dhcp.iface
    );
    ensure!(
        service_ip != dhcp.dhcp_ip,
        "VM service IP {service_ip} collides with the dynamic DHCPv4 address {} on {} (ensure service IPs are outside the host DHCP pool)",
        dhcp.dhcp_ip,
        dhcp.iface
    );
    Ok(())
}

/// Choose a strictly lower route metric than the DHCP/kernel metric so both the default egress
/// route and the on-link bridge subnet route prefer `service_ip` without colliding with kernel or
/// DHCP routes (distinct metrics coexist cleanly in Linux FIB).
pub fn compute_service_metric(dhcp_metric: u32) -> Result<u32> {
    ensure!(
        dhcp_metric > 1,
        "guest DHCP route metric {dhcp_metric} leaves no room for a higher-priority PreferredSource route"
    );
    if dhcp_metric > PREFERRED_SERVICE_METRIC {
        Ok(PREFERRED_SERVICE_METRIC)
    } else {
        Ok(dhcp_metric - 1)
    }
}

/// Render the runtime `systemd-networkd` drop-in adding `service_ip` with IPv4 DAD and
/// `PreferredSource` routes for both off-subnet default egress and on-link bridge traffic.
pub fn render_service_ip_dropin(
    service_ip: Ipv4Addr,
    dhcp: &DiscoveredDhcpConfig,
    service_metric: u32,
) -> String {
    format!(
        concat!(
            "# Managed at runtime by outbe-up internal vm-boot (hybrid service IP).\n",
            "[Address]\n",
            "Address={service_ip}/{prefix_len}\n",
            "DuplicateAddressDetection=ipv4\n",
            "\n",
            "[Route]\n",
            "Destination=0.0.0.0/0\n",
            "Gateway={gateway}\n",
            "PreferredSource={service_ip}\n",
            "Metric={service_metric}\n",
            "\n",
            "[Route]\n",
            "Destination={network}/{prefix_len}\n",
            "Scope=link\n",
            "PreferredSource={service_ip}\n",
            "Metric={service_metric}\n",
        ),
        service_ip = service_ip,
        prefix_len = dhcp.prefix_len,
        gateway = dhcp.gateway,
        network = dhcp.network,
        service_metric = service_metric,
    )
}

/// Atomically write `<networkd_run_dir>/<network_file_name>.d/80-outbe-service-ip.conf` refusing
/// symlinks at every path component and setting explicit `0755`/`0644` permissions (overriding
/// `outbe-vm-boot.service`'s `UMask=0077`) so the unprivileged `systemd-network` user can read it.
pub fn write_networkd_dropin_secure(
    networkd_run_dir: &Path,
    network_file_name: &str,
    content: &str,
) -> Result<PathBuf> {
    let base_fd = open_dir_nofollow(CWD, networkd_run_dir).wrap_err_with(|| {
        format!(
            "networkd runtime directory missing or symlink: {}",
            networkd_run_dir.display()
        )
    })?;
    fchmod(&base_fd, Mode::from_raw_mode(0o755))
        .map_err(io::Error::from)
        .wrap_err_with(|| format!("chmod 0755 {}", networkd_run_dir.display()))?;

    let dropin_dir_name = format!("{network_file_name}.d");
    match mkdirat(&base_fd, &dropin_dir_name, Mode::from_raw_mode(0o755)) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(err) => {
            return Err(io::Error::from(err)).wrap_err_with(|| {
                format!(
                    "create drop-in directory {}/{dropin_dir_name}",
                    networkd_run_dir.display()
                )
            });
        }
    }
    let dropin_dir_path = networkd_run_dir.join(&dropin_dir_name);
    let dropin_dir_fd = open_dir_nofollow(&base_fd, Path::new(&dropin_dir_name))
        .wrap_err_with(|| format!("refusing symlink drop-in dir {}", dropin_dir_path.display()))?;
    fchmod(&dropin_dir_fd, Mode::from_raw_mode(0o755))
        .map_err(io::Error::from)
        .wrap_err_with(|| format!("chmod 0755 {}", dropin_dir_path.display()))?;

    let dropin_path = dropin_dir_path.join(SERVICE_IP_DROPIN_NAME);
    match statat(
        &dropin_dir_fd,
        SERVICE_IP_DROPIN_NAME,
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(stat) => {
            ensure!(
                FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile,
                "refusing non-regular file or symlink at {}",
                dropin_path.display()
            );
        }
        Err(Errno::NOENT) => {}
        Err(err) => {
            return Err(io::Error::from(err))
                .wrap_err_with(|| format!("stat {}", dropin_path.display()));
        }
    }

    let tmp_name = format!(".{SERVICE_IP_DROPIN_NAME}.tmp.{}", std::process::id());
    let _ = unlinkat(&dropin_dir_fd, &tmp_name, AtFlags::empty());
    let tmp_fd = openat(
        &dropin_dir_fd,
        &tmp_name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o644),
    )
    .map_err(io::Error::from)
    .wrap_err_with(|| {
        format!(
            "create temporary drop-in {}/{tmp_name}",
            dropin_dir_path.display()
        )
    })?;
    fchmod(&tmp_fd, Mode::from_raw_mode(0o644))
        .map_err(io::Error::from)
        .wrap_err("chmod 0644 temporary networkd drop-in")?;
    let mut tmp_file = fs::File::from(tmp_fd);
    if let Err(err) = tmp_file
        .write_all(content.as_bytes())
        .and_then(|()| tmp_file.sync_all())
    {
        drop(tmp_file);
        let _ = unlinkat(&dropin_dir_fd, &tmp_name, AtFlags::empty());
        return Err(err).wrap_err_with(|| format!("write {}", dropin_path.display()));
    }
    drop(tmp_file);

    renameat(
        &dropin_dir_fd,
        &tmp_name,
        &dropin_dir_fd,
        SERVICE_IP_DROPIN_NAME,
    )
    .map_err(io::Error::from)
    .wrap_err_with(|| format!("install {}", dropin_path.display()))?;

    Ok(dropin_path)
}

fn open_dir_nofollow(parent: impl AsFd, path: &Path) -> Result<OwnedFd> {
    openat(
        parent,
        path,
        OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
    .wrap_err_with(|| format!("refusing symlink or non-directory: {}", path.display()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServiceAddrState {
    Absent,
    Tentative,
    DadFailed,
    Ready,
}

fn inspect_service_addr_state(
    raw_json: &str,
    iface: &str,
    service_ip: Ipv4Addr,
    prefix_len: u8,
    dhcp_ip: Ipv4Addr,
) -> Result<(ServiceAddrState, bool)> {
    let parsed: Value =
        serde_json::from_str(raw_json).wrap_err("parse `ip -j -4 addr show` JSON")?;
    let ifaces = parsed
        .as_array()
        .ok_or_else(|| eyre::eyre!("`ip -j -4 addr show` JSON root is not an array"))?;

    let mut service_state = ServiceAddrState::Absent;
    let mut dhcp_present = false;
    let service_str = service_ip.to_string();
    let dhcp_str = dhcp_ip.to_string();

    for entry in ifaces {
        if entry.get("ifname").and_then(Value::as_str) != Some(iface) {
            continue;
        }
        let Some(addrs) = entry.get("addr_info").and_then(Value::as_array) else {
            continue;
        };
        for addr in addrs {
            if addr.get("family").and_then(Value::as_str) != Some("inet") {
                continue;
            }
            let local = addr.get("local").and_then(Value::as_str).unwrap_or("");
            let plen = addr
                .get("prefixlen")
                .and_then(Value::as_u64)
                .and_then(|v| u8::try_from(v).ok());
            if local == dhcp_str && plen == Some(prefix_len) {
                dhcp_present = true;
            }
            if local == service_str && plen == Some(prefix_len) {
                if addr.get("dadfailed").and_then(Value::as_bool) == Some(true) {
                    service_state = ServiceAddrState::DadFailed;
                } else if addr.get("tentative").and_then(Value::as_bool) == Some(true) {
                    service_state = ServiceAddrState::Tentative;
                } else {
                    service_state = ServiceAddrState::Ready;
                }
            }
        }
    }
    Ok((service_state, dhcp_present))
}

fn verify_preferred_source_routes(
    raw_json: &str,
    service_ip: Ipv4Addr,
    dhcp: &DiscoveredDhcpConfig,
    service_metric: u32,
) -> Result<(bool, bool)> {
    let parsed: Value =
        serde_json::from_str(raw_json).wrap_err("parse `ip -j -4 route show` JSON")?;
    let routes = parsed
        .as_array()
        .ok_or_else(|| eyre::eyre!("`ip -j -4 route show` JSON root is not an array"))?;

    let service_str = service_ip.to_string();
    let gw_str = dhcp.gateway.to_string();
    let subnet_cidr = format!("{}/{}", dhcp.network, dhcp.prefix_len);

    let mut default_ok = false;
    let mut subnet_ok = false;

    for route in routes {
        let dst = route.get("dst").and_then(Value::as_str).unwrap_or("");
        let prefsrc = route.get("prefsrc").and_then(Value::as_str);
        let metric = route
            .get("metric")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok());
        if prefsrc != Some(service_str.as_str()) {
            continue;
        }
        if dst == "default"
            && route.get("gateway").and_then(Value::as_str) == Some(gw_str.as_str())
            && metric.is_some_and(|m| m <= service_metric && m < dhcp.dhcp_metric)
        {
            default_ok = true;
        }
        if dst == subnet_cidr
            && route.get("scope").and_then(Value::as_str) == Some("link")
            && metric.is_some_and(|m| m <= service_metric && m < dhcp.dhcp_metric)
        {
            subnet_ok = true;
        }
    }
    Ok((default_ok, subnet_ok))
}

fn wait_for_service_ip_ready(
    backend: &dyn GuestNetBackend,
    service_ip: Ipv4Addr,
    dhcp: &DiscoveredDhcpConfig,
    service_metric: u32,
    options: &ServiceIpOptions,
) -> Result<()> {
    let deadline = Instant::now() + options.apply_wait_timeout;

    loop {
        let addr_json = backend.ip_addr_show()?;
        let (addr_state, dhcp_present) = inspect_service_addr_state(
            &addr_json,
            &dhcp.iface,
            service_ip,
            dhcp.prefix_len,
            dhcp.dhcp_ip,
        )?;
        let last_status = match addr_state {
            ServiceAddrState::DadFailed => {
                bail!(
                    "IPv4 Duplicate Address Detection (DAD) conflict for service IP {service_ip}/{} on {}: another host on the bridge already claims this address",
                    dhcp.prefix_len,
                    dhcp.iface
                );
            }
            ServiceAddrState::Ready => {
                let route_json = backend.ip_route_show(&dhcp.iface)?;
                let (default_ok, subnet_ok) =
                    verify_preferred_source_routes(&route_json, service_ip, dhcp, service_metric)?;
                if default_ok && subnet_ok && dhcp_present {
                    return Ok(());
                }
                format!(
                    "service IP {service_ip} configured on {}, waiting for state (dhcp_present={dhcp_present}, default_prefsrc_route={default_ok}, subnet_prefsrc_route={subnet_ok})",
                    dhcp.iface
                )
            }
            ServiceAddrState::Tentative => format!(
                "service IP {service_ip}/{} on {} is still tentative (IPv4 DAD in progress)",
                dhcp.prefix_len, dhcp.iface
            ),
            ServiceAddrState::Absent => format!(
                "service IP {service_ip}/{} not yet present on {} after networkctl reload",
                dhcp.prefix_len, dhcp.iface
            ),
        };

        if Instant::now() >= deadline {
            bail!(
                "timed out after {}s waiting for VM service IP {service_ip}/{} on {} to become ready: {last_status}",
                options.apply_wait_timeout.as_secs(),
                dhcp.prefix_len,
                dhcp.iface
            );
        }
        backend.sleep(options.apply_poll_interval);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::{symlink, PermissionsExt},
        sync::Mutex,
    };

    use serde_json::json;

    use super::*;

    // Fixtures modeled directly on `/home/ubuntu/outbe-vm-work/dhcp-opt3/evidence/{guest-facts.stdout,guest-static.stdout}`:
    //   eth0: 10.77.0.122/24 dynamic, metric 1024, gateway 10.77.0.1, /etc/systemd/network/20-wired.network
    fn fixture_addr_dhcp_only() -> String {
        json!([
            {
                "ifindex": 1,
                "ifname": "lo",
                "flags": ["LOOPBACK", "UP", "LOWER_UP"],
                "operstate": "UNKNOWN",
                "addr_info": [{
                    "family": "inet",
                    "local": "127.0.0.1",
                    "prefixlen": 8,
                    "scope": "host"
                }]
            },
            {
                "ifindex": 2,
                "ifname": "eth0",
                "flags": ["BROADCAST", "MULTICAST", "UP", "LOWER_UP"],
                "operstate": "UP",
                "addr_info": [{
                    "family": "inet",
                    "local": "10.77.0.122",
                    "prefixlen": 24,
                    "broadcast": "10.77.0.255",
                    "scope": "global",
                    "dynamic": true,
                    "metric": 1024,
                    "label": "eth0"
                }]
            },
            {
                "ifindex": 3,
                "ifname": "docker0",
                "flags": ["NO-CARRIER", "BROADCAST", "MULTICAST", "UP"],
                "operstate": "DOWN",
                "addr_info": [{
                    "family": "inet",
                    "local": "172.17.0.1",
                    "prefixlen": 16,
                    "scope": "global",
                    "label": "docker0"
                }]
            }
        ])
        .to_string()
    }

    fn fixture_addr_with_service(service_ip: &str, tentative: bool, dadfailed: bool) -> String {
        json!([
            {
                "ifindex": 2,
                "ifname": "eth0",
                "flags": ["BROADCAST", "MULTICAST", "UP", "LOWER_UP"],
                "operstate": "UP",
                "addr_info": [
                    {
                        "family": "inet",
                        "local": "10.77.0.122",
                        "prefixlen": 24,
                        "broadcast": "10.77.0.255",
                        "scope": "global",
                        "dynamic": true,
                        "metric": 1024,
                        "label": "eth0"
                    },
                    {
                        "family": "inet",
                        "local": service_ip,
                        "prefixlen": 24,
                        "broadcast": "10.77.0.255",
                        "scope": "global",
                        "secondary": true,
                        "tentative": tentative,
                        "dadfailed": dadfailed,
                        "label": "eth0"
                    }
                ]
            }
        ])
        .to_string()
    }

    fn fixture_default_route_dhcp() -> String {
        json!([
            {
                "dst": "default",
                "gateway": "10.77.0.1",
                "dev": "eth0",
                "protocol": "dhcp",
                "prefsrc": "10.77.0.122",
                "metric": 1024
            }
        ])
        .to_string()
    }

    /// Resulting `ip -j -4 route show dev eth0` after drop-in reload:
    ///   default via 10.77.0.1 dev eth0 proto static src 10.77.0.81 metric 512
    ///   default via 10.77.0.1 dev eth0 proto dhcp   src 10.77.0.122 metric 1024
    ///   10.77.0.0/24 dev eth0 proto static scope link src 10.77.0.81 metric 512
    ///   10.77.0.0/24 dev eth0 proto kernel scope link src 10.77.0.122 metric 1024
    ///   10.77.0.1    dev eth0 proto dhcp   scope link src 10.77.0.122 metric 1024
    fn fixture_routes_after_reload(service_ip: &str) -> String {
        json!([
            {
                "dst": "default",
                "gateway": "10.77.0.1",
                "dev": "eth0",
                "protocol": "static",
                "prefsrc": service_ip,
                "metric": 512
            },
            {
                "dst": "default",
                "gateway": "10.77.0.1",
                "dev": "eth0",
                "protocol": "dhcp",
                "prefsrc": "10.77.0.122",
                "metric": 1024
            },
            {
                "dst": "10.77.0.0/24",
                "dev": "eth0",
                "protocol": "static",
                "scope": "link",
                "prefsrc": service_ip,
                "metric": 512
            },
            {
                "dst": "10.77.0.0/24",
                "dev": "eth0",
                "protocol": "kernel",
                "scope": "link",
                "prefsrc": "10.77.0.122",
                "metric": 1024
            },
            {
                "dst": "10.77.0.1",
                "dev": "eth0",
                "protocol": "dhcp",
                "scope": "link",
                "prefsrc": "10.77.0.122",
                "metric": 1024
            }
        ])
        .to_string()
    }

    fn fixture_networkctl_status() -> String {
        json!({
            "Index": 2,
            "Name": "eth0",
            "NetworkFile": "/etc/systemd/network/20-wired.network",
            "OperationalState": "routable",
            "AdministrativeState": "configured"
        })
        .to_string()
    }

    struct StubBackendState {
        addr_responses: Vec<String>,
        route_default_responses: Vec<String>,
        route_dev_responses: Vec<String>,
        networkctl_status_responses: Vec<String>,
        reload_calls: usize,
        sleep_calls: usize,
    }

    struct StubBackend {
        state: Mutex<StubBackendState>,
    }

    impl StubBackend {
        fn new(
            addr_responses: Vec<String>,
            route_default_responses: Vec<String>,
            route_dev_responses: Vec<String>,
            networkctl_status_responses: Vec<String>,
        ) -> Self {
            Self {
                state: Mutex::new(StubBackendState {
                    addr_responses,
                    route_default_responses,
                    route_dev_responses,
                    networkctl_status_responses,
                    reload_calls: 0,
                    sleep_calls: 0,
                }),
            }
        }

        fn next_or_last(queue: &mut Vec<String>) -> String {
            if queue.len() > 1 {
                queue.remove(0)
            } else {
                queue.first().cloned().unwrap_or_else(|| "[]".to_owned())
            }
        }
    }

    impl GuestNetBackend for StubBackend {
        fn ip_addr_show(&self) -> Result<String> {
            Ok(Self::next_or_last(
                &mut self.state.lock().unwrap().addr_responses,
            ))
        }

        fn ip_route_default(&self) -> Result<String> {
            Ok(Self::next_or_last(
                &mut self.state.lock().unwrap().route_default_responses,
            ))
        }

        fn ip_route_show(&self, _iface: &str) -> Result<String> {
            Ok(Self::next_or_last(
                &mut self.state.lock().unwrap().route_dev_responses,
            ))
        }

        fn networkctl_status(&self, _iface: &str) -> Result<String> {
            Ok(Self::next_or_last(
                &mut self.state.lock().unwrap().networkctl_status_responses,
            ))
        }

        fn networkctl_reload(&self) -> Result<()> {
            self.state.lock().unwrap().reload_calls += 1;
            Ok(())
        }

        fn sleep(&self, _duration: Duration) {
            self.state.lock().unwrap().sleep_calls += 1;
        }
    }

    fn fast_options(run_dir: &Path) -> ServiceIpOptions {
        ServiceIpOptions {
            networkd_run_dir: run_dir.to_path_buf(),
            dhcp_wait_timeout: Duration::from_millis(200),
            dhcp_poll_interval: Duration::from_millis(5),
            apply_wait_timeout: Duration::from_millis(200),
            apply_poll_interval: Duration::from_millis(5),
        }
    }

    #[test]
    fn happy_path_writes_dropin_with_0755_0644_reloads_only_and_waits_for_dad_and_routes() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run-network");
        fs::create_dir(&run_dir).unwrap();
        fs::set_permissions(&run_dir, fs::Permissions::from_mode(0o700)).unwrap();

        let service_ip = Ipv4Addr::new(10, 77, 0, 81);
        let backend = StubBackend::new(
            vec![
                // 1. Initial DHCP discovery
                fixture_addr_dhcp_only(),
                // 2. First poll after reload: tentative (DAD running)
                fixture_addr_with_service("10.77.0.81", true, false),
                // 3. Second poll: DAD complete, DHCP address preserved
                fixture_addr_with_service("10.77.0.81", false, false),
            ],
            vec![fixture_default_route_dhcp()],
            vec![fixture_routes_after_reload("10.77.0.81")],
            vec![fixture_networkctl_status()],
        );

        let applied =
            configure_service_ip_with_backend(service_ip, &backend, &fast_options(&run_dir))
                .unwrap();
        assert_eq!(applied.iface, "eth0");
        assert_eq!(applied.dhcp_ip, Ipv4Addr::new(10, 77, 0, 122));
        assert_eq!(applied.service_ip, service_ip);
        assert_eq!(applied.prefix_len, 24);
        assert_eq!(applied.gateway, Ipv4Addr::new(10, 77, 0, 1));
        assert_eq!(applied.service_metric, 512);
        assert_eq!(
            applied.dropin_path,
            run_dir.join("20-wired.network.d/80-outbe-service-ip.conf")
        );
        assert_eq!(backend.state.lock().unwrap().reload_calls, 1);

        // Verify explicit 0755/0644 permissions so non-root `systemd-network` can read under UMask=0077.
        assert_eq!(
            fs::metadata(&run_dir).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(run_dir.join("20-wired.network.d"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(&applied.dropin_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );

        let rendered = fs::read_to_string(&applied.dropin_path).unwrap();
        assert!(rendered.contains("Address=10.77.0.81/24\n"), "{rendered}");
        assert!(
            rendered.contains("DuplicateAddressDetection=ipv4\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Destination=0.0.0.0/0\nGateway=10.77.0.1\nPreferredSource=10.77.0.81\nMetric=512\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                "Destination=10.77.0.0/24\nScope=link\nPreferredSource=10.77.0.81\nMetric=512\n"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn waits_for_delayed_dhcp_lease_and_refuses_when_missing_without_guessing_slash24() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run-network");
        fs::create_dir(&run_dir).unwrap();

        // Delayed lease arrives on poll 2 -> succeeds.
        let delayed = StubBackend::new(
            vec![
                "[]".to_owned(),
                fixture_addr_dhcp_only(),
                fixture_addr_with_service("10.77.0.82", false, false),
            ],
            vec![fixture_default_route_dhcp()],
            vec![fixture_routes_after_reload("10.77.0.82")],
            vec![fixture_networkctl_status()],
        );
        let applied = configure_service_ip_with_backend(
            Ipv4Addr::new(10, 77, 0, 82),
            &delayed,
            &fast_options(&run_dir),
        )
        .unwrap();
        assert_eq!(applied.service_ip, Ipv4Addr::new(10, 77, 0, 82));
        assert!(delayed.state.lock().unwrap().sleep_calls >= 1);

        // Missing lease never arrives -> times out cleanly without writing any drop-in.
        let run_dir2 = tmp.path().join("run-network-2");
        fs::create_dir(&run_dir2).unwrap();
        let missing = StubBackend::new(
            vec!["[]".to_owned()],
            vec!["[]".to_owned()],
            vec!["[]".to_owned()],
            vec!["{}".to_owned()],
        );
        let mut opts = fast_options(&run_dir2);
        opts.dhcp_wait_timeout = Duration::from_millis(25);
        let err = configure_service_ip_with_backend(Ipv4Addr::new(10, 77, 0, 82), &missing, &opts)
            .unwrap_err();
        assert!(
            err.to_string().contains("refusing to guess subnet prefix"),
            "{err}"
        );
        assert_eq!(missing.state.lock().unwrap().reload_calls, 0);
        assert!(!run_dir2.join("20-wired.network.d").exists());
    }

    #[test]
    fn rejects_loopback_outside_subnet_network_broadcast_gateway_and_dhcp_ip() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run-network");
        fs::create_dir(&run_dir).unwrap();

        let cases = [
            (Ipv4Addr::new(127, 0, 0, 1), "non-loopback private"),
            (Ipv4Addr::new(162, 19, 107, 121), "non-loopback private"),
            (Ipv4Addr::new(10, 78, 0, 10), "outside guest DHCP subnet"),
            (Ipv4Addr::new(10, 77, 0, 0), "network"),
            (Ipv4Addr::new(10, 77, 0, 255), "broadcast"),
            (Ipv4Addr::new(10, 77, 0, 1), "gateway"),
            (Ipv4Addr::new(10, 77, 0, 122), "dynamic DHCPv4 address"),
        ];
        for (bad_ip, expect_substr) in cases {
            let backend = StubBackend::new(
                vec![fixture_addr_dhcp_only()],
                vec![fixture_default_route_dhcp()],
                vec![],
                vec![fixture_networkctl_status()],
            );
            let err = configure_service_ip_with_backend(bad_ip, &backend, &fast_options(&run_dir))
                .unwrap_err();
            assert!(
                err.to_string().contains(expect_substr),
                "for {bad_ip}, expected `{expect_substr}`, got: {err}"
            );
            assert_eq!(backend.state.lock().unwrap().reload_calls, 0);
        }
    }

    #[test]
    fn detects_ipv4_dad_conflict_immediately_and_times_out_stuck_tentative() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run-network");
        fs::create_dir(&run_dir).unwrap();

        // 1. Immediate failure on dadfailed=true.
        let dad_conflict = StubBackend::new(
            vec![
                fixture_addr_dhcp_only(),
                fixture_addr_with_service("10.77.0.83", false, true),
            ],
            vec![fixture_default_route_dhcp()],
            vec![fixture_routes_after_reload("10.77.0.83")],
            vec![fixture_networkctl_status()],
        );
        let err = configure_service_ip_with_backend(
            Ipv4Addr::new(10, 77, 0, 83),
            &dad_conflict,
            &fast_options(&run_dir),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("Duplicate Address Detection (DAD) conflict"),
            "{err}"
        );

        // 2. Stuck tentative=true times out cleanly.
        let stuck = StubBackend::new(
            vec![
                fixture_addr_dhcp_only(),
                fixture_addr_with_service("10.77.0.83", true, false),
            ],
            vec![fixture_default_route_dhcp()],
            vec![fixture_routes_after_reload("10.77.0.83")],
            vec![fixture_networkctl_status()],
        );
        let mut opts = fast_options(&run_dir);
        opts.apply_wait_timeout = Duration::from_millis(25);
        let err = configure_service_ip_with_backend(Ipv4Addr::new(10, 77, 0, 83), &stuck, &opts)
            .unwrap_err();
        assert!(err.to_string().contains("still tentative"), "{err}");
    }

    #[test]
    fn refuses_symlinked_networkd_dir_or_dropin_file() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let symlink_run_dir = tmp.path().join("run-link");
        symlink(&outside, &symlink_run_dir).unwrap();

        let err = write_networkd_dropin_secure(&symlink_run_dir, "20-wired.network", "[Address]\n")
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("symlink"),
            "expected symlink refusal, got: {err:#}"
        );

        let real_run_dir = tmp.path().join("run-real");
        let dropin_dir = real_run_dir.join("20-wired.network.d");
        fs::create_dir_all(&dropin_dir).unwrap();
        let outside_file = outside.join("victim.conf");
        fs::write(&outside_file, "untouched").unwrap();
        symlink(&outside_file, dropin_dir.join(SERVICE_IP_DROPIN_NAME)).unwrap();

        let err = write_networkd_dropin_secure(&real_run_dir, "20-wired.network", "[Address]\n")
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("symlink"),
            "expected dropin file symlink refusal, got: {err:#}"
        );
        assert_eq!(fs::read_to_string(&outside_file).unwrap(), "untouched");
    }
}
