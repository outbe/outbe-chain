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
    height: u64,
    block_hash: alloy_primitives::B256,
) -> Result<LogRecords, String> {
    if expected_validator >= validators || !tx_hash.starts_with("0x") || tx_hash.len() != 66 {
        return Err("invalid expected TEE sidecar outage identity".into());
    }
    let mut accepted = BTreeSet::new();
    let mut sinks = BTreeSet::new();
    let mut witness_sinks = BTreeSet::new();
    let mut node_proof = false;
    let mut requires_witness_proof = false;
    for (path, content) in logs {
        let Some(sink) = runtime_sink(path, validators, expected_validator) else {
            continue;
        };
        let one_fail_stop = || format!("expected one TEE outage fail-stop in {}", path.display());
        let lines = content.lines().collect::<Vec<_>>();
        let Some(SidecarFailStop {
            fatal,
            proof,
            has_witness_proof,
            requires_witness_proof: witness_required,
        }) = sidecar_fail_stop(&lines, sink, height, block_hash).map_err(|_| one_fail_stop())?
        else {
            continue;
        };
        if !sinks.insert(sink) {
            return Err(one_fail_stop());
        }
        if has_witness_proof {
            witness_sinks.insert(sink);
        }
        requires_witness_proof |= witness_required;
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
    let required_sinks = BTreeSet::from(["node.log", "reth.log"]);
    if sinks != required_sinks
        || !node_proof
        || (requires_witness_proof && witness_sinks != required_sinks)
    {
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
    has_witness_proof: bool,
    requires_witness_proof: bool,
}

fn sidecar_fail_stop(
    lines: &[&str],
    sink: &str,
    height: u64,
    block_hash: alloy_primitives::B256,
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
    let has_witness_proof = local_witness_failure_before(lines, *index, height, block_hash);
    if sink != "node.log" {
        return Ok(Some(SidecarFailStop {
            fatal: *index,
            proof: None,
            has_witness_proof,
            requires_witness_proof: false,
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
    let mut witness_error = None;
    for (trailer_index, line) in trailer {
        if line.trim() == "0: executor actor returned fatal error" {
            actor_error = Some(trailer_index);
        }
        if line.trim() == format!(
            "2: failed to send finalized block to execution layer at height {height} digest {block_hash:#x}: \
             execution stopped on a node-local dependency failure: \
             tee_sidecar_unavailable: io error: Connection refused (os error 111)"
        ) {
            sidecar_error = Some(trailer_index);
        }
        if line.trim() == format!(
            "2: failed to send finalized block to execution layer at height {height} digest {block_hash:#x}: \
             execution stopped on a node-local dependency failure: \
             witness fault during transaction execution"
        ) {
            witness_error = Some(trailer_index);
        }
    }
    let legacy_proof = actor_error
        .zip(sidecar_error)
        .filter(|(actor, sidecar)| actor < sidecar);
    let witness_proof = actor_error
        .zip(witness_error)
        .filter(|(actor, witness)| has_witness_proof && actor < witness);
    Ok(Some(SidecarFailStop {
        fatal: *index,
        proof: legacy_proof.or(witness_proof),
        has_witness_proof,
        requires_witness_proof: legacy_proof.is_none() && witness_proof.is_some(),
    }))
}

fn local_witness_failure_before(
    lines: &[&str],
    fatal: usize,
    height: u64,
    block_hash: alloy_primitives::B256,
) -> bool {
    const MARKER: &str = "ERROR outbe_consensus::executor::actor::finalization: CRITICAL: certified finalized block failed enclave witness verification";
    let start = lines[..fatal]
        .iter()
        .rposition(|line| process_start_boundary(line))
        .unwrap_or(0);
    let end = lines[fatal + 1..]
        .iter()
        .position(|line| process_start_boundary(line))
        .map_or(lines.len(), |offset| fatal + 1 + offset);
    let mut proofs = lines
        .iter()
        .enumerate()
        .take(end)
        .skip(start)
        .filter(|(_, line)| line.contains(MARKER));
    let Some((index, line)) = proofs.next() else {
        return false;
    };
    let expected = format!(
        "{MARKER} height={height} digest={block_hash:#x} \
         witness_reason=LocalUnavailable reason=witness fault during transaction execution"
    );
    let Some(prefix) = line.strip_suffix(&expected) else {
        return false;
    };
    let timestamp = prefix.trim_end();
    let exact_prefix = timestamp.is_empty()
        || (timestamp.contains('T')
            && timestamp.contains(':')
            && !timestamp.bytes().any(|byte| byte.is_ascii_whitespace()));
    index < fatal && exact_prefix && proofs.next().is_none()
}
