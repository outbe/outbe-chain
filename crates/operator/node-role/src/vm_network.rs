//! Plain static IPv4 configuration for Outbe VMs (Owner Decision 10, revised).
//!
//! Reads the VM's static network parameters from `/opt/outbe-chain/validator.env`
//! (`OUTBE_VM_IP`, `OUTBE_VM_PREFIX`, `OUTBE_VM_GATEWAY`, `OUTBE_VM_DNS`), writes one
//! runtime `systemd-networkd` drop-in under `/run/systemd/network/20-wired.network.d/`, runs
//! `networkctl reload`, and waits bounded until the static IPv4 address (non-tentative, no DAD
//! conflict) and default route via the configured gateway are up.

use std::{
    collections::{BTreeMap, BTreeSet},
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
pub const BASE_NETWORK_FILE_NAME: &str = "20-wired.network";
pub const STATIC_NETWORK_DROPIN_NAME: &str = "80-outbe-static.conf";
pub const DEFAULT_APPLY_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_APPLY_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub const DEFAULT_RPC_PORT: u16 = 8_545;
pub const DEFAULT_CONSENSUS_PORT: u16 = 30_400;
pub const DEFAULT_RETH_PORT: u16 = 30_303;
pub const DEFAULT_DISCV5_PORT: u16 = 31_303;
pub const DEFAULT_RADICLE_PORT: u16 = 8_776;
const FIXED_GUEST_PORTS: &[u16] = &[8_551, 8_876, 9_000, 9_002, 9_003, 9_101];

/// Validated explicit listen/advertise ports from `validator.env` (`OUTBE_*_PORT`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VmPortsConfig {
    pub rpc_port: u16,
    pub consensus_port: u16,
    pub reth_port: u16,
    pub discv5_port: u16,
    pub radicle_port: u16,
}

impl Default for VmPortsConfig {
    fn default() -> Self {
        Self {
            rpc_port: DEFAULT_RPC_PORT,
            consensus_port: DEFAULT_CONSENSUS_PORT,
            reth_port: DEFAULT_RETH_PORT,
            discv5_port: DEFAULT_DISCV5_PORT,
            radicle_port: DEFAULT_RADICLE_PORT,
        }
    }
}

impl VmPortsConfig {
    /// Parse and strictly validate the five explicit port keys from `validator.env` (VM mode: all 5 required).
    pub fn from_env(values: &BTreeMap<String, String>) -> Result<Self> {
        Self::from_env_inner(values, true)
    }

    /// Parse and validate port keys for bare-host `validator.env`, allowing legacy installations
    /// created before `OUTBE_RPC_PORT` existed to default to `8545`.
    pub fn from_env_bare_host(values: &BTreeMap<String, String>) -> Result<Self> {
        Self::from_env_inner(values, false)
    }

    fn from_env_inner(values: &BTreeMap<String, String>, require_rpc_port: bool) -> Result<Self> {
        let rpc_port =
            if require_rpc_port || values.get("OUTBE_RPC_PORT").is_some_and(|v| !v.is_empty()) {
                parse_port_env(values, "OUTBE_RPC_PORT")?
            } else {
                DEFAULT_RPC_PORT
            };
        let cfg = Self {
            rpc_port,
            consensus_port: parse_port_env(values, "OUTBE_CONSENSUS_PORT")?,
            reth_port: parse_port_env(values, "OUTBE_RETH_P2P_PORT")?,
            discv5_port: parse_port_env(values, "OUTBE_RETH_DISCOVERY_PORT")?,
            radicle_port: parse_port_env(values, "OUTBE_RADICLE_PORT")?,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// Strictly validate that all five ports are non-zero, pairwise distinct, and do not collide
    /// with fixed loopback/service ports (8551, 8876, 9000, 9002, 9003, 9101).
    pub fn validate(&self) -> Result<()> {
        let entries = [
            ("OUTBE_RPC_PORT", self.rpc_port),
            ("OUTBE_CONSENSUS_PORT", self.consensus_port),
            ("OUTBE_RETH_P2P_PORT", self.reth_port),
            ("OUTBE_RETH_DISCOVERY_PORT", self.discv5_port),
            ("OUTBE_RADICLE_PORT", self.radicle_port),
        ];
        let mut seen: BTreeMap<u16, &'static str> = BTreeMap::new();
        for (name, port) in entries {
            ensure!(port > 0, "{name} must be in 1..=65535, got 0");
            ensure!(
                !FIXED_GUEST_PORTS.contains(&port),
                "{name} ({port}) collides with a fixed service port"
            );
            if let Some(prev) = seen.insert(port, name) {
                bail!("{name} ({port}) collides with {prev} ({port})");
            }
        }
        Ok(())
    }
}

fn parse_port_env(values: &BTreeMap<String, String>, key: &str) -> Result<u16> {
    let raw = values
        .get(key)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| eyre::eyre!("missing {key} in validator.env"))?;
    ensure!(
        raw.bytes().all(|b| b.is_ascii_digit()),
        "{key} must be a decimal port in 1..=65535, got `{raw}`"
    );
    let port: u16 = raw
        .parse()
        .wrap_err_with(|| format!("{key} must be in 1..=65535, got `{raw}`"))?;
    ensure!(port > 0, "{key} must be in 1..=65535, got 0");
    Ok(port)
}

/// Validated static IPv4 configuration for one Outbe VM from `validator.env`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StaticVmNetworkConfig {
    pub address: Ipv4Addr,
    pub prefix_len: u8,
    pub gateway: Ipv4Addr,
    pub dns: Vec<Ipv4Addr>,
}

