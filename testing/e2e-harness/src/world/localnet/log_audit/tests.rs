use std::path::{Path, PathBuf};

use super::{
    audit_full_node_shutdown, audit_loaded_logs, audit_loaded_logs_with_expectations,
    audit_validator_shutdown,
};
use outbe_primitives::runtime_audit_v1::{
    BODY_READ_REQUEST_DEADLINE, OTHER_PAYLOAD_EXECUTION_FAILURE, PAYLOAD_EXECUTION_FAILED,
    PROCESS_INSTANCE_FIELD, PROPOSAL_VIEW_CANCELLED, SCHEMA_VERSION,
};

const PROCESS_A: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PROCESS_B: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[test]
fn expected_sidecar_payload_warning_requires_exact_outage_and_process() {
    let tx = format!("0x{}", "11".repeat(32));
    let other_tx = format!("0x{}", "22".repeat(32));
    let height = 11;
    let block_hash = alloy_primitives::B256::repeat_byte(0x33);
    let warning = |tx: &str| {
        format!(
            "WARN payload_builder: failed to resolve pending payload err=evm execution error: \
         internal EVM error occurred when executing transaction {tx}: fatal: fatal: \
         tee_sidecar_unavailable: io error: Connection refused (os error 111)"
        )
    };
    let lines = [
        warning(&tx),
        "INFO reth::cli: Starting Reth version=example".to_owned(),
        warning(&tx),
        warning(&other_tx),
        warning(&tx).replace("Connection refused", "Connection reset"),
        "ERROR outbe_chain::launch::node::consensus: consensus stack failed e=executor actor returned fatal error".to_owned(),
        "0: executor actor returned fatal error".to_owned(),
        format!("2: failed to send finalized block to execution layer at height {height} digest {block_hash:#x}: execution stopped on a node-local dependency failure: tee_sidecar_unavailable: io error: Connection refused (os error 111)"),
        "INFO reth::cli: Starting Reth version=example".to_owned(),
        warning(&tx),
    ];
    let node = PathBuf::from("validator-3/node.log");
    let reth = PathBuf::from("validator-3/logs/54322345/reth.log");
    let other = PathBuf::from("validator-2/node.log");
    let logs = vec![
        (node.clone(), lines.join("\n")),
        (reth.clone(), lines.join("\n")),
        (other.clone(), lines.join("\n")),
    ];
    let accepted =
        super::expected_tee_sidecar_outage_records(&logs, 4, 3, &tx, height, block_hash).unwrap();
    for path in [&node, &reth] {
        assert!(accepted.contains(&(path.clone(), 2)));
        for rejected in [0, 3, 4, 9] {
            assert!(!accepted.contains(&(path.clone(), rejected)));
        }
    }
    assert!(!accepted.iter().any(|(path, _)| path == &other));
    for invalid in [
        logs.iter().map(|(path, text)| (path.clone(), text.replace("height 11", "height 12"))).collect::<Vec<_>>(),
        logs.iter().map(|(path, text)| (path.clone(), text.replace(&format!("{block_hash:#x}"), &other_tx))).collect(),
        logs.iter().map(|(path, text)| (path.clone(), text.replace("Connection refused", "Connection reset"))).collect(),
        logs.iter().map(|(path, text)| (path.clone(), text.replace("0: executor actor returned fatal error", "INFO reth::cli: Starting Reth version=example\n0: executor actor returned fatal error"))).collect(),
    ] {
        assert!(super::expected_tee_sidecar_outage_records(&invalid, 4, 3, &tx, height, block_hash).is_err());
    }
    assert!(
        super::expected_tee_sidecar_outage_records(&logs, 4, 2, &tx, height, block_hash).is_err()
    );
    let without_fail_stop = vec![(node, warning(&tx)), (reth, warning(&tx))];
    assert!(super::expected_tee_sidecar_outage_records(
        &without_fail_stop,
        4,
        3,
        &tx,
        height,
        block_hash
    )
    .is_err());
}

fn event(fields: &str, human_message: &str) -> String {
    event_for(PROCESS_A, fields, human_message)
}

