//! Exercise the receiver's IP admission independently of the shared loopback
//! used by localnet validator processes. A registered identity announces a
//! different source IP; probes bind that IP and send no handshake payload.
use std::{
    net::{IpAddr, SocketAddr},
    thread::sleep,
    time::{Duration, Instant},
};

use cucumber::{given, then, when};
use eyre::{ensure, eyre, Result};
use serde_json::json;

use super::dkg::{capture_dkg_owner, dkg_boundary_at, dkg_ports, record_dkg_checkpoint};
use crate::{
    internal::{addresses, eth},
    world::World,
};

const SOURCE: &str = "127.0.0.5";
const UNKNOWN_SOURCE: &str = "127.0.0.254";
const ENCODED_SOURCE: &str = "00047f00000576c0";

fn unchanged_secondary(world: &World, ports: &[u16], height: u64) -> Result<()> {
    let address = world
        .state
        .joiner_addr
        .as_ref()
        .ok_or_else(|| eyre!("missing transport identity"))?;
    let anchor = world
        .state
        .lifecycle_before
        .ok_or_else(|| eyre!("missing admission anchor"))?;
    for &port in ports {
        let before = world
            .rpc
            .validator_record_at(port, address, anchor.height)
            .ok_or_else(|| eyre!("missing anchor validator record"))?;
        let after = world
            .rpc
            .validator_record_at(port, address, height)
            .ok_or_else(|| eyre!("missing current validator record"))?;
        ensure!(
            before.status <= 1
                && after.status == before.status
                && !after.has_bls_share
                && before.consensus_pubkey == after.consensus_pubkey
                && before.stake == after.stake,
            "secondary identity, stake or admission status changed during retention proof"
        );
        let p2p = eth::read_call_at_result(
            &world.rpc.url(port),
            addresses::VS_ADDR,
            &eth::IValidatorSet::getP2pAddressCall {
                validatorAddress: address.parse()?,
            },
            height,
        )
        .map_err(|error| eyre!(error))?;
        ensure!(
            p2p.version == 1 && hex::encode(p2p.encoded) == ENCODED_SOURCE,
            "registered source IP changed during retention proof"
        );
    }
    Ok(())
}

fn source_ip_admitted(source: IpAddr, destination: SocketAddr) -> Result<bool> {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                use tokio::{io::AsyncReadExt, net::TcpSocket, time::timeout};
                let socket = TcpSocket::new_v4()?;
                socket.bind(SocketAddr::new(source, 0))?;
                let mut stream =
                    timeout(Duration::from_secs(3), socket.connect(destination)).await??;
                // Commonware's transport handshake timeout is five seconds. An
                // admitted silent client stays open; the IP gate closes immediately.
                let mut byte = [0];
                match timeout(Duration::from_secs(1), stream.read(&mut byte)).await {
                    Err(_) => Ok(true),
                    Ok(Ok(0)) => Ok(false),
                    Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionReset => Ok(false),
                    other => Err(eyre!("unexpected pre-handshake probe response: {other:?}")),
                }
            })
    })
    .join()
    .map_err(|_| eyre!("IP admission probe thread panicked"))?
}

fn probe_receivers(world: &mut World, phase: &str) -> Result<()> {
    let validators: serde_json::Value = serde_json::from_slice(&std::fs::read(
        world.localnet.scenario_dir().join("validators.json"),
    )?)?;
    let endpoints = validators
        .as_array()
        .ok_or_else(|| eyre!("missing founder endpoints"))?;
    ensure!(
        endpoints.len() == 4,
        "admission fixture requires four receivers"
    );
    for (index, entry) in endpoints.iter().enumerate() {
        let endpoint: SocketAddr = entry["p2p_address"]
            .as_str()
            .ok_or_else(|| eyre!("missing consensus endpoint"))?
            .parse()?;
        ensure!(
            !source_ip_admitted(UNKNOWN_SOURCE.parse()?, endpoint)?,
            "receiver {index} admitted an unregistered control IP"
        );
        ensure!(
            source_ip_admitted(SOURCE.parse()?, endpoint)?,
            "receiver {index} lost registered secondary source IP after {phase}"
        );
        world.state.restart_observations.push(json!({"phase": phase,
            "receiver": index, "endpoint": endpoint, "source_ip": SOURCE,
            "unknown_source_rejected": true, "registered_source_admitted": true}));
    }
    Ok(())
}

