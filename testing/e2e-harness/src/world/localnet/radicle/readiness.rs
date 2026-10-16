//! Radicle sidecar readiness: owned child, native identity, and status TCP.

use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use eyre::{bail, ensure, eyre, Result, WrapErr};
use outbe_radicle::endpoint::EndpointAddress;
use outbe_radicle::integration::query_sidecar;

use super::{NATIVE_CONTROL_DEADLINE, RADICLE_READY_POLL};
use crate::internal::eth::block_on;
use crate::internal::proc::ChildGuard;

/// What one sidecar must prove before its owner hands it to the scenario.
pub(super) struct RadicleReadiness<'a> {
    pub(super) expected_node_id: [u8; 32],
    pub(super) control_socket: &'a Path,
    pub(super) peer: SocketAddr,
    pub(super) status: SocketAddr,
    pub(super) log: &'a Path,
}

/// Decode only the provisioned OpenSSH public file, without a CLI profile or
/// any fallback to the private key. Metadata errors and symlinks fail closed.
pub(super) fn provisioned_radicle_node_id(home: &Path) -> Result<[u8; 32]> {
    let public = home.join("keys/radicle.pub");
    let metadata = fs::symlink_metadata(&public)
        .wrap_err_with(|| format!("inspect Radicle public identity {}", public.display()))?;
    ensure!(
        metadata.file_type().is_file(),
        "Radicle public identity must be a regular non-symlink file: {}",
        public.display()
    );
    radicle::crypto::ssh::Keystore::new(&home.join("keys"))
        .public_key()
        .wrap_err_with(|| format!("decode Radicle public identity {}", public.display()))?
        .map(radicle::crypto::PublicKey::into_inner)
        .ok_or_else(|| eyre!("Radicle public identity disappeared: {}", public.display()))
}

/// A TCP listener alone is not readiness. Retain ownership in the caller until
/// native identity/config, status TCP and the final child observation succeed.
/// Each blocking observation consumes the same deadline. There is no per-probe
/// reset.
pub(super) fn wait_radicle_ready(
    guard: &mut ChildGuard,
    target: &RadicleReadiness<'_>,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let pid = guard.pid();
    poll_radicle_ready(guard, target, deadline).wrap_err_with(|| {
        let exit = guard.exit_status();
        format!(
            "Radicle readiness failed: pid={pid}, node_id={}, peer={}, status={}, control={}, log={}, exit={exit:?}",
            native_node_id(target.expected_node_id),
            target.peer,
            target.status,
            target.control_socket.display(),
            target.log.display()
        )
    })
}

fn poll_radicle_ready(
    guard: &mut ChildGuard,
    target: &RadicleReadiness<'_>,
    deadline: Instant,
) -> Result<()> {
    let mut last_error = "native control not yet observed".to_owned();
    loop {
        if let Some(exit) = guard.exit_status()? {
            bail!("owned Radicle child exited: {exit}; last observation: {last_error}");
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure!(
            !remaining.is_zero(),
            "Radicle readiness deadline: {last_error}"
        );
        let observation = observe_radicle_once(target, deadline, remaining)?;
        if let Err(pending) = &observation {
            last_error.clone_from(pending);
        }
        if let Some(exit) = guard.exit_status()? {
            bail!("owned Radicle child exited during readiness: {exit}; last observation: {last_error}");
        }
        ensure!(
            Instant::now() < deadline,
            "Radicle readiness deadline: {last_error}"
        );
        if observation.is_ok() {
            return Ok(());
        }
        std::thread::sleep(
            RADICLE_READY_POLL.min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

/// One readiness observation. The inner `Err` is a pending observation to
/// retry; the outer `Err` is a fatal identity or endpoint mismatch.
fn observe_radicle_once(
    target: &RadicleReadiness<'_>,
    deadline: Instant,
    remaining: Duration,
) -> Result<Result<(), String>> {
    let native = block_on(query_sidecar(
        target.control_socket.to_path_buf(),
        remaining.min(NATIVE_CONTROL_DEADLINE),
    ));
    let info = match native {
        Ok(info) => info,
        Err(error) => return Ok(Err(format!("native control observation failed: {error}"))),
    };
    ensure!(
        info.node_id == target.expected_node_id,
        "native Radicle NodeId differs from the provisioned public identity"
    );
    let addresses = info
        .addresses
        .iter()
        .map(native_address)
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        addresses == [target.peer.to_string()],
        "native Radicle advertised addresses differ from the requested listen endpoint"
    );
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Ok(Err(
            "native observation exhausted the readiness deadline".to_owned()
        ));
    }
    Ok(
        TcpStream::connect_timeout(&target.status, remaining.min(RADICLE_READY_POLL))
            .map(drop)
            .map_err(|error| format!("status TCP observation failed: {error}")),
    )
}

pub(super) fn native_node_id(node_id: [u8; 32]) -> String {
    let mut bytes = [0; 34];
    bytes[..2].copy_from_slice(&[0xed, 0x01]);
    bytes[2..].copy_from_slice(&node_id);
    multibase::encode(multibase::Base::Base58Btc, bytes)
}

pub(super) fn native_address(address: &EndpointAddress) -> Result<String> {
    let encoded = address.encode();
    match encoded.as_slice() {
        [0, a, b, c, d, ..] => Ok(format!("{a}.{b}.{c}.{d}:{}", address.port())),
        [1, octets @ .., _, _] if octets.len() == 16 => {
            let octets: [u8; 16] = octets
                .try_into()
                .map_err(|_| eyre!("invalid IPv6 endpoint"))?;
            Ok(format!(
                "[{}]:{}",
                std::net::Ipv6Addr::from(octets),
                address.port()
            ))
        }
        _ => address
            .host()
            .map(|host| format!("{host}:{}", address.port()))
            .ok_or_else(|| eyre!("invalid native Radicle endpoint")),
    }
}
