//! Host probes: the kernel's ephemeral range and loopback bindability.

use std::net::{TcpListener, TcpStream, UdpSocket};

use eyre::{bail, Result, WrapErr};

use super::{Proto, Service};

#[cfg(not(target_os = "macos"))]
pub(super) fn ephemeral_port_range() -> Result<(u16, u16)> {
    let path = "/proc/sys/net/ipv4/ip_local_port_range";
    let value = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("read {path} for Radicle port allocation"))?;
    parse_ephemeral_port_range(&value).wrap_err_with(|| format!("parse {path}"))
}

#[cfg(target_os = "macos")]
pub(super) fn ephemeral_port_range() -> Result<(u16, u16)> {
    let output = std::process::Command::new("/usr/sbin/sysctl")
        .args([
            "-n",
            "net.inet.ip.portrange.first",
            "net.inet.ip.portrange.last",
        ])
        .output()
        .wrap_err("read macOS ephemeral port range for Radicle port allocation")?;
    if !output.status.success() {
        bail!(
            "read macOS ephemeral port range: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let value =
        std::str::from_utf8(&output.stdout).wrap_err("decode macOS ephemeral port range")?;
    parse_ephemeral_port_range(value).wrap_err("parse macOS ephemeral port range")
}

pub(super) fn parse_ephemeral_port_range(value: &str) -> Result<(u16, u16)> {
    let ports = value
        .split_whitespace()
        .map(str::parse::<u16>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let [low, high] = ports.as_slice() else {
        bail!("ephemeral port range must contain exactly two ports");
    };
    if *low == 0 || low > high {
        bail!("invalid ephemeral port range {low}..={high}");
    }
    Ok((*low, *high))
}

/// Whether every port of the block starting at `start` is bindable.
pub(super) fn window_free(start: u16) -> bool {
    Service::CORE
        .iter()
        .all(|svc| is_free(start + svc.offset(), svc.proto()))
}

pub(super) fn radicle_window_free(start: u16) -> bool {
    Service::RADICLE
        .iter()
        .all(|svc| is_free(start + svc.offset(), svc.proto()))
}

/// Whether `port` is bindable on loopback for the required transport(s).
///
/// A bind probe alone is not enough for TCP. Nodes bind their service ports on
/// the wildcard address, and BSD `SO_REUSEADDR` semantics let a later
/// loopback-specific bind succeed over a live `0.0.0.0` listener. So ask for a
/// connection first. An answer proves that somebody owns the port, whatever
/// address they bound. Only then try to bind, so the probe's own listener can
/// never answer its own connect.
pub(super) fn is_free(port: u16, proto: Proto) -> bool {
    let tcp = || {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return false;
        }
        TcpListener::bind(("127.0.0.1", port)).is_ok()
    };
    let udp = || UdpSocket::bind(("127.0.0.1", port)).is_ok();
    match proto {
        Proto::Tcp => tcp(),
        Proto::Udp => udp(),
        Proto::TcpUdp => tcp() && udp(),
    }
}
