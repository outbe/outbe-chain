use super::*;

// Read pending work at the copied execution frontier E. Finalized H may precede
// the Lysis activation and is recorded independently, without a pending-head gate.
pub(super) fn snapshot_pending_materialization_at(
    world: &crate::world::World,
    at: &crate::world::state::SnapshotBlock,
    expected: &crate::world::rpc::OcompCertifiedGenerationV1,
) -> eyre::Result<serde_json::Value> {
    use crate::internal::{addresses, eth};
    use outbe_ocomp_protocol::{
        nod_materialization::NodMaterializationHeadV1, profile::poc_schema_limits,
    };
    eyre::ensure!(
        canonical_snapshot_block(world, at.number)? == *at,
        "native height differs from canonical RPC identity"
    );
    let returned = eth::read_call_at_result(
        &world.rpc.url(world.validators.primary_port()),
        addresses::NOD_FACTORY_ADDR,
        &eth::INodFactory::materializationHeadCall {},
        at.number,
    )
    .map_err(|error| eyre::eyre!(error))?;
    eyre::ensure!(
        returned.exists,
        "no pending materialization head at native height {}",
        at.number
    );
    let raw = returned.canonicalHead;
    let head = NodMaterializationHeadV1::decode_canonical(raw.as_ref(), &poc_schema_limits())?;
    eyre::ensure!(
        head.job_id == expected.job_id
            && head.program_semantics_hash == expected.program_semantics_hash
            && head.worldwide_day == expected.worldwide_day
            && head.generation == expected.generation
            && head.nod_root == expected.nod_root
            && head.nod_count == expected.nod_count,
        "native-height materialization head is not the actual certified generation"
    );
    eyre::ensure!(
        head.next_nod_ordinal < head.nod_count,
        "actual certified generation already completed at native height {}",
        at.number
    );
    eyre::ensure!(
        canonical_snapshot_block(world, at.number)? == *at,
        "canonical identity changed during historical materialization read"
    );
    Ok(serde_json::json!({
        "observed": snapshot_now_millis()?,
        "block_number": at.number,
        "block_hash": at.hash,
        "canonical_head": hex::encode(raw),
        "queue_sequence": head.queue_sequence,
        "job_id": hex::encode(head.job_id),
        "program_semantics_hash": hex::encode(head.program_semantics_hash),
        "worldwide_day": head.worldwide_day,
        "generation": head.generation,
        "nod_root": hex::encode(head.nod_root),
        "nod_count": head.nod_count,
        "next_nod_ordinal": head.next_nod_ordinal,
        "last_progress_height": head.last_progress_height,
    }))
}

#[cfg(test)]
mod worker_collector_tests {
    use super::*;
    use crate::world::ocomp::{OcompProcessRecordV1, OcompProcessRole};

    fn record(pid: u32, start: u64) -> OcompProcessRecordV1 {
        OcompProcessRecordV1 {
            validator_index: Some(4),
            role: OcompProcessRole::Worker,
            worker_ordinal: Some(0),
            pid,
            started_at_millis: start,
            stopped_at_millis: None,
        }
    }

    #[test]
    fn worker_history_cannot_drop_prior_incarnations_or_hide_another_ordinal() {
        let mut old = record(10, 1);
        old.stopped_at_millis = Some(2);
        let live = record(11, 10);
        assert!(snapshot_require_history(&[old.clone()], &[old.clone(), live.clone()]).is_ok());
        assert!(
            snapshot_require_history(std::slice::from_ref(&old), std::slice::from_ref(&live))
                .is_err()
        );
        let mut other = live.clone();
        other.worker_ordinal = Some(1);
        assert!(snapshot_v1_worker_records(&[live.clone(), other]).is_err());
        assert!(snapshot_unique_worker(&[live.clone(), record(12, 11)]).is_err());
    }