fn event_for(process_instance: &str, fields: &str, human_message: &str) -> String {
    format!(
        "2026-08-13T00:00:00Z DEBUG \
         {PROCESS_INSTANCE_FIELD}={process_instance} {fields} {human_message}"
    )
}

#[test]
fn cancelled_deadline_uses_typed_fields_across_reordered_logs() {
    let root = tempfile::TempDir::new().unwrap();
    let first = root.path().join("validator-0/node.log.1");
    let current = root.path().join("validator-0/node.log");
    let payload_id = "0x850344ecbf402d32";
    let tx_hash = "0xde0edec00c2c9e76f5a142a7a06cf07079ebf0bcc191d570ab87b8a83150d3ce";
    let logs = vec![
        (
            first,
            event(
                &format!(
                    "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
                     payload_id={payload_id} tx_hash={tx_hash} \
                     failure_kind={BODY_READ_REQUEST_DEADLINE}"
                ),
                "this human message may be rewritten",
            ),
        ),
        (
            current,
            event(
                &format!(
                    "audit_schema={SCHEMA_VERSION} audit_event={PROPOSAL_VIEW_CANCELLED} \
                     payload_id={payload_id}"
                ),
                "different wording and later file order",
            ),
        ),
    ];

    let audit = audit_loaded_logs(&logs);
    assert!(audit.is_clean(), "{:?}", audit.findings);
    assert_eq!(audit.counts.expected_request_deadline_cancellation, 1);
    assert!(audit.ensure_clean().is_ok());
    assert_eq!(audit.json()["clean"], true);
}

#[test]
fn deadline_for_a_live_payload_fails_closed() {
    let path = tempfile::TempDir::new()
        .unwrap()
        .path()
        .join("validator-0/node.log");
    let logs = vec![(
        path,
        event(
            &format!(
                "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
                 payload_id=0x850344ecbf402d32 \
                 tx_hash=0xde0edec00c2c9e76f5a142a7a06cf07079ebf0bcc191d570ab87b8a83150d3ce \
                 failure_kind={BODY_READ_REQUEST_DEADLINE}"
            ),
            "message is irrelevant",
        ),
    )];

    let audit = audit_loaded_logs(&logs);
    assert!(!audit.is_clean());
    assert!(audit.ensure_clean().is_err());
    assert_eq!(audit.json()["clean"], false);
}

#[test]
fn cancellation_from_a_prior_process_cannot_authorize_a_current_failure() {
    let root = tempfile::TempDir::new().unwrap();
    let directory = root.path().join("validator-0");
    let payload_id = "0x850344ecbf402d32";
    let tx_hash = "0xde0edec00c2c9e76f5a142a7a06cf07079ebf0bcc191d570ab87b8a83150d3ce";
    let logs = vec![
        (
            directory.join("node.log.1"),
            event_for(
                PROCESS_A,
                &format!(
                    "audit_schema={SCHEMA_VERSION} audit_event={PROPOSAL_VIEW_CANCELLED} \
                     payload_id={payload_id}"
                ),
                "previous process",
            ),
        ),
        (
            directory.join("node.log"),
            event_for(
                PROCESS_B,
                &format!(
                    "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
                     payload_id={payload_id} tx_hash={tx_hash} \
                     failure_kind={BODY_READ_REQUEST_DEADLINE}"
                ),
                "current process",
            ),
        ),
    ];

    let audit = audit_loaded_logs(&logs);
    assert!(!audit.is_clean());
    assert_eq!(audit.counts.expected_request_deadline_cancellation, 0);
}

#[test]
fn hygiene_uses_record_severity_instead_of_nested_error_words() {
    let root = tempfile::TempDir::new().unwrap();
    let path = root.path().join("validator-0/node.log");
    let debug = vec![(
        path.clone(),
        "2026-08-13T00:00:00Z DEBUG retry discarded err=fatal: stale speculative work".to_owned(),
    )];
    assert!(audit_loaded_logs(&debug).is_clean());

    let error = vec![(
        path,
        "2026-08-13T00:00:00Z ERROR consensus supervisor fatal exit DEBUG context".to_owned(),
    )];
    assert!(!audit_loaded_logs(&error).is_clean());
}