#[given(expr = "a {word} transport peer has its own registered source IP")]
fn registered_transport_peer(world: &mut World, status: String) {
    (|| -> Result<()> {
        ensure!(
            status == "Registered" || status == "Pending",
            "unknown admission status"
        );
        let index = world.validators.joiner_index();
        world.localnet.provision_joiner(index)?;
        let key = world.validators.joiner().evm_key()?;
        let address = eth::address_of(&key).ok_or_else(|| eyre!("invalid probe identity"))?;
        world.state.joiner_addr = Some(format!("{address:#x}"));
        let tx = eth::send_call(
            &world.rpc.url(world.validators.primary_port()),
            addresses::VS_ADDR,
            &key,
            &eth::IValidatorSet::setP2pAddressCall {
                validatorAddress: address,
                version: 1,
                encoded: alloy_primitives::Bytes::from(hex::decode(ENCODED_SOURCE)?),
            },
            None,
        )?;
        let receipt = eth::raw_json_result(
            &world.rpc.url(world.validators.primary_port()),
            "eth_getTransactionReceipt",
            json!([tx]),
        )?;
        ensure!(
            receipt["status"] == "0x1",
            "probe P2P registration reverted"
        );
        if status == "Pending" {
            world.rpc.stake(&key, 1000)?;
        }
        for i in 0..4 {
            capture_dkg_owner(world, i)?;
        }
        let ports = dkg_ports(world, &[0, 1, 2, 3])?;
        let target = world.rpc.fresh_finality_target(&ports)?;
        let anchor = world.rpc.wait_finalized_checkpoint(&ports, target, 40)?;
        world.state.lifecycle_before = Some(anchor);
        let expected_status = if status == "Pending" { 1 } else { 0 };
        for &port in &ports {
            ensure!(
                world
                    .rpc
                    .validator_record_at(port, &format!("{address:#x}"), anchor.height)
                    .is_some_and(|record| record.status == expected_status),
                "probe did not reach the requested {status} status"
            );
        }
        unchanged_secondary(world, &ports, anchor.height)?;
        record_dkg_checkpoint(world, "transport_admission_anchor", &ports, anchor);
        Ok(())
    })()
    .expect("register a secondary peer with a distinct source IP without confirming readiness");
}

#[then("its source IP is admitted before any retention eviction")]
fn initial_admission(world: &mut World) {
    probe_receivers(world, "initial admission").expect("initial IP admission");
}

#[when("two complete DKG rotations exclude the transport peer")]
fn excluded_rotations(world: &mut World) {
    (|| -> Result<()> {
        let anchor = world
            .state
            .lifecycle_before
            .ok_or_else(|| eyre!("missing admission anchor"))?;
        let address: alloy_primitives::Address = world
            .state
            .joiner_addr
            .as_ref()
            .ok_or_else(|| eyre!("missing transport identity"))?
            .parse()?;
        let mut next = anchor.height + 1;
        let mut cycles = std::collections::BTreeSet::new();
        let deadline = Instant::now() + Duration::from_secs(600);
        while cycles.len() < 2 {
            ensure!(
                Instant::now() < deadline,
                "secondary admission did not span two DKG rotations"
            );
            let ports = dkg_ports(world, &[0, 1, 2, 3])?;
            let point = world
                .rpc
                .wait_finalized_checkpoint(&ports, anchor.height, 1)?;
            while next <= point.height {
                if let Some(boundary) = dkg_boundary_at(world, &ports, next)? {
                    if boundary.freeze >= anchor.height {
                        unchanged_secondary(world, &ports, boundary.height)?;
                        ensure!(
                            !boundary.members.contains(&address) && boundary.members.len() == 4,
                            "secondary peer entered the DKG target without readiness"
                        );
                        ensure!(cycles.insert(boundary.cycle), "duplicate DKG cycle");
                        world.state.restart_observations.push(json!({
                            "phase": "transport_excluded_boundary", "boundary": boundary}));
                    }
                }
                next += 1;
            }
            sleep(Duration::from_millis(250));
        }
        let ports = dkg_ports(world, &[0, 1, 2, 3])?;
        let target = world.rpc.fresh_finality_target(&ports)?;
        let point = world.rpc.wait_finalized_checkpoint(&ports, target, 40)?;
        unchanged_secondary(world, &ports, point.height)?;
        record_dkg_checkpoint(world, "transport_retention_pressure", &ports, point);
        Ok(())
    })()
    .expect("unchanged secondary admission spans two complete excluded rotations");
}

#[then("its source IP remains admitted on every active validator")]
fn retained_admission(world: &mut World) {
    probe_receivers(world, "two excluded DKG rotations").expect("secondary IP survives retention");
}