    #[test]
    fn bounded_file_observation_retains_not_found_and_rejects_oversize() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("artifact");
        assert!(snapshot_read_file(&path, 3).unwrap().bytes.is_none());
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(snapshot_read_file(&path, 3).unwrap().bytes.unwrap(), b"abc");
        std::fs::write(&path, b"abcd").unwrap();
        assert!(snapshot_read_file(&path, 3).is_err());
    }

    #[test]
    fn raw_http_observation_preserves_body_and_rejects_status_and_truncation() {
        let body = b"unit_counter 7\n";
        let raw = [
            b"HTTP/1.0 200 OK\r\nContent-Length: 15\r\n\r\n".as_slice(),
            body,
        ]
        .concat();
        // The exact body is 15 bytes. Changing a byte must not change framing.
        assert_eq!(snapshot_http_body(&raw).unwrap(), body);
        assert!(snapshot_http_body(b"HTTP/1.0 503 unavailable\r\n\r\nno").is_err());
        assert!(snapshot_http_body(b"HTTP/1.0 200 OK\r\nContent-Length: 4\r\n\r\na").is_err());
    }
    #[test]
    fn worker_http_reads_actual_bounded_socket_response() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            assert!(request.starts_with(b"GET /metrics HTTP/1.0\r\n"));
            socket
                .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 3\r\n\r\nraw")
                .unwrap();
        });
        let observed = snapshot_http_get(address, "/metrics");
        server.join().unwrap();
        assert_eq!(observed.unwrap(), b"raw");
    }
}

// Append inside features/ocomp/offline_snapshot.rs after the existing helpers.
// This fragment owns no launches, requests, mutations or background polling.
// Scope: recipient slot 4, worker ordinal 0, one installed V1 bundle lane.
use crate::world::state::{
    SnapshotDirectoryListing, SnapshotResultObservation, SnapshotWorkerExecution,
    SnapshotWorkerHttpObservation,
};

const SNAPSHOT_RECIPIENT_SLOT: u8 = 4;
const SNAPSHOT_OBJECT_BYTES: u64 = 1_048_576; // Current runtime CAS object ceiling.

#[derive(Clone, Debug)]
pub(crate) struct SnapshotWorkerBeforeRequest {
    domain_root: std::path::PathBuf,
    address: std::net::SocketAddr,
    bundle: outbe_ocomp::bundle::PinnedProtocolBundle,
    artifact_inventory: SnapshotDirectoryListing,
    local_inventory: SnapshotDirectoryListing,
    history: Vec<crate::world::ocomp::OcompProcessRecordV1>,
    history_from: u64,
}

#[derive(Clone)]
pub(super) struct SnapshotWorkerRunning {
    pub(super) before_request: SnapshotWorkerBeforeRequest,
    pub(super) before: SnapshotWorkerHttpObservation,
    pub(super) before_status: SnapshotWorkerHttpObservation,
}

pub(super) struct SnapshotWorkerLiveResult {
    pub(super) running: SnapshotWorkerRunning,
    pub(super) after: SnapshotWorkerHttpObservation,
    pub(super) after_status: SnapshotWorkerHttpObservation,
    pub(super) artifact: SnapshotFileRead,
    pub(super) local: SnapshotFileRead,
    pub(super) local_result: SnapshotResultObservation,
    pub(super) job_id: alloy_primitives::B256,
    pub(super) history: Vec<crate::world::ocomp::OcompProcessRecordV1>,
    pub(super) history_through: u64,
}

// Retain these alongside the final existing evidence type, for raw diagnostics.
pub(super) struct SnapshotCollectedWorker {
    pub(super) new_job: SnapshotNewJobObservation,
    pub(super) before_status: SnapshotWorkerHttpObservation,
    pub(super) after_status: SnapshotWorkerHttpObservation,
    pub(super) admission_record: SnapshotFileRead,
}