#[test]
fn duplicate_typed_records_are_one_cancelled_episode() {
    let root = tempfile::TempDir::new().unwrap();
    let directory = root.path().join("validator-0");
    let payload = "0x850344ecbf402d32";
    let transaction = "0xde0edec00c2c9e76f5a142a7a06cf07079ebf0bcc191d570ab87b8a83150d3ce";
    let cancellation = event(
        &format!(
            "audit_schema={SCHEMA_VERSION} audit_event={PROPOSAL_VIEW_CANCELLED} \
             payload_id={payload}"
        ),
        "human cancellation text",
    );
    let failure = event(
        &format!(
            "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
             payload_id={payload} tx_hash={transaction} \
             failure_kind={BODY_READ_REQUEST_DEADLINE}"
        ),
        "human failure text",
    );
    let logs = vec![
        (directory.join("node.log.1"), cancellation.clone()),
        (
            directory.join("node.log"),
            format!("{cancellation}\n{failure}\n{failure}"),
        ),
    ];

    let audit = audit_loaded_logs(&logs);
    assert!(audit.is_clean(), "{:?}", audit.findings);
    assert_eq!(audit.counts.expected_request_deadline_cancellation, 1);
}

#[test]
fn conflicting_duplicate_failure_kinds_fail_closed_in_any_order() {
    let root = tempfile::TempDir::new().unwrap();
    let path = root.path().join("validator-0/node.log");
    let payload = "0x850344ecbf402d32";
    let transaction = "0xde0edec00c2c9e76f5a142a7a06cf07079ebf0bcc191d570ab87b8a83150d3ce";
    let cancellation = event(
        &format!(
            "audit_schema={SCHEMA_VERSION} audit_event={PROPOSAL_VIEW_CANCELLED} \
             payload_id={payload}"
        ),
        "cancelled",
    );
    let deadline = event(
        &format!(
            "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
             payload_id={payload} tx_hash={transaction} \
             failure_kind={BODY_READ_REQUEST_DEADLINE}"
        ),
        "deadline",
    );
    let other = event(
        &format!(
            "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
             payload_id={payload} tx_hash={transaction} \
             failure_kind={OTHER_PAYLOAD_EXECUTION_FAILURE}"
        ),
        "other",
    );

    for records in [
        format!("{cancellation}\n{deadline}\n{other}"),
        format!("{cancellation}\n{other}\n{deadline}"),
    ] {
        let audit = audit_loaded_logs(&[(path.clone(), records)]);
        assert!(!audit.is_clean());
        assert_eq!(audit.counts.expected_request_deadline_cancellation, 0);
    }
}

#[test]
fn malformed_unknown_mismatched_and_other_failures_fail_closed() {
    let root = tempfile::TempDir::new().unwrap();
    let directory = root.path().join("validator-0");
    let cancellation = event(
        &format!(
            "audit_schema={SCHEMA_VERSION} audit_event={PROPOSAL_VIEW_CANCELLED} \
             payload_id=0x850344ecbf402d32"
        ),
        "cancelled",
    );
    let records = [
        event(
            &format!(
                "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
                 payload_id=0x950344ecbf402d32 \
                 tx_hash=0xde0edec00c2c9e76f5a142a7a06cf07079ebf0bcc191d570ab87b8a83150d3ce \
                 failure_kind={BODY_READ_REQUEST_DEADLINE}"
            ),
            "different payload",
        ),
        event(
            &format!(
                "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
                 payload_id=0x850344ecbf402d32 \
                 tx_hash=0xce0edec00c2c9e76f5a142a7a06cf07079ebf0bcc191d570ab87b8a83150d3ce \
                 failure_kind={OTHER_PAYLOAD_EXECUTION_FAILURE}"
            ),
            "wrong failure kind",
        ),
        event(
            &format!(
                "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
                 payload_id=0x850344ecbf402d32 \
                 failure_kind={BODY_READ_REQUEST_DEADLINE}"
            ),
            "missing transaction hash",
        ),
        event(
            "audit_schema=2 audit_event=proposal_view_cancelled_v2 \
             payload_id=0x850344ecbf402d32",
            "unknown schema and event",
        ),
    ];
    let logs = vec![(
        directory.join("node.log"),
        std::iter::once(cancellation)
            .chain(records)
            .collect::<Vec<_>>()
            .join("\n"),
    )];

    let audit = audit_loaded_logs(&logs);
    assert!(!audit.is_clean());
    assert!(audit.findings.len() >= 4, "{:?}", audit.findings);
}