impl StaticVmNetworkConfig {
    /// Parse and strictly validate static VM network keys from `validator.env`.
    pub fn from_env(values: &BTreeMap<String, String>) -> Result<Self> {
        let raw_ip = values
            .get("OUTBE_VM_IP")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| eyre::eyre!("missing OUTBE_VM_IP in validator.env"))?;
        let address: Ipv4Addr = raw_ip
            .parse()
            .wrap_err("OUTBE_VM_IP must be an IPv4 literal")?;

        let raw_prefix = values
            .get("OUTBE_VM_PREFIX")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| eyre::eyre!("missing OUTBE_VM_PREFIX in validator.env"))?;
        ensure!(
            raw_prefix.bytes().all(|b| b.is_ascii_digit()),
            "OUTBE_VM_PREFIX must be a decimal integer in 1..=30, got `{raw_prefix}`"
        );
        let prefix_len: u8 = raw_prefix
            .parse()
            .wrap_err_with(|| format!("invalid OUTBE_VM_PREFIX `{raw_prefix}`"))?;

        let raw_gateway = values
            .get("OUTBE_VM_GATEWAY")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| eyre::eyre!("missing OUTBE_VM_GATEWAY in validator.env"))?;
        let gateway: Ipv4Addr = raw_gateway
            .parse()
            .wrap_err("OUTBE_VM_GATEWAY must be an IPv4 literal")?;

        let raw_dns = values
            .get("OUTBE_VM_DNS")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| eyre::eyre!("missing OUTBE_VM_DNS in validator.env"))?;
        let dns = parse_dns_list(raw_dns)?;

        let cfg = Self {
            address,
            prefix_len,
            gateway,
            dns,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// Strictly validate address, prefix length, gateway, and DNS servers.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.address.is_loopback() && self.address.is_private(),
            "VM static IP (OUTBE_VM_IP) must be a non-loopback private IPv4 address, got {}",
            self.address
        );
        let (network, broadcast) = subnet_bounds(self.address, self.prefix_len)?;
        ensure!(
            self.address != network && self.address != broadcast,
            "VM static IP {} cannot be the network ({network}) or broadcast ({broadcast}) address of {network}/{}",
            self.address,
            self.prefix_len
        );

        ensure!(
            !self.gateway.is_unspecified()
                && !self.gateway.is_broadcast()
                && !self.gateway.is_multicast()
                && !self.gateway.is_loopback(),
            "OUTBE_VM_GATEWAY must be a valid unicast IPv4 address, got {}",
            self.gateway
        );
        let mask = !0u32 << (32 - self.prefix_len);
        ensure!(
            (u32::from(self.gateway) & mask) == u32::from(network),
            "OUTBE_VM_GATEWAY {} is outside VM subnet {network}/{}",
            self.gateway,
            self.prefix_len
        );
        ensure!(
            self.gateway != network && self.gateway != broadcast,
            "OUTBE_VM_GATEWAY {} cannot be the network ({network}) or broadcast ({broadcast}) address of {network}/{}",
            self.gateway,
            self.prefix_len
        );
        ensure!(
            self.gateway != self.address,
            "OUTBE_VM_GATEWAY {} cannot equal VM static IP {}",
            self.gateway,
            self.address
        );

        ensure!(
            !self.dns.is_empty(),
            "OUTBE_VM_DNS must list at least one IPv4 DNS server"
        );
        let mut seen = BTreeSet::new();
        for server in &self.dns {
            validate_dns_ip(*server)?;
            ensure!(
                seen.insert(*server),
                "duplicate DNS server {server} in OUTBE_VM_DNS"
            );
        }
        Ok(())
    }
}