/// Call with the actual recipient domain root while root-owned writers are stopped.
/// No future job or unit ID is required. Port is Config::ocomp_worker_port(4, 0).
pub(super) fn snapshot_worker_inventory(
    topology: &crate::world::ocomp::OcompTopology,
    domain_root: &std::path::Path,
    worker_port: u16,
    expected_bundle: alloy_primitives::B256,
) -> eyre::Result<SnapshotWorkerBeforeRequest> {
    let history_from = snapshot_now_millis()?;
    let history = snapshot_v1_worker_records(topology.process_records())?;
    ensure!(
        history.iter().all(|p| p.stopped_at_millis.is_some()),
        "recipient worker inventory requires stopped writers"
    );
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let raw = snapshot_read_file(
        &domain_root
            .join("protocol-bundles-v1")
            .join(format!("{}.ocb1", hex::encode(expected_bundle))),
        SNAPSHOT_OBJECT_BYTES,
    )?;
    let bundle = outbe_ocomp::bundle::PinnedProtocolBundle::decode(
        raw.bytes
            .as_deref()
            .ok_or_else(|| eyre!("missing installed V1 bundle"))?,
        expected_bundle,
        &limits,
    )?;
    let inbox = domain_root
        .join("worker-inbox-v1")
        .join(hex::encode(bundle.hash()));
    let artifact_inventory = observe_snapshot_directory(&inbox.join("artifacts"))?;
    let local_inventory = observe_snapshot_directory(&domain_root.join("node-v1/local-results"))?;
    Ok(SnapshotWorkerBeforeRequest {
        domain_root: domain_root.to_path_buf(),
        address: ([127, 0, 0, 1], worker_port).into(),
        bundle,
        artifact_inventory,
        local_inventory,
        history,
        history_from,
    })
}

/// Call after ordinary worker launch and before root requests the next-day job.
pub(super) fn snapshot_worker_before(
    topology: &mut crate::world::ocomp::OcompTopology,
    inventory: SnapshotWorkerBeforeRequest,
) -> eyre::Result<SnapshotWorkerRunning> {
    let history = snapshot_v1_worker_records(topology.process_records())?;
    snapshot_require_history(&inventory.history, &history)?;
    let process = snapshot_unique_worker(&history)?;
    let owner = snapshot_owner(&inventory, process);
    let before_status = snapshot_worker_http(topology, &owner, inventory.address, "/status")?;
    let _: outbe_ocomp::worker_observability::WorkerStatusV1 =
        serde_json::from_slice(&before_status.body)?;
    let before = snapshot_worker_http(topology, &owner, inventory.address, "/metrics")?;
    worker_counters(&before.body)?;
    Ok(SnapshotWorkerRunning {
        before_request: inventory,
        before,
        before_status,
    })
}

/// Call after the existing root-owned public/local completion wait, before stop.
/// Select a new inbox artifact by its raw job identity. The later independent
/// admission/plan/CAS check authenticates it. The inbox is never the spec source.
pub(super) fn snapshot_worker_after(
    topology: &mut crate::world::ocomp::OcompTopology,
    running: &SnapshotWorkerRunning,
    job_id: alloy_primitives::B256,
) -> eyre::Result<SnapshotWorkerLiveResult> {
    let owner = &running.before.owner;
    let inventory = &running.before_request;
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let current = observe_snapshot_directory(&inventory.artifact_inventory.root)?;
    let mut selected = None;
    for entry in &current.entries {
        if inventory.artifact_inventory.entries.contains(entry)
            || entry.extension().is_none_or(|ext| ext != "ocb1")
        {
            continue;
        }
        let observation = snapshot_read_file(&current.root.join(entry), SNAPSHOT_OBJECT_BYTES)?;
        let raw = observation
            .bytes
            .as_deref()
            .ok_or_else(|| eyre!("new inbox artifact vanished"))?;
        let artifact = outbe_ocomp_protocol::unit::UnitArtifactV1::decode_canonical(raw, &limits)?;
        if artifact.job_id == job_id && artifact.protocol_bundle_hash == inventory.bundle.hash() {
            ensure!(
                entry
                    == &std::path::PathBuf::from(format!("{}.ocb1", hex::encode(artifact.unit_id))),
                "inbox filename/unit mismatch"
            );
            selected = Some(observation);
            break;
        }
    }
    let artifact = selected
        .ok_or_else(|| eyre!("no newly present recipient artifact for finalized job {job_id}"))?;
    let local = snapshot_read_file(
        &inventory
            .local_inventory
            .root
            .join(format!("{}.lysis-result-v1.ocb1", hex::encode(job_id))),
        SNAPSHOT_OBJECT_BYTES,
    )?;
    let raw = local
        .bytes
        .as_deref()
        .ok_or_else(|| eyre!("new local result not yet committed"))?;
    let decoded = outbe_ocomp_protocol::result::LysisResultV1::decode_canonical(raw, &limits)?;
    ensure!(
        decoded.job_id == job_id
            && decoded.protocol_bundle_hash == inventory.bundle.hash()
            && decoded.encode_canonical(&limits)? == raw,
        "local result job/bundle/canonical encoding mismatch"
    );
    let local_result = SnapshotResultObservation {
        job_id: hex::encode(job_id),
        digest: hex::encode(decoded.result_digest(&limits)?),
    };
    let after_status = snapshot_worker_http(topology, owner, inventory.address, "/status")?;
    let _: outbe_ocomp::worker_observability::WorkerStatusV1 =
        serde_json::from_slice(&after_status.body)?;
    let after = snapshot_worker_http(topology, owner, inventory.address, "/metrics")?;
    let history = snapshot_v1_worker_records(topology.process_records())?;
    snapshot_require_history(&inventory.history, &history)?;
    ensure!(
        snapshot_unique_worker(&history)? == owner.process,
        "worker incarnation changed during observation"
    );
    let history_through = snapshot_now_millis()?;
    Ok(SnapshotWorkerLiveResult {
        running: running.clone(),
        after,
        after_status,
        artifact,
        local,
        local_result,
        job_id,
        history,
        history_through,
    })
}