fn legacy_deadline_bundle(hash: &str, terminal_hash: &str) -> Vec<(PathBuf, String)> {
    let reth = format!(
        "2026-08-09T10:48:40Z ERROR on_new_payload{{block_hash={hash} block_num=323}}: fatal: body read request deadline exceeded\n\
         2026-08-09T10:48:40Z DEBUG on_new_payload{{block_hash={hash} block_num=323}}: fatal: body read request deadline exceeded block=NumHash {{ number: 323, hash: {hash} }}\n\
         2026-08-09T10:48:40Z WARN Failed to deliver newPayload response, receiver dropped (request cancelled): Err(Internal(BlockExecutionError(Other(\"fatal: body read request deadline exceeded\")))) payload=NumHash {{ number: 323, hash: {terminal_hash} }}"
    );
    let node = format!(
        "2026-08-09T10:48:40Z ERROR outbe::executor: fatal: body read request deadline exceeded\n\
         2026-08-09T10:48:40Z WARN Failed to deliver newPayload response, receiver dropped (request cancelled): Err(Internal(BlockExecutionError(Other(\"fatal: body read request deadline exceeded\")))) payload=NumHash {{ number: 323, hash: {terminal_hash} }}"
    );
    vec![
        (
            PathBuf::from("scenario-1/validator-1/logs/54322345/reth.log"),
            reth,
        ),
        (PathBuf::from("scenario-1/validator-1/node.log"), node),
    ]
}

#[test]
fn legacy_new_payload_adapter_preserves_the_existing_exact_rule() {
    let hash = "0x36aded35254849d7f72af50566e7ef7b799b8d970741319889c63b3ae292ce3a";
    let accepted =
        audit_loaded_logs_with_expectations(&legacy_deadline_bundle(hash, hash), 4, None, None);
    assert!(accepted.is_clean(), "{:?}", accepted.findings);
    assert_eq!(accepted.counts.expected_request_deadline_cancellation, 1);

    let other = "0x46aded35254849d7f72af50566e7ef7b799b8d970741319889c63b3ae292ce3a";
    assert!(!audit_loaded_logs_with_expectations(
        &legacy_deadline_bundle(hash, other),
        4,
        None,
        None
    )
    .is_clean());
}

#[test]
fn legacy_new_payload_adapter_accepts_rotated_reth_log() {
    let hash = "0x36aded35254849d7f72af50566e7ef7b799b8d970741319889c63b3ae292ce3a";
    let mut logs = legacy_deadline_bundle(hash, hash);
    logs[0].0 = PathBuf::from("scenario-1/validator-1/logs/54322345/reth.log.1");

    let accepted = audit_loaded_logs_with_expectations(&logs, 4, None, None);
    assert!(accepted.is_clean(), "{:?}", accepted.findings);
    assert_eq!(accepted.counts.expected_request_deadline_cancellation, 1);
}

#[test]
fn legacy_new_payload_adapter_accepts_only_the_canonical_joiner() {
    let hash = "0x36aded35254849d7f72af50566e7ef7b799b8d970741319889c63b3ae292ce3a";
    let joiner_logs = legacy_deadline_bundle(hash, hash)
        .into_iter()
        .map(|(path, content)| {
            (
                PathBuf::from(path.to_string_lossy().replace("validator-1", "validator-4")),
                content,
            )
        })
        .collect::<Vec<_>>();

    let accepted = audit_loaded_logs_with_expectations(&joiner_logs, 4, None, None);
    assert!(accepted.is_clean(), "{:?}", accepted.findings);
    assert_eq!(accepted.counts.expected_request_deadline_cancellation, 1);

    let outside_topology = joiner_logs
        .into_iter()
        .map(|(path, content)| {
            (
                PathBuf::from(path.to_string_lossy().replace("validator-4", "validator-5")),
                content,
            )
        })
        .collect::<Vec<_>>();
    let rejected = audit_loaded_logs_with_expectations(&outside_topology, 4, None, None);
    assert!(!rejected.is_clean());
    assert_eq!(rejected.counts.expected_request_deadline_cancellation, 0);
}