/// Parse a strict comma-separated list of IPv4 DNS servers (no whitespace, no empty items, no duplicates).
pub fn parse_dns_list(raw: &str) -> Result<Vec<Ipv4Addr>> {
    ensure!(
        !raw.is_empty(),
        "OUTBE_VM_DNS must list at least one IPv4 DNS server"
    );
    ensure!(
        !raw.chars().any(char::is_whitespace),
        "OUTBE_VM_DNS must not contain whitespace: `{raw}`"
    );
    let mut servers = Vec::new();
    let mut seen = BTreeSet::new();
    for item in raw.split(',') {
        ensure!(
            !item.is_empty(),
            "OUTBE_VM_DNS contains an empty entry: `{raw}`"
        );
        let ip: Ipv4Addr = item
            .parse()
            .wrap_err_with(|| format!("invalid IPv4 address `{item}` in OUTBE_VM_DNS"))?;
        validate_dns_ip(ip)?;
        ensure!(seen.insert(ip), "duplicate DNS server {ip} in OUTBE_VM_DNS");
        servers.push(ip);
    }
    Ok(servers)
}

/// Format a validated DNS server list for `validator.env`.
pub fn format_dns_list(servers: &[Ipv4Addr]) -> String {
    servers
        .iter()
        .map(Ipv4Addr::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn validate_dns_ip(ip: Ipv4Addr) -> Result<()> {
    ensure!(
        !ip.is_unspecified() && !ip.is_broadcast() && !ip.is_multicast(),
        "OUTBE_VM_DNS contains invalid IPv4 server address {ip}"
    );
    Ok(())
}

/// Summary of the applied static VM network configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedStaticNetwork {
    pub iface: String,
    pub address: Ipv4Addr,
    pub prefix_len: u8,
    pub gateway: Ipv4Addr,
    pub dns: Vec<Ipv4Addr>,
    pub dropin_path: PathBuf,
}

/// Runtime timing and filesystem configuration for [`configure_static_network_with_backend`].
#[derive(Clone, Debug)]
pub struct StaticNetworkOptions {
    pub networkd_run_dir: PathBuf,
    pub network_file_name: String,
    pub apply_wait_timeout: Duration,
    pub apply_poll_interval: Duration,
}

impl Default for StaticNetworkOptions {
    fn default() -> Self {
        Self {
            networkd_run_dir: PathBuf::from(DEFAULT_NETWORKD_RUN_DIR),
            network_file_name: BASE_NETWORK_FILE_NAME.to_owned(),
            apply_wait_timeout: DEFAULT_APPLY_WAIT_TIMEOUT,
            apply_poll_interval: DEFAULT_APPLY_POLL_INTERVAL,
        }
    }
}

/// Injectable guest networking backend so static drop-in rendering, reload, DAD, and default route
/// readiness logic can be unit-tested offline against fixtures.
pub trait GuestNetBackend {
    fn ip_link_show(&self) -> Result<String>;
    fn ip_addr_show(&self) -> Result<String>;
    fn ip_route_show(&self, iface: &str) -> Result<String>;
    fn networkctl_reload(&self) -> Result<()>;
    fn sleep(&self, duration: Duration);
}

/// Live guest backend executing `ip` and `networkctl` from the measured image.
pub struct LiveGuestNetBackend;

impl GuestNetBackend for LiveGuestNetBackend {
    fn ip_link_show(&self) -> Result<String> {
        run_capture("ip", &["-j", "link", "show"])
    }

    fn ip_addr_show(&self) -> Result<String> {
        run_capture("ip", &["-j", "-4", "addr", "show"])
    }

    fn ip_route_show(&self, iface: &str) -> Result<String> {
        run_capture("ip", &["-j", "-4", "route", "show", "dev", iface])
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

/// Configure plain static IPv4 addressing on the live guest's bridge interface.
pub fn configure_static_network(config: &StaticVmNetworkConfig) -> Result<AppliedStaticNetwork> {
    configure_static_network_with_backend(
        config,
        &LiveGuestNetBackend,
        &StaticNetworkOptions::default(),
    )
}

/// Configure plain static IPv4 addressing using an injectable [`GuestNetBackend`].
pub fn configure_static_network_with_backend(
    config: &StaticVmNetworkConfig,
    backend: &dyn GuestNetBackend,
    options: &StaticNetworkOptions,
) -> Result<AppliedStaticNetwork> {
    config.validate()?;

    let link_json = backend.ip_link_show()?;
    let iface = parse_primary_interface(&link_json)?.ok_or_else(|| {
        eyre::eyre!("no en*/eth* network interface found for static VM IP configuration")
    })?;

    let dropin_content = render_static_network_dropin(config);
    let dropin_path = write_networkd_dropin_secure(
        &options.networkd_run_dir,
        &options.network_file_name,
        &dropin_content,
    )?;

    backend
        .networkctl_reload()
        .wrap_err("failed to reload systemd-networkd after writing static network drop-in")?;

    wait_for_static_network_ready(backend, &iface, config, options)?;

    Ok(AppliedStaticNetwork {
        iface,
        address: config.address,
        prefix_len: config.prefix_len,
        gateway: config.gateway,
        dns: config.dns.clone(),
        dropin_path,
    })
}

fn is_candidate_bridge_iface(name: &str) -> bool {
    name.starts_with("eth") || name.starts_with("en")
}

/// Parse `ip -j link show` and return the first `en*`/`eth*` interface name.
pub fn parse_primary_interface(raw_json: &str) -> Result<Option<String>> {
    let parsed: Value = serde_json::from_str(raw_json).wrap_err("parse `ip -j link show` JSON")?;
    let links = parsed
        .as_array()
        .ok_or_else(|| eyre::eyre!("`ip -j link show` JSON root is not an array"))?;

    for link in links {
        let Some(name) = link.get("ifname").and_then(Value::as_str) else {
            continue;
        };
        if is_candidate_bridge_iface(name) {
            return Ok(Some(name.to_owned()));
        }
    }
    Ok(None)
}

fn subnet_bounds(addr: Ipv4Addr, prefix_len: u8) -> Result<(Ipv4Addr, Ipv4Addr)> {
    ensure!(
        (1..=30).contains(&prefix_len),
        "VM IPv4 prefix length /{prefix_len} is invalid (must be /1../30)"
    );
    let mask = !0u32 << (32 - prefix_len);
    let network = Ipv4Addr::from(u32::from(addr) & mask);
    let broadcast = Ipv4Addr::from(u32::from(network) | !mask);
    Ok((network, broadcast))
}

/// Render the single runtime `systemd-networkd` drop-in configuring plain static IPv4 addressing,
/// default gateway, and DNS servers (with DHCP/RA/link-local disabled).
pub fn render_static_network_dropin(config: &StaticVmNetworkConfig) -> String {
    let mut out = concat!(
        "# Managed at runtime by outbe-vm-agent vm-boot (static IPv4).\n",
        "[Network]\n",
        "DHCP=no\n",
        "IPv6AcceptRA=no\n",
        "LinkLocalAddressing=no\n",
    )
    .to_owned();
    for server in &config.dns {
        out.push_str(&format!("DNS={server}\n"));
    }
    out.push_str(&format!(
        concat!(
            "\n",
            "[Address]\n",
            "Address={address}/{prefix_len}\n",
            "DuplicateAddressDetection=ipv4\n",
            "\n",
            "[Route]\n",
            "Destination=0.0.0.0/0\n",
            "Gateway={gateway}\n",
        ),
        address = config.address,
        prefix_len = config.prefix_len,
        gateway = config.gateway,
    ));
    out
}

/// Atomically write `<networkd_run_dir>/<network_file_name>.d/80-outbe-static.conf` refusing
/// symlinks at every path component and setting explicit `0755`/`0644` permissions (overriding
/// `outbe-vm-boot.service`'s `UMask=0077`) so `systemd-network` can read it.
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

    let dropin_path = dropin_dir_path.join(STATIC_NETWORK_DROPIN_NAME);
    match statat(
        &dropin_dir_fd,
        STATIC_NETWORK_DROPIN_NAME,
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

    let tmp_name = format!(".{STATIC_NETWORK_DROPIN_NAME}.tmp.{}", std::process::id());
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
        STATIC_NETWORK_DROPIN_NAME,
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
enum StaticAddrState {
    Absent,
    Tentative,
    DadFailed,
    Ready,
}

fn inspect_static_addr_state(
    raw_json: &str,
    iface: &str,
    address: Ipv4Addr,
    prefix_len: u8,
) -> Result<StaticAddrState> {
    let parsed: Value =
        serde_json::from_str(raw_json).wrap_err("parse `ip -j -4 addr show` JSON")?;
    let ifaces = parsed
        .as_array()
        .ok_or_else(|| eyre::eyre!("`ip -j -4 addr show` JSON root is not an array"))?;

    let target_str = address.to_string();
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
            if local == target_str && plen == Some(prefix_len) {
                if addr.get("dadfailed").and_then(Value::as_bool) == Some(true) {
                    return Ok(StaticAddrState::DadFailed);
                }
                if addr.get("tentative").and_then(Value::as_bool) == Some(true) {
                    return Ok(StaticAddrState::Tentative);
                }
                return Ok(StaticAddrState::Ready);
            }
        }
    }
    Ok(StaticAddrState::Absent)
}

fn has_default_gateway_route(raw_json: &str, gateway: Ipv4Addr) -> Result<bool> {
    let parsed: Value =
        serde_json::from_str(raw_json).wrap_err("parse `ip -j -4 route show` JSON")?;
    let routes = parsed
        .as_array()
        .ok_or_else(|| eyre::eyre!("`ip -j -4 route show` JSON root is not an array"))?;

    let gw_str = gateway.to_string();
    Ok(routes.iter().any(|route| {
        route.get("dst").and_then(Value::as_str) == Some("default")
            && route.get("gateway").and_then(Value::as_str) == Some(gw_str.as_str())
    }))
}

fn wait_for_static_network_ready(
    backend: &dyn GuestNetBackend,
    iface: &str,
    config: &StaticVmNetworkConfig,
    options: &StaticNetworkOptions,
) -> Result<()> {
    let deadline = Instant::now() + options.apply_wait_timeout;

    loop {
        let addr_json = backend.ip_addr_show()?;
        let addr_state =
            inspect_static_addr_state(&addr_json, iface, config.address, config.prefix_len)?;
        let last_status = match addr_state {
            StaticAddrState::DadFailed => {
                bail!(
                    "IPv4 Duplicate Address Detection (DAD) conflict for static IP {}/{} on {iface}: another host on the bridge already claims this address",
                    config.address,
                    config.prefix_len
                );
            }
            StaticAddrState::Ready => {
                let route_json = backend.ip_route_show(iface)?;
                if has_default_gateway_route(&route_json, config.gateway)? {
                    return Ok(());
                }
                format!(
                    "static IP {}/{} is up on {iface}, waiting for default route via {}",
                    config.address, config.prefix_len, config.gateway
                )
            }
            StaticAddrState::Tentative => format!(
                "static IP {}/{} on {iface} is still tentative (IPv4 DAD in progress)",
                config.address, config.prefix_len
            ),
            StaticAddrState::Absent => format!(
                "static IP {}/{} not yet present on {iface} after networkctl reload",
                config.address, config.prefix_len
            ),
        };

        if Instant::now() >= deadline {
            bail!(
                "timed out after {}s waiting for static VM IP {}/{} on {iface} to become ready: {last_status}",
                options.apply_wait_timeout.as_secs(),
                config.address,
                config.prefix_len
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

    fn sample_config() -> StaticVmNetworkConfig {
        StaticVmNetworkConfig {
            address: Ipv4Addr::new(10, 77, 0, 80),
            prefix_len: 24,
            gateway: Ipv4Addr::new(10, 77, 0, 1),
            dns: vec![Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)],
        }
    }

    fn fixture_link_eth0() -> String {
        json!([
            { "ifindex": 1, "ifname": "lo", "flags": ["LOOPBACK", "UP"], "operstate": "UNKNOWN" },
            { "ifindex": 2, "ifname": "eth0", "flags": ["BROADCAST", "MULTICAST", "UP", "LOWER_UP"], "operstate": "UP" }
        ])
        .to_string()
    }

    fn fixture_addr(ip: &str, prefix_len: u8, tentative: bool, dadfailed: bool) -> String {
        let mut entry = json!({
            "family": "inet",
            "local": ip,
            "prefixlen": prefix_len,
            "scope": "global"
        });
        if tentative {
            entry["tentative"] = json!(true);
        }
        if dadfailed {
            entry["dadfailed"] = json!(true);
        }
        json!([
            {
                "ifindex": 2,
                "ifname": "eth0",
                "flags": ["BROADCAST", "MULTICAST", "UP", "LOWER_UP"],
                "operstate": "UP",
                "addr_info": [entry]
            }
        ])
        .to_string()
    }

    fn fixture_routes_ready(gw: &str) -> String {
        json!([
            { "dst": "default", "gateway": gw, "protocol": "static" },
            { "dst": "10.77.0.0/24", "protocol": "kernel", "scope": "link", "prefsrc": "10.77.0.80" }
        ])
        .to_string()
    }

    struct FakeGuestNet {
        links: String,
        addr_sequence: Mutex<Vec<String>>,
        route_json: String,
        reloads: Mutex<usize>,
    }

    impl FakeGuestNet {
        fn new(addr_sequence: Vec<String>, route_json: String) -> Self {
            Self {
                links: fixture_link_eth0(),
                addr_sequence: Mutex::new(addr_sequence),
                route_json,
                reloads: Mutex::new(0),
            }
        }
    }

    impl GuestNetBackend for FakeGuestNet {
        fn ip_link_show(&self) -> Result<String> {
            Ok(self.links.clone())
        }

        fn ip_addr_show(&self) -> Result<String> {
            let mut seq = self.addr_sequence.lock().unwrap();
            if seq.len() > 1 {
                Ok(seq.remove(0))
            } else {
                Ok(seq[0].clone())
            }
        }

        fn ip_route_show(&self, _iface: &str) -> Result<String> {
            Ok(self.route_json.clone())
        }

        fn networkctl_reload(&self) -> Result<()> {
            *self.reloads.lock().unwrap() += 1;
            Ok(())
        }

        fn sleep(&self, _duration: Duration) {}
    }

    #[test]
    fn from_env_validates_strictly_and_rejects_invalid_inputs() {
        let mut env = BTreeMap::from([
            ("OUTBE_VM_IP".to_owned(), "10.77.0.80".to_owned()),
            ("OUTBE_VM_PREFIX".to_owned(), "24".to_owned()),
            ("OUTBE_VM_GATEWAY".to_owned(), "10.77.0.1".to_owned()),
            ("OUTBE_VM_DNS".to_owned(), "1.1.1.1,8.8.8.8".to_owned()),
        ]);
        let parsed = StaticVmNetworkConfig::from_env(&env).unwrap();
        assert_eq!(parsed, sample_config());

        for (key, bad_val, expected_err) in [
            ("OUTBE_VM_IP", "127.0.0.1", "non-loopback private"),
            ("OUTBE_VM_IP", "198.51.100.10", "non-loopback private"),
            ("OUTBE_VM_IP", "10.77.0.0", "network"),
            ("OUTBE_VM_IP", "10.77.0.255", "broadcast"),
            ("OUTBE_VM_PREFIX", "0", "prefix length"),
            ("OUTBE_VM_PREFIX", "31", "prefix length"),
            ("OUTBE_VM_PREFIX", "32", "prefix length"),
            ("OUTBE_VM_PREFIX", "+24", "decimal integer"),
            ("OUTBE_VM_GATEWAY", "10.78.0.1", "outside VM subnet"),
            (
                "OUTBE_VM_GATEWAY",
                "10.77.0.80",
                "cannot equal VM static IP",
            ),
            ("OUTBE_VM_GATEWAY", "10.77.0.0", "network"),
            ("OUTBE_VM_DNS", "", "missing OUTBE_VM_DNS"),
            ("OUTBE_VM_DNS", "1.1.1.1, 8.8.8.8", "whitespace"),
            ("OUTBE_VM_DNS", "1.1.1.1,,8.8.8.8", "empty entry"),
            ("OUTBE_VM_DNS", "1.1.1.1,1.1.1.1", "duplicate DNS"),
            ("OUTBE_VM_DNS", "0.0.0.0", "invalid IPv4"),
            ("OUTBE_VM_DNS", "224.0.0.1", "invalid IPv4"),
            ("OUTBE_VM_DNS", "not-an-ip", "invalid IPv4"),
        ] {
            let mut bad = env.clone();
            bad.insert(key.to_owned(), bad_val.to_owned());
            let err = StaticVmNetworkConfig::from_env(&bad)
                .expect_err(&format!("expected failure for {key}={bad_val}"));
            assert!(
                format!("{err:#}").contains(expected_err),
                "for {key}={bad_val}, expected `{expected_err}`, got: {err:#}"
            );
        }

        // Missing any of the four static keys must fail closed (no defaults).
        for required_key in [
            "OUTBE_VM_IP",
            "OUTBE_VM_PREFIX",
            "OUTBE_VM_GATEWAY",
            "OUTBE_VM_DNS",
        ] {
            let mut missing = env.clone();
            missing.remove(required_key);
            let err = StaticVmNetworkConfig::from_env(&missing).unwrap_err();
            assert!(err.to_string().contains(required_key), "{err}");
        }
        env.clear();
        assert!(StaticVmNetworkConfig::from_env(&env).is_err());
    }

    #[test]
    fn render_static_dropin_has_no_dhcp_and_no_preferred_source() {
        let dropin = render_static_network_dropin(&sample_config());
        assert!(dropin.contains("DHCP=no\n"));
        assert!(dropin.contains("IPv6AcceptRA=no\n"));
        assert!(dropin.contains("LinkLocalAddressing=no\n"));
        assert!(dropin.contains("DNS=1.1.1.1\n"));
        assert!(dropin.contains("DNS=8.8.8.8\n"));
        assert!(dropin.contains("Address=10.77.0.80/24\n"));
        assert!(dropin.contains("DuplicateAddressDetection=ipv4\n"));
        assert!(dropin.contains("Destination=0.0.0.0/0\n"));
        assert!(dropin.contains("Gateway=10.77.0.1\n"));
        assert!(!dropin.contains("PreferredSource"));
        assert!(!dropin.contains("Metric="));
    }

    #[test]
    fn configure_static_network_writes_dropin_reloads_and_waits_for_dad_and_route() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("network");
        fs::create_dir(&run_dir).unwrap();

        let backend = FakeGuestNet::new(
            vec![
                "[]".to_owned(),
                fixture_addr("10.77.0.80", 24, true, false),
                fixture_addr("10.77.0.80", 24, false, false),
            ],
            fixture_routes_ready("10.77.0.1"),
        );
        let opts = StaticNetworkOptions {
            networkd_run_dir: run_dir.clone(),
            network_file_name: BASE_NETWORK_FILE_NAME.to_owned(),
            apply_wait_timeout: Duration::from_secs(5),
            apply_poll_interval: Duration::from_millis(1),
        };

        let applied =
            configure_static_network_with_backend(&sample_config(), &backend, &opts).unwrap();
        assert_eq!(applied.iface, "eth0");
        assert_eq!(applied.address, Ipv4Addr::new(10, 77, 0, 80));
        assert_eq!(applied.prefix_len, 24);
        assert_eq!(applied.gateway, Ipv4Addr::new(10, 77, 0, 1));
        assert_eq!(*backend.reloads.lock().unwrap(), 1);

        let expected_path = run_dir.join("20-wired.network.d/80-outbe-static.conf");
        assert_eq!(applied.dropin_path, expected_path);
        assert_eq!(
            fs::metadata(&expected_path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(
            fs::metadata(expected_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        let written = fs::read_to_string(&expected_path).unwrap();
        assert!(written.contains("Address=10.77.0.80/24"));
        assert!(written.contains("Gateway=10.77.0.1"));
        assert!(written.contains("DNS=1.1.1.1"));
    }

    #[test]
    fn configure_static_network_fails_closed_on_dad_conflict_timeout_and_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("network");
        fs::create_dir(&run_dir).unwrap();
        let opts = StaticNetworkOptions {
            networkd_run_dir: run_dir.clone(),
            network_file_name: BASE_NETWORK_FILE_NAME.to_owned(),
            apply_wait_timeout: Duration::from_millis(20),
            apply_poll_interval: Duration::from_millis(5),
        };

        // 1. DAD conflict fails immediately.
        let dad_backend = FakeGuestNet::new(
            vec![fixture_addr("10.77.0.80", 24, false, true)],
            fixture_routes_ready("10.77.0.1"),
        );
        let dad_err = configure_static_network_with_backend(&sample_config(), &dad_backend, &opts)
            .unwrap_err();
        assert!(dad_err.to_string().contains("DAD"), "{dad_err}");

        // 2. Missing default route times out cleanly.
        let no_route_backend = FakeGuestNet::new(
            vec![fixture_addr("10.77.0.80", 24, false, false)],
            "[]".to_owned(),
        );
        let timeout_err =
            configure_static_network_with_backend(&sample_config(), &no_route_backend, &opts)
                .unwrap_err();
        assert!(
            timeout_err.to_string().contains("timed out")
                && timeout_err.to_string().contains("default route"),
            "{timeout_err}"
        );

        // 3. Symlink drop-in directory is refused.
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let sym_run = tmp.path().join("sym-network");
        fs::create_dir(&sym_run).unwrap();
        symlink(&outside, sym_run.join("20-wired.network.d")).unwrap();
        let sym_err =
            write_networkd_dropin_secure(&sym_run, BASE_NETWORK_FILE_NAME, "test").unwrap_err();
        assert!(sym_err.to_string().contains("symlink"), "{sym_err}");
    }

    #[test]
    fn vm_ports_from_env_validates_strictly_and_rejects_collisions() {
        let valid = BTreeMap::from([
            ("OUTBE_RPC_PORT".to_owned(), "8546".to_owned()),
            ("OUTBE_CONSENSUS_PORT".to_owned(), "30401".to_owned()),
            ("OUTBE_RETH_P2P_PORT".to_owned(), "30304".to_owned()),
            ("OUTBE_RETH_DISCOVERY_PORT".to_owned(), "31304".to_owned()),
            ("OUTBE_RADICLE_PORT".to_owned(), "8777".to_owned()),
        ]);
        let parsed = VmPortsConfig::from_env(&valid).unwrap();
        assert_eq!(parsed.rpc_port, 8_546);
        assert_eq!(parsed.consensus_port, 30_401);
        assert_eq!(parsed.reth_port, 30_304);
        assert_eq!(parsed.discv5_port, 31_304);
        assert_eq!(parsed.radicle_port, 8_777);

        for missing_key in [
            "OUTBE_RPC_PORT",
            "OUTBE_CONSENSUS_PORT",
            "OUTBE_RETH_P2P_PORT",
            "OUTBE_RETH_DISCOVERY_PORT",
            "OUTBE_RADICLE_PORT",
        ] {
            let mut m = valid.clone();
            m.remove(missing_key);
            assert!(VmPortsConfig::from_env(&m)
                .unwrap_err()
                .to_string()
                .contains(missing_key));
        }

        for (key, bad, expect) in [
            ("OUTBE_RPC_PORT", "0", "1..=65535"),
            ("OUTBE_RPC_PORT", "8551", "fixed service port"),
            ("OUTBE_RPC_PORT", "9000", "fixed service port"),
            ("OUTBE_RPC_PORT", "30401", "collides with"),
            ("OUTBE_CONSENSUS_PORT", "0", "1..=65535"),
            ("OUTBE_RETH_P2P_PORT", "70000", "1..=65535"),
            ("OUTBE_RETH_DISCOVERY_PORT", "+31303", "decimal port"),
            ("OUTBE_RADICLE_PORT", "8551", "fixed service port"),
            ("OUTBE_RADICLE_PORT", "9000", "fixed service port"),
            ("OUTBE_RADICLE_PORT", "8546", "collides with"),
            ("OUTBE_RETH_P2P_PORT", "30401", "collides with"),
        ] {
            let mut m = valid.clone();
            m.insert(key.to_owned(), bad.to_owned());
            let err = VmPortsConfig::from_env(&m).unwrap_err();
            assert!(
                format!("{err:#}").contains(expect),
                "for {key}={bad}, expected `{expect}`, got: {err:#}"
            );
        }
    }
}