/// Read immutable job authorities after the writer releases its lock (normally
/// root's existing ordinary stop before K). All native readers drop on return.
pub(super) fn verify_snapshot_admission(
    live: &SnapshotWorkerLiveResult,
    admissions: &outbe_ocomp::admission_catalog::AdmissionCatalogReader,
    audit: &outbe_ocomp::lysis_plan_audit::LocalLysisPlanAuditV1<'_>,
    reader: &outbe_ocomp::cas::FilesystemCasReader,
    admission_path: &std::path::Path,
) -> eyre::Result<(
    outbe_ocomp_protocol::unit::UnitSpecV1,
    outbe_ocomp::admission_catalog::VerifiedAdmissionRecordV1,
    SnapshotFileRead,
)> {
    let inventory = &live.running.before_request;
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let raw = live
        .artifact
        .bytes
        .as_deref()
        .ok_or_else(|| eyre!("missing captured artifact"))?;
    let artifact = outbe_ocomp_protocol::unit::UnitArtifactV1::decode_canonical(raw, &limits)?;
    let mut matched = None;
    for entry in admissions.exact_plan_cursor()? {
        let entry = entry?;
        if entry.unit_id == artifact.unit_id {
            ensure!(matched.is_none(), "duplicate unit admission");
            matched = Some(entry);
        }
    }
    let admitted = matched.ok_or_else(|| eyre!("captured unit has no independent admission"))?;
    let spec = audit.candidate_spec_at(admitted.plan_ordinal)?;
    ensure!(
        admitted.job_id == live.job_id
            && admitted.protocol_bundle_hash == inventory.bundle.hash()
            && admitted.plan_hash == audit.plan().plan_hash(&limits)?
            && admitted.unit_id == spec.unit_id(&limits)?,
        "admission does not bind the canonical job plan"
    );
    artifact.validate_against(&spec, &limits)?;
    let cas_object = reader.read_verified(&admitted.artifact_ref)?;
    ensure!(
        cas_object.bytes() == raw,
        "inbox bytes differ from independently admitted CAS object"
    );
    let admission_record = snapshot_read_file(
        &admission_path.join(format!("{:010}.admission", admitted.plan_ordinal)),
        SNAPSHOT_OBJECT_BYTES,
    )?;
    ensure!(
        admission_record.bytes.is_some(),
        "verified admission file vanished"
    );
    Ok((spec, admitted, admission_record))
}