#[test]
fn unsupported_update_requires_a_rejection_not_every_replica_to_build() {
    let rejection = "ERROR fatal: cannot activate protocol version v3.0 (50331648): binary supports at most v0.1 (1)";
    let idle = (0..4)
        .map(|validator| {
            (
                PathBuf::from(format!("scenario-1/validator-{validator}/node.log")),
                "INFO finalized height=54".to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert!(!audit_loaded_logs_with_expectations(&idle, 4, Some(50_331_648), None).is_clean());
    for rejecting_validator in 0..4 {
        let mut logs = idle.clone();
        logs[rejecting_validator].1 = rejection.to_owned();
        let audit = audit_loaded_logs_with_expectations(&logs, 4, Some(50_331_648), None);
        assert!(audit.is_clean(), "{:?}", audit.findings);
        assert_eq!(audit.counts.expected_update_fatal, 1);
        logs[rejecting_validator]
            .1
            .push_str("\nERROR fatal: database corruption");
        assert!(!audit_loaded_logs_with_expectations(&logs, 4, Some(50_331_648), None).is_clean());
        logs[rejecting_validator].1 = rejection.replace("v3.0 (50331648)", "v3.1 (50397184)");
        assert!(!audit_loaded_logs_with_expectations(&logs, 4, Some(50_331_648), None).is_clean());
    }
}

#[test]
fn legacy_update_and_dkg_expectations_remain_exact_adapters() {
    let update = vec![(
        PathBuf::from("scenario-1/validator-0/node.log"),
        "ERROR fatal: cannot activate protocol version v3.0 (50331648): binary supports at most v0.1 (1)"
            .to_owned(),
    )];
    let update_audit = audit_loaded_logs_with_expectations(&update, 1, Some(50_331_648), None);
    assert!(update_audit.is_clean(), "{:?}", update_audit.findings);
    assert_eq!(update_audit.counts.expected_update_fatal, 1);

    let key = "91782b96da4ceae23d5adfa62ec55ef41827d43c8b624035972bc0a086f743266168a73e42e5b6c14c119dcf94d39588";
    let reveal = vec![(
        PathBuf::from("scenario-1/validator-0/node.log"),
        format!(
            "WARN DKG: a validator's individual share was REVEALED \
             revealed_validator={key}"
        ),
    )];
    let reveal_audit = audit_loaded_logs_with_expectations(&reveal, 1, None, Some(key));
    assert!(reveal_audit.is_clean(), "{:?}", reveal_audit.findings);
    assert_eq!(reveal_audit.counts.expected_dkg_reveal, 1);
}

fn tee_lease_guard_shutdown_bundle(validator: usize) -> Vec<(PathBuf, String)> {
    let guard =
        "ERROR outbe_chain::launch::shutdown: finalized TEE lease guard requested node shutdown \
        reason=validator is jailed; complete ordinary unjail and then run tee join";
    let shutdown = "INFO outbe_chain::launch::node::consensus: consensus stack shutting down";
    vec![
        (
            PathBuf::from(format!("scenario-1/validator-{validator}/node.log")),
            format!("{guard}\n{shutdown}"),
        ),
        (
            PathBuf::from(format!(
                "scenario-1/validator-{validator}/logs/54322345/reth.log"
            )),
            format!("{guard}\n{shutdown}"),
        ),
    ]
}

fn tee_lease_full_node_shutdown_bundle(full_node: usize) -> Vec<(PathBuf, String)> {
    let guard = "ERROR outbe_chain::launch::shutdown: finalized TEE lease guard requested node shutdown reason=finalized TEE lease expired at 1789793371; stop node and run tee join";
    let shutdown = "INFO outbe_chain::launch::node::consensus: consensus stack shutting down";
    vec![
        (
            PathBuf::from(format!("scenario-1/validator-{full_node}/node.log")),
            format!("{guard}\n{shutdown}\n"),
        ),
        (
            PathBuf::from(format!(
                "scenario-1/validator-{full_node}/logs/54322345/reth.log"
            )),
            format!(
                "{guard}\n{shutdown}\nDEBUG reth::cli: shutting down gracefully\nDEBUG reth::cli: shutdown signal received, terminating engine\nDEBUG engine::tree: received terminate request\nDEBUG engine::tree: persistence complete, signaling termination\n"
            ),
        ),
    ]
}

fn full_node_proof(logs: &[(PathBuf, String)]) -> crate::world::state::TeeLeaseShutdownV1 {
    crate::world::state::TeeLeaseShutdownV1 {
        slot: 4,
        pid: 1234,
        deadline: 1789793371,
        exit_code: Some(0),
        exit_signal: None,
        sinks: logs
            .iter()
            .map(
                |(path, content)| crate::world::state::TeeLeaseLogIntervalV1 {
                    path: path.clone(),
                    device: 1,
                    inode: 2,
                    start: 0,
                    end: content.len() as u64,
                    content: content.clone(),
                },
            )
            .collect(),
    }
}

#[test]
fn tee_lease_guard_accepts_only_the_explicit_exact_two_sink_shutdown_bundle() {
    let expected = tee_lease_guard_shutdown_bundle(3);
    let accepted = audit_validator_shutdown(&expected, 3);
    assert!(accepted.is_clean(), "{:?}", accepted.findings);
    assert_eq!(accepted.counts.expected_tee_lease_guard_shutdown_fatal, 4);

    let mut later_unrelated_shutdown = expected.clone();
    later_unrelated_shutdown[0]
        .1
        .push_str("\nINFO outbe_chain::launch::node::consensus: consensus stack shutting down");
    later_unrelated_shutdown[1].1.push_str(
        "\nINFO reth::cli: Starting Reth version=1.2.3\n\
         INFO outbe_chain::launch::node::consensus: consensus stack shutting down",
    );
    let accepted_with_later_shutdown = audit_validator_shutdown(&later_unrelated_shutdown, 3);
    assert!(
        accepted_with_later_shutdown.is_clean(),
        "{:?}",
        accepted_with_later_shutdown.findings
    );
    assert_eq!(
        accepted_with_later_shutdown
            .counts
            .expected_tee_lease_guard_shutdown_fatal,
        4
    );

    let unarmed = audit_loaded_logs_with_expectations(&expected, 4, None, None);
    assert!(!unarmed.is_clean());
    assert_eq!(unarmed.findings.len(), 2);

    for replacement in [
        "WARN outbe_chain::launch::shutdown:",
        "ERROR outbe_chain::launch::node:",
        "ERROR other:",
        "ERROR outbe_chain::launch::shutdown: unrelated reason ",
    ] {
        let mut changed = expected.clone();
        changed[0].1 = changed[0]
            .1
            .replace("ERROR outbe_chain::launch::shutdown:", replacement);
        assert_ne!(changed[0].1, expected[0].1);
        assert!(!audit_validator_shutdown(&changed, 3).is_clean());
    }

    let mut duplicate_guard = expected.clone();
    duplicate_guard[0].1.push('\n');
    duplicate_guard[0]
        .1
        .push_str(expected[0].1.lines().next().unwrap());
    assert!(!audit_validator_shutdown(&duplicate_guard, 3).is_clean());

    let wrong_validator = audit_validator_shutdown(&expected, 2);
    assert!(!wrong_validator.is_clean());

    let missing_sink = vec![expected[0].clone()];
    assert!(!audit_validator_shutdown(&missing_sink, 3).is_clean());

    let mut duplicate_sink = expected.clone();
    duplicate_sink.push(expected[0].clone());
    assert!(!audit_validator_shutdown(&duplicate_sink, 3).is_clean());

    let reordered = expected
        .iter()
        .map(|(path, content)| {
            let lines = content.lines().collect::<Vec<_>>();
            (path.clone(), format!("{}\n{}", lines[1], lines[0]))
        })
        .collect::<Vec<_>>();
    assert!(!audit_validator_shutdown(&reordered, 3).is_clean());

    let process_epoch_crossing = expected
        .iter()
        .map(|(path, content)| {
            (
                path.clone(),
                content.replacen(
                    "\nINFO outbe_chain::launch::node::consensus: consensus stack shutting down",
                    "\nINFO reth::cli: Starting Reth version=1.2.3\nINFO outbe_chain::launch::node::consensus: consensus stack shutting down",
                    1,
                ),
            )
        })
        .collect::<Vec<_>>();
    assert!(!audit_validator_shutdown(&process_epoch_crossing, 3).is_clean());

    let mut extra_fatal = expected.clone();
    extra_fatal[0]
        .1
        .push_str("\nERROR outbe_chain::launch::node: unrelated fatal condition");
    assert!(!audit_validator_shutdown(&extra_fatal, 3).is_clean());

    let mut duplicate_fatal_trailer = expected.clone();
    duplicate_fatal_trailer[0]
        .1
        .push_str("\nERROR engine::tree: Fatal error");
    assert!(!audit_validator_shutdown(&duplicate_fatal_trailer, 3).is_clean());
}

#[test]
fn tee_lease_full_node_accepts_only_owned_controlled_shutdown() {
    let expected = tee_lease_full_node_shutdown_bundle(4);
    let proof = full_node_proof(&expected);
    let audit = |logs: &[(PathBuf, String)], proof: &crate::world::state::TeeLeaseShutdownV1| {
        audit_full_node_shutdown(logs, proof)
    };
    let accepted = audit(&expected, &proof);
    assert!(accepted.is_clean(), "{:?}", accepted.findings);
    assert_eq!(accepted.counts.expected_tee_lease_guard_shutdown_fatal, 4);
    let unarmed = audit_loaded_logs_with_expectations(&expected, 4, None, None);
    assert!(!unarmed.is_clean());
    assert_eq!(unarmed.findings.len(), 2);
    let mut later_expiry = expected.clone();
    let guard = expected[0].1.lines().next().unwrap();
    later_expiry[0].1.push_str(guard);
    later_expiry[0].1.push('\n');
    assert!(!audit(&later_expiry, &proof).is_clean());
    for (code, signal, slot, pid, deadline) in [
        (None, None, 4, 1234, 1789793371),
        (Some(1), None, 4, 1234, 1789793371),
        (None, Some(15), 4, 1234, 1789793371),
        (Some(0), Some(9), 4, 1234, 1789793371),
        (Some(0), None, 5, 1234, 1789793371),
        (Some(0), None, 4, 0, 1789793371),
        (Some(0), None, 4, 1234, 1789793372),
    ] {
        let mut bad = proof.clone();
        bad.exit_code = code;
        bad.exit_signal = signal;
        bad.slot = slot;
        bad.pid = pid;
        bad.deadline = deadline;
        assert!(!audit(&expected, &bad).is_clean());
    }
    for trailer in [
        "thread tokio-rt panicked",
        "ERROR engine::tree: Fatal error",
        "ERROR reth::cli: Fatal error in consensus engine",
        "ERROR other: unrelated fatal",
    ] {
        let mut bad = expected.clone();
        bad[0].1.push_str(trailer);
        assert!(!audit(&bad, &proof).is_clean(), "{trailer}");
    }
}

#[test]
fn tee_lease_full_node_rejects_every_incomplete_or_cross_process_bundle() {
    let baseline = tee_lease_full_node_shutdown_bundle(4);
    let check = |logs: &[(PathBuf, String)]| {
        let proof = full_node_proof(logs);
        audit_full_node_shutdown(logs, &proof).is_clean()
    };
    assert!(check(&baseline));
    for sink in 0..2 {
        let lines: Vec<_> = baseline[sink].1.lines().collect();
        for removed in 0..lines.len() {
            let mut bad = baseline.clone();
            bad[sink].1 = lines
                .iter()
                .enumerate()
                .filter_map(|(i, line)| (i != removed).then_some(*line))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(!check(&bad), "missing sink {sink} line {removed}");
        }
        for (duplicated, line) in lines.iter().enumerate() {
            let mut bad = baseline.clone();
            bad[sink].1.push_str(line);
            bad[sink].1.push('\n');
            assert!(!check(&bad), "duplicate sink {sink} line {duplicated}");
        }
    }
    for replacement in [
        "INFO reth::cli: Starting Reth version=other\nINFO outbe_chain::launch::node::consensus: consensus stack shutting down",
        "INFO outbe_chain::launch::node::consensus: consensus stack shutting down\nINFO reth::cli: Starting Reth version=other",
    ] {
        let mut bad = baseline.clone();
        bad[1].1 = bad[1].1.replace(
            "INFO outbe_chain::launch::node::consensus: consensus stack shutting down",
            replacement,
        );
        assert!(!check(&bad));
    }
    let mut reordered = baseline.clone();
    reordered[1].1 = reordered[1].1.replace("DEBUG reth::cli: shutdown signal received, terminating engine\nDEBUG engine::tree: received terminate request",
        "DEBUG engine::tree: received terminate request\nDEBUG reth::cli: shutdown signal received, terminating engine");
    assert!(!check(&reordered));
    for replacement in [
        "WARN outbe_chain::launch::shutdown:",
        "ERROR outbe_chain::launch::node:",
        "ERROR other:",
        "ERROR outbe_chain::launch::shutdown: unrelated reason ",
    ] {
        let mut bad = baseline.clone();
        bad[0].1 = bad[0]
            .1
            .replace("ERROR outbe_chain::launch::shutdown:", replacement);
        assert_ne!(bad[0].1, baseline[0].1);
        assert!(!check(&bad));
    }
    let mut too_far = baseline.clone();
    too_far[1].1 = too_far[1].1.replace(
        "DEBUG reth::cli: shutting down gracefully",
        &format!(
            "{}DEBUG reth::cli: shutting down gracefully",
            "INFO noise\n".repeat(65)
        ),
    );
    assert!(!check(&too_far));
    assert!(!check(&baseline[..1]));
    let mut mismatched = baseline.clone();
    mismatched[1].0 = PathBuf::from("scenario-1/validator-5/logs/54322345/reth.log");
    assert!(!check(&mismatched));
}

#[test]
fn tee_lease_full_node_immutable_intervals_exclude_future_and_detect_changes() {
    let baseline = tee_lease_full_node_shutdown_bundle(4);
    let proof = full_node_proof(&baseline);
    let mut appended = baseline.clone();
    for (_, log) in &mut appended {
        log.push_str("INFO reth::cli: Starting Reth version=new\n");
    }
    assert!(audit_full_node_shutdown(&appended, &proof).is_clean());
    for changed in ["", "INFO replaced process\n"] {
        let mut bad = baseline.clone();
        bad[1].1 = changed.to_owned();
        assert!(!audit_full_node_shutdown(&bad, &proof).is_clean());
    }
    let mut previous = baseline.clone();
    let mut shifted = proof.clone();
    for (index, (_, content)) in previous.iter_mut().enumerate() {
        *content = format!("INFO previous process\n{content}");
        shifted.sinks[index].start += 22;
        shifted.sinks[index].end += 22;
    }
    assert!(audit_full_node_shutdown(&previous, &shifted).is_clean());
}

#[test]
fn runtime_log_discovery_is_explicit_and_rotation_aware() {
    for path in [
        "validator-0/node.log",
        "validator-0/node.log.1",
        "validator-0/enclave.log",
        "validator-0/radicle.log",
        "validator-0/logs/54322345/reth.log",
        "validator-0/logs/54322345/reth.log.1",
    ] {
        assert!(super::is_runtime_log(Path::new(path)), "missed {path}");
    }
    assert!(!super::is_runtime_log(Path::new(
        "validator-0/data/rocksdb/000011.log"
    )));
}

#[test]
fn terminal_shutdown_and_native_fatal_failures_cannot_be_a_clean_audit() {
    for message in [
        "WARN Radicle integration shutdown deadline exceeded error=manager stopped",
        "ERROR Radicle integration shutdown failed error=endpoint discovery failed",
        "WARN Radicle endpoint actor stopped error=network closed",
        "ERROR reth::cli: Fatal error in consensus engine",
        "ERROR fatal node shutdown failure=body commitment mismatch",
    ] {
        let logs = vec![(PathBuf::from("validator-0/node.log"), message.to_owned())];
        assert!(!audit_loaded_logs(&logs).is_clean(), "{message}");
    }
}
