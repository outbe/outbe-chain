//! The exact fail-stop of a validator whose TEE sidecar became unavailable.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::discovery::{process_start_boundary, runtime_sink};
use super::LogRecords;

pub(super) fn expected_tee_sidecar_outage_records(
    logs: &[(PathBuf, String)],
    validators: usize,
    expected_validator: usize,
    tx_hash: &str,
) -> Result<LogRecords, String> {
    if expected_validator >= validators || !tx_hash.starts_with("0x") || tx_hash.len() != 66 {
        return Err("invalid expected TEE sidecar outage identity".into());
    }
    let mut accepted = BTreeSet::new();
    let mut sinks = BTreeSet::new();
    let mut node_proof = false;
    for (path, content) in logs {
        let Some(sink) = runtime_sink(path, validators, expected_validator) else {
            continue;
        };
        let one_fail_stop = || format!("expected one TEE outage fail-stop in {}", path.display());
        let lines = content.lines().collect::<Vec<_>>();
        let Some(SidecarFailStop { fatal, proof }) =
            sidecar_fail_stop(&lines, sink, tx_hash).map_err(|_| one_fail_stop())?
        else {
            continue;
        };
        if !sinks.insert(sink) {
            return Err(one_fail_stop());
        }
        if let Some((actor, sidecar)) = proof {
            node_proof = true;
            accepted.insert((path.clone(), actor));
            accepted.insert((path.clone(), sidecar));
        }
        accepted.insert((path.clone(), fatal));
        accepted.extend(
            payload_warnings_before(&lines, fatal, tx_hash)
                .into_iter()
                .map(|index| (path.clone(), index)),
        );
    }
    if sinks != BTreeSet::from(["node.log", "reth.log"]) || !node_proof {
        return Err(format!(
            "missing matching validator-{expected_validator} TEE sidecar fail-stop for {tx_hash}"
        ));
    }
    Ok(accepted)
}

/// The payload-builder warnings for `tx_hash` within the fail-stop's process.
fn payload_warnings_before(lines: &[&str], fatal: usize, tx_hash: &str) -> Vec<usize> {
    let process_start = lines[..fatal]
        .iter()
        .rposition(|line| process_start_boundary(line))
        .unwrap_or(0);
    let expected_payload_error = format!(
        "WARN payload_builder: failed to resolve pending payload err=evm execution error: \
         internal EVM error occurred when executing transaction {tx_hash}: fatal: fatal: \
         tee_sidecar_unavailable: io error: Connection refused (os error 111)"
    );
    lines
        .iter()
        .enumerate()
        .take(fatal)
        .skip(process_start)
        .filter_map(|(index, line)| line.ends_with(&expected_payload_error).then_some(index))
        .collect()
}

struct SidecarFailStop {
    fatal: usize,
    proof: Option<(usize, usize)>,
}

fn sidecar_fail_stop(
    lines: &[&str],
    sink: &str,
    tx_hash: &str,
) -> Result<Option<SidecarFailStop>, ()> {
    let fatal = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            line.contains("ERROR outbe_chain::launch::node::consensus: consensus stack failed e=executor actor returned fatal error")
                .then_some(index)
        })
        .collect::<Vec<_>>();
    let [index] = fatal.as_slice() else {
        if fatal.is_empty() {
            return Ok(None);
        }
        return Err(());
    };
    if sink != "node.log" {
        return Ok(Some(SidecarFailStop {
            fatal: *index,
            proof: None,
        }));
    }
    let trailer = lines
        .iter()
        .enumerate()
        .skip(index + 1)
        .take(20)
        .take_while(|(_, line)| !process_start_boundary(line));
    let mut actor_error = None;
    let mut sidecar_error = None;
    for (trailer_index, line) in trailer {
        if line.trim() == "0: executor actor returned fatal error" {
            actor_error = Some(trailer_index);
        }
        if line.contains("2: internal EVM error occurred when executing transaction")
            && line.contains(tx_hash)
            && line.contains("tee_sidecar_unavailable: io error: Connection refused")
        {
            sidecar_error = Some(trailer_index);
        }
    }
    let proof = actor_error
        .zip(sidecar_error)
        .filter(|(actor, sidecar)| actor < sidecar);
    Ok(Some(SidecarFailStop {
        fatal: *index,
        proof,
    }))
}
