//! Parse the sentinel-framed output of the read-only NAT preflight command.

use std::net::Ipv4Addr;

use eyre::{bail, ensure, Context, Result};

use super::{
    commands::validate_iface_name, inspect_nft_json, parse_persistent_sysctl, FileOwnership,
    Ipv4Subnet, ManagedFileStates, NftInspection, PersistentSysctlState, SENTINEL_BRIDGE_ADDR,
    SENTINEL_IP_FORWARD, SENTINEL_NFT_JSON, SENTINEL_OWNED_FILES, SENTINEL_PERSISTENT_SYSCTL,
    SENTINEL_UPLINK_ROUTE,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreflightReport {
    pub(crate) bridge: String,
    pub(crate) bridge_addr: Ipv4Addr,
    pub(crate) subnet: Ipv4Subnet,
    pub(crate) uplink: String,
    pub(crate) ip_forward: u8,
    pub(crate) persistent_sysctl: PersistentSysctlState,
    pub(crate) files: ManagedFileStates,
    pub(crate) nft: NftInspection,
}

pub(crate) fn parse_preflight_output(
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
    ensure_founders_in_subnet(founder_ips, subnet, bridge)?;

    let uplink = parse_uplink_section(&uplink_section)?;
    validate_iface_name(&uplink, "uplink")?;
    ensure!(
        uplink != bridge,
        "detected uplink '{uplink}' is the same interface as bridge '{bridge}'; pass --uplink"
    );

    let ip_forward = parse_ip_forward(&ip_fwd_section)?;
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

fn ensure_founders_in_subnet(
    founder_ips: &[Ipv4Addr],
    subnet: Ipv4Subnet,
    bridge: &str,
) -> Result<()> {
    for ip in founder_ips {
        ensure!(
            subnet.contains(*ip),
            "founder bridge_ip {ip} is outside host bridge `{bridge}` subnet {}",
            subnet.cidr_string()
        );
    }
    Ok(())
}

/// Parse the runtime `net.ipv4.ip_forward` value. Only `0` and `1` are valid.
fn parse_ip_forward(section: &str) -> Result<u8> {
    let ip_forward: u8 = section
        .trim()
        .parse()
        .wrap_err_with(|| format!("invalid net.ipv4.ip_forward value: {:?}", section.trim()))?;
    ensure!(
        ip_forward == 0 || ip_forward == 1,
        "unexpected net.ipv4.ip_forward value: {ip_forward}"
    );
    Ok(ip_forward)
}

pub(crate) fn parse_owned_files_section(section: &str) -> Result<ManagedFileStates> {
    let mut rules = None;
    let mut service = None;
    let mut sysctl = None;
    let records = section
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| l.split_once('='));
    for (key, val) in records {
        let state = parse_file_ownership(key, val)?;
        match key {
            "rules" => rules = Some(state),
            "service" => service = Some(state),
            "sysctl" => sysctl = Some(state),
            _ => {}
        }
    }
    Ok(ManagedFileStates {
        rules: rules.ok_or_else(|| eyre::eyre!("missing rules ownership probe"))?,
        service: service.ok_or_else(|| eyre::eyre!("missing service ownership probe"))?,
        sysctl: sysctl.ok_or_else(|| eyre::eyre!("missing sysctl ownership probe"))?,
    })
}

fn parse_file_ownership(key: &str, val: &str) -> Result<FileOwnership> {
    match val {
        "absent" => Ok(FileOwnership::Absent),
        "owned" => Ok(FileOwnership::Owned),
        "foreign" => Ok(FileOwnership::Foreign),
        other => Err(eyre::eyre!(
            "unexpected file ownership state '{other}' for '{key}'"
        )),
    }
}

/// Return the lines after `start_sentinel` up to `end_sentinel` (or the end of `raw`).
fn extract_section(raw: &str, start_sentinel: &str, end_sentinel: Option<&str>) -> Result<String> {
    let mut lines = raw.lines();
    let found = lines.by_ref().any(|line| line.trim() == start_sentinel);
    ensure!(found, "preflight output missing section {start_sentinel}");
    let body: Vec<&str> = lines
        .take_while(|line| end_sentinel.is_none_or(|end| line.trim() != end))
        .collect();
    Ok(body.join("\n"))
}

/// Return the token that follows the first `keyword` token on `line`.
fn token_after<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    let mut tokens = line.split_whitespace();
    tokens.by_ref().find(|&tok| tok == keyword)?;
    tokens.next()
}

/// Parse `ip -4 -o addr show dev <bridge>` output into `(host_addr, normalized_subnet)`.
pub(super) fn parse_bridge_cidr(output: &str, bridge: &str) -> Result<(Ipv4Addr, Ipv4Subnet)> {
    let Some(cidr) = output.lines().find_map(|line| token_after(line, "inet")) else {
        bail!("no IPv4 address found on bridge interface `{bridge}`");
    };
    let (addr_str, prefix_str) = cidr
        .split_once('/')
        .ok_or_else(|| eyre::eyre!("missing CIDR prefix on bridge {bridge}: {cidr}"))?;
    let addr: Ipv4Addr = addr_str
        .parse()
        .wrap_err_with(|| format!("invalid IPv4 address on bridge {bridge}: {addr_str}"))?;
    let prefix: u8 = prefix_str
        .parse()
        .wrap_err_with(|| format!("invalid CIDR prefix on bridge {bridge}: {prefix_str}"))?;
    let subnet = Ipv4Subnet::from_addr_prefix(addr, prefix)?;
    Ok((addr, subnet))
}

/// Parse either `override <iface>` or `ip -4 -o route show default` output.
pub(super) fn parse_uplink_section(output: &str) -> Result<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .find_map(|line| {
            line.strip_prefix("override ")
                .map(str::trim)
                .or_else(|| token_after(line, "dev"))
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            eyre::eyre!(
                "could not detect IPv4 default-route uplink interface; pass --uplink <iface>"
            )
        })
}
