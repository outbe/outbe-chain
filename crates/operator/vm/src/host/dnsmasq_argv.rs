//! Parse the DHCP settings of a dnsmasq command line.

use std::{net::Ipv4Addr, path::PathBuf};

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

/// The dnsmasq options that host inspection reads.
#[derive(Clone, Copy)]
enum DnsmasqOption {
    Interface,
    HostsFile,
    LeasesFile,
    PidFile,
    DhcpRange,
}

/// Each option with its long name and its optional short name.
const DNSMASQ_OPTIONS: [(DnsmasqOption, &str, Option<&str>); 5] = [
    (DnsmasqOption::Interface, "--interface", Some("-i")),
    (DnsmasqOption::HostsFile, "--dhcp-hostsfile", None),
    (DnsmasqOption::LeasesFile, "--dhcp-leasefile", None),
    (DnsmasqOption::PidFile, "--pid-file", Some("-x")),
    (DnsmasqOption::DhcpRange, "--dhcp-range", Some("-F")),
];

pub(crate) fn parse_dnsmasq_argv(argv: &[String]) -> ParsedDnsmasqArgs {
    let mut out = ParsedDnsmasqArgs::default();
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        let Some((option, inline_value)) = classify_dnsmasq_arg(arg) else {
            continue;
        };
        // A separate-value option takes the next argument when one exists.
        if let Some(value) = inline_value.or_else(|| args.next().map(String::as_str)) {
            out.apply(option, value);
        }
    }
    out
}

/// Match `arg` against the known options. An `--option=value` argument returns its inline
/// value. An `--option` or short argument returns `None` as the value, because the value is
/// the next argument.
fn classify_dnsmasq_arg(arg: &str) -> Option<(DnsmasqOption, Option<&str>)> {
    DNSMASQ_OPTIONS.iter().find_map(|&(option, long, short)| {
        if let Some(value) = arg
            .strip_prefix(long)
            .and_then(|rest| rest.strip_prefix('='))
        {
            return Some((option, Some(value)));
        }
        (arg == long || short == Some(arg)).then_some((option, None))
    })
}

impl ParsedDnsmasqArgs {
    fn apply(&mut self, option: DnsmasqOption, value: &str) {
        match option {
            DnsmasqOption::Interface => self.interface = Some(value.to_owned()),
            DnsmasqOption::HostsFile => self.hosts_file = Some(PathBuf::from(value)),
            DnsmasqOption::LeasesFile => self.leases_file = Some(PathBuf::from(value)),
            DnsmasqOption::PidFile => self.pid_file = Some(PathBuf::from(value)),
            DnsmasqOption::DhcpRange => parse_dhcp_range(value, self),
        }
    }
}

fn parse_dhcp_range(raw: &str, out: &mut ParsedDnsmasqArgs) {
    let parts: Vec<&str> = raw.split(',').map(str::trim).collect();
    let mut ips = Vec::new();
    let mut tail = None;
    for part in &parts {
        if let Ok(ip) = part.parse::<Ipv4Addr>() {
            ips.push(ip);
        } else if is_dhcp_range_tail(part) {
            tail = Some((*part).to_owned());
        }
    }
    if ips.len() >= 2 {
        out.pool_start = Some(ips[0]);
        out.pool_end = Some(ips[1]);
        out.netmask = ips.get(2).copied().or(out.netmask);
    }
    if let Some(lease) = tail {
        out.lease_duration = Some(lease);
    }
}

/// Returns true if a non-address `--dhcp-range` field is a lease duration and not a tag.
fn is_dhcp_range_tail(part: &str) -> bool {
    !part.is_empty() && !part.starts_with("set:") && !part.starts_with("tag:")
}