pub(super) fn snapshot_worker_bind_admission(
    live: SnapshotWorkerLiveResult,
    requested: u64,
    request: SnapshotBlock,
    canonical_result: SnapshotResultObservation,
    canonical_result_at: SnapshotBlock,
) -> eyre::Result<SnapshotCollectedWorker> {
    use outbe_ocomp::{
        admission_catalog::AdmissionCatalogReader,
        cas::{CasLimits, FilesystemCasReader},
        input_artifacts::poc_input_list_limits,
        input_ref_catalog::VerifiedInputChunkRefCatalog,
    };
    let inventory = &live.running.before_request;
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let reader = FilesystemCasReader::open(
        inventory.domain_root.join("cas-v1"),
        CasLimits {
            max_object_bytes: SNAPSHOT_OBJECT_BYTES,
            max_total_bytes: u64::MAX,
        },
    )?;
    let job = hex::encode(live.job_id);
    let admission_path = inventory
        .domain_root
        .join("supervisor-v1/jobs")
        .join(&job)
        .join("admissions");
    let admissions = AdmissionCatalogReader::open_existing(&admission_path, &reader, limits)?;
    let input_refs = VerifiedInputChunkRefCatalog::reopen(
        inventory
            .domain_root
            .join("exporter-v1/input-refs")
            .join(&job),
        &reader,
        limits,
        poc_input_list_limits(),
    )?;
    let audit = outbe_ocomp::lysis_plan_audit::open_read_only_local_plan_audit(
        &admissions,
        &input_refs,
        &reader,
        &inventory.bundle,
        &limits,
    )?;
    ensure!(
        audit.plan().job_id == live.job_id
            && audit.plan().protocol_bundle_hash == inventory.bundle.hash(),
        "independent plan is not the requested job/bundle"
    );
    let (spec, admitted, admission_record) =
        verify_snapshot_admission(&live, &admissions, &audit, &reader, &admission_path)?;
    let workers = live
        .history
        .iter()
        .cloned()
        .map(|p| snapshot_owner(inventory, p))
        .collect();
    let worker = SnapshotWorkerExecution {
        owner: live.running.before.owner.clone(),
        before: live.running.before.clone(),
        after: live.after,
        attribution: SnapshotWorkerAttribution::SingleOwnedProducer {
            inventory_from: inventory.history_from,
            inventory_through: live.history_through,
            workers,
        },
        artifact_before: SnapshotPriorFileObservation::DirectoryListing(
            inventory.artifact_inventory.clone(),
        ),
        artifact_after: live.artifact,
        admission_catalog: admission_path,
        canonical_unit_spec: spec.encode_canonical(&limits)?,
        admitted_artifact_len: admitted.artifact_ref.encoded_bytes,
        admitted_artifact_keccak256: admitted.artifact_ref.transport_digest,
        log: None,
    };
    let new_job = SnapshotNewJobObservation {
        requested,
        request,
        job_id: job,
        worker,
        local_before: SnapshotPriorFileObservation::DirectoryListing(
            inventory.local_inventory.clone(),
        ),
        local_after: live.local,
        local_result_root: inventory.local_inventory.root.clone(),
        local_result: live.local_result,
        canonical_result,
        canonical_result_at,
    };
    actual_worker(&new_job, SNAPSHOT_RECIPIENT_SLOT)?;
    ensure!(
        new_job.local_result == new_job.canonical_result,
        "local result differs from independently observed public result"
    );
    newly_present(
        &new_job.local_before,
        &new_job.local_after,
        requested,
        &new_job.local_result_root,
    )?;
    Ok(SnapshotCollectedWorker {
        new_job,
        before_status: live.running.before_status,
        after_status: live.after_status,
        admission_record,
    })
}

pub(super) fn snapshot_v1_worker_records(
    records: &[crate::world::ocomp::OcompProcessRecordV1],
) -> eyre::Result<Vec<crate::world::ocomp::OcompProcessRecordV1>> {
    let records: Vec<_> = records
        .iter()
        .filter(|p| {
            p.validator_index == Some(SNAPSHOT_RECIPIENT_SLOT)
                && p.role == crate::world::ocomp::OcompProcessRole::Worker
        })
        .cloned()
        .collect();
    ensure!(
        records.iter().all(|p| p.worker_ordinal == Some(0)),
        "collector is scoped to V1 worker0; another lane/ordinal is present"
    );
    Ok(records)
}

pub(super) fn snapshot_require_history(
    before: &[crate::world::ocomp::OcompProcessRecordV1],
    after: &[crate::world::ocomp::OcompProcessRecordV1],
) -> eyre::Result<()> {
    ensure!(
        before.iter().all(|old| after.iter().any(|new| old == new)),
        "retained process history dropped or rewrote a prior stopped incarnation"
    );
    Ok(())
}

pub(super) fn snapshot_unique_worker(
    records: &[crate::world::ocomp::OcompProcessRecordV1],
) -> eyre::Result<crate::world::ocomp::OcompProcessRecordV1> {
    let mut live = records.iter().filter(|p| p.stopped_at_millis.is_none());
    let process = live
        .next()
        .ok_or_else(|| eyre!("no live owned recipient worker"))?;
    ensure!(live.next().is_none(), "multiple live recipient workers");
    Ok(process.clone())
}

pub(super) fn snapshot_owner(
    inventory: &SnapshotWorkerBeforeRequest,
    process: crate::world::ocomp::OcompProcessRecordV1,
) -> SnapshotWorkerOwner {
    SnapshotWorkerOwner {
        process,
        endpoint: format!("http://{}", inventory.address),
        inbox_root: inventory
            .domain_root
            .join("worker-inbox-v1")
            .join(hex::encode(inventory.bundle.hash())),
        bundle_hash: inventory.bundle.hash(),
    }
}

pub(super) fn snapshot_worker_http(
    topology: &mut crate::world::ocomp::OcompTopology,
    owner: &SnapshotWorkerOwner,
    address: std::net::SocketAddr,
    path: &str,
) -> eyre::Result<SnapshotWorkerHttpObservation> {
    topology.ensure_worker_alive(SNAPSHOT_RECIPIENT_SLOT, 0)?;
    ensure!(
        snapshot_unique_worker(&snapshot_v1_worker_records(topology.process_records())?)?
            == owner.process,
        "wrong owned worker before HTTP observation"
    );
    let body = snapshot_http_get(address, path)?;
    topology.ensure_worker_alive(SNAPSHOT_RECIPIENT_SLOT, 0)?;
    ensure!(
        snapshot_unique_worker(&snapshot_v1_worker_records(topology.process_records())?)?
            == owner.process,
        "wrong owned worker after HTTP observation"
    );
    Ok(SnapshotWorkerHttpObservation {
        owner: owner.clone(),
        observed: snapshot_now_millis()?,
        body,
    })
}

pub(super) fn snapshot_http_get(
    address: std::net::SocketAddr,
    path: &str,
) -> eyre::Result<Vec<u8>> {
    use std::io::{Read as _, Write as _};
    let timeout = std::time::Duration::from_secs(2);
    let mut stream = std::net::TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    // HTTP/1.0 requests close-delimited/non-chunked responses from this local server.
    stream.write_all(
        format!("GET {path} HTTP/1.0\r\nHost: {address}\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let mut raw = Vec::new();
    stream.take(65_537).read_to_end(&mut raw)?;
    ensure!(raw.len() <= 65_536, "worker HTTP response exceeds bound");
    snapshot_http_body(&raw)
}

pub(super) fn snapshot_http_body(raw: &[u8]) -> eyre::Result<Vec<u8>> {
    let offset = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| eyre!("malformed worker HTTP headers"))?;
    let headers = std::str::from_utf8(&raw[..offset])?;
    ensure!(
        headers
            .lines()
            .next()
            .and_then(|s| s.split_whitespace().nth(1))
            == Some("200"),
        "worker HTTP request failed"
    );
    let body = &raw[offset + 4..];
    for line in headers.lines().skip(1) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| eyre!("malformed worker HTTP header"))?;
        ensure!(
            !name.eq_ignore_ascii_case("transfer-encoding"),
            "unexpected transfer encoding on HTTP/1.0 response"
        );
        if name.eq_ignore_ascii_case("content-length") {
            ensure!(
                value.trim().parse::<usize>()? == body.len(),
                "truncated or overlong worker HTTP body"
            );
        }
    }
    Ok(body.to_vec())
}

pub(super) fn snapshot_read_file(
    path: &std::path::Path,
    max_bytes: u64,
) -> eyre::Result<SnapshotFileRead> {
    use std::io::Read as _;
    let bytes = match std::fs::File::open(path) {
        Ok(file) => {
            ensure!(file.metadata()?.is_file(), "expected regular evidence file");
            let mut bytes = Vec::new();
            file.take(
                max_bytes
                    .checked_add(1)
                    .ok_or_else(|| eyre!("invalid file byte bound"))?,
            )
            .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 <= max_bytes,
                "evidence file exceeds byte bound"
            );
            Some(bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    Ok(SnapshotFileRead {
        path: path.to_path_buf(),
        observed: snapshot_now_millis()?,
        bytes,
    })
}
