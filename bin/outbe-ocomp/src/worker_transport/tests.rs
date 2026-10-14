use super::*;
use std::io::Write as _;

use outbe_ocomp_protocol::common::BoundedBytes;
use outbe_ocomp_protocol::unit::{EntityIdHalfOpenRange, UnitInterval, UnitPhase, UnitSpecV1};
use zeromq::util::PeerIdentity;

fn identity() -> EndpointIdentity {
    EndpointIdentity {
        chain_id: 41,
        genesis_hash: B256::repeat_byte(0x41),
        boot_nonce: B256::repeat_byte(0x42),
        protocol_bundle_hash: B256::repeat_byte(0x43),
    }
}

fn request(marker: u8) -> RunUnitV1 {
    let limits = crate::control::poc_schema_limits();
    let spec = UnitSpecV1 {
        protocol_bundle_hash: identity().protocol_bundle_hash,
        job_id: B256::repeat_byte(marker),
        attempt: 0,
        phase: UnitPhase::Enumerate,
        interval: UnitInterval::EntityIdRange(EntityIdHalfOpenRange {
            start: B256::from([marker; 32]),
            end: None,
        }),
        canonical_ordered_inputs: Vec::new(),
        lysis_program_semantics_hash: B256::repeat_byte(0x44),
        planner_spec_version: 1,
        reducer_spec_version: 1,
    };
    RunUnitV1 {
        protocol_bundle_hash: spec.protocol_bundle_hash,
        job_id: spec.job_id,
        attempt: spec.attempt,
        plan_hash: B256::repeat_byte(marker.wrapping_add(1)),
        unit_index: 0,
        canonical_unit_spec: BoundedBytes(spec.encode_canonical(&limits).unwrap()),
        unit_membership_siblings: Vec::new(),
        plan_ref: outbe_ocomp_protocol::CasObjectRefV1 {
            transport_digest: B256::repeat_byte(marker.wrapping_add(2)),
            encoded_bytes: 1,
            expected_ocb1_kind: None,
        },
        input_manifest_ref: outbe_ocomp_protocol::CasObjectRefV1 {
            transport_digest: B256::repeat_byte(marker.wrapping_add(3)),
            encoded_bytes: 1,
            expected_ocb1_kind: None,
        },
        ordered_input_refs: Vec::new(),
    }
}

fn dealer_command(message: ZmqMessage) -> SupervisorCommandV1 {
    let frames = message.into_vec();
    assert_eq!(frames.len(), 1);
    serde_json::from_slice(&frames[0]).expect("decode Supervisor command")
}

async fn dealer_event(sender: &mut zeromq::DealerSendHalf, event: &WorkerEventV1) {
    sender
        .send(serde_json::to_vec(event).unwrap().into())
        .await
        .expect("send Worker event");
}

#[test]
fn registry_accepts_four_workers_and_rejects_fifth() {
    let limits = crate::control::poc_schema_limits();
    let state = WorkerApiState::new(
        identity(),
        7,
        limits,
        WorkerApiDurations::default(),
        "tcp://127.0.0.1:9766".into(),
    );
    for ordinal in 0_u8..4 {
        let mut worker = identity();
        worker.boot_nonce = B256::repeat_byte(0x50 + ordinal);
        state
            .register(WorkerRegistrationV1::from_identity(worker, limits))
            .expect("register worker");
    }
    let mut fifth = identity();
    fifth.boot_nonce = B256::repeat_byte(0x60);
    assert!(matches!(
        state.register(WorkerRegistrationV1::from_identity(fifth, limits)),
        Err(WorkerApiErrorV1::RegistryFull)
    ));
    assert_eq!(state.status().unwrap().registered_workers, 4);
}

#[test]
fn cancelled_dispatch_does_not_wait_for_registration_timeout() {
    let limits = crate::control::poc_schema_limits();
    let state = WorkerApiState::new(
        identity(),
        7,
        limits,
        WorkerApiDurations::default(),
        "tcp://127.0.0.1:9766".into(),
    );
    let cancelled = AtomicBool::new(true);
    let request = RunUnitV1 {
        protocol_bundle_hash: identity().protocol_bundle_hash,
        job_id: B256::repeat_byte(0x71),
        attempt: 0,
        plan_hash: B256::repeat_byte(0x72),
        unit_index: 0,
        canonical_unit_spec: outbe_ocomp_protocol::common::BoundedBytes(Vec::new()),
        unit_membership_siblings: Vec::new(),
        plan_ref: outbe_ocomp_protocol::CasObjectRefV1 {
            transport_digest: B256::repeat_byte(0x73),
            encoded_bytes: 1,
            expected_ocb1_kind: None,
        },
        input_manifest_ref: outbe_ocomp_protocol::CasObjectRefV1 {
            transport_digest: B256::repeat_byte(0x74),
            encoded_bytes: 1,
            expected_ocb1_kind: None,
        },
        ordered_input_refs: Vec::new(),
    };
    assert!(matches!(
        state.dispatch(&request, Some(&cancelled)),
        Err(WorkerApiErrorV1::DispatchCancelled)
    ));
}

#[test]
fn router_cancel_delivery_returns_worker_to_ready_for_the_next_unit() {
    let limits = crate::control::poc_schema_limits();
    let server =
        SupervisorWorkerServerV1::start("127.0.0.1:0".parse().unwrap(), identity(), 7, limits)
            .expect("start Axum and ZeroMQ worker server");
    let (accepted_tx, accepted_rx) = mpsc::sync_channel(1);
    let (cancelled_tx, cancelled_rx) = mpsc::sync_channel(1);
    let http_address = server.address();
    let worker = std::thread::spawn(move || {
        let mut worker_identity = identity();
        worker_identity.boot_nonce = B256::repeat_byte(0x55);
        let registration: WorkerRegistrationResponseV1 = reqwest::blocking::Client::new()
            .post(format!("http://{http_address}/v1/workers/register"))
            .json(&WorkerRegistrationV1::from_identity(
                worker_identity,
                limits,
            ))
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let peer: PeerIdentity = registration.worker_id.parse().unwrap();
                let mut options = zeromq::SocketOptions::default();
                options.peer_identity(peer);
                let mut socket = zeromq::DealerSocket::with_options(options);
                socket
                    .connect(&registration.message_endpoint)
                    .await
                    .unwrap();
                let (mut sender, mut receiver) = socket.split();
                dealer_event(
                    &mut sender,
                    &WorkerEventV1::Ready {
                        worker_id: registration.worker_id.clone(),
                        registry_generation: registration.registry_generation,
                    },
                )
                .await;

                let first = match dealer_command(receiver.recv().await.unwrap()) {
                    SupervisorCommandV1::Work {
                        lease,
                        registry_generation,
                    } => {
                        assert_eq!(registry_generation, registration.registry_generation);
                        lease
                    }
                    SupervisorCommandV1::Cancel { .. } => panic!("expected first Work"),
                };
                dealer_event(
                    &mut sender,
                    &WorkerEventV1::Accepted {
                        lease: WorkerLeaseRefV1 {
                            worker_id: first.worker_id.clone(),
                            lease_id: first.lease_id.clone(),
                            unit_id: first.unit_id.clone(),
                        },
                        registry_generation: registration.registry_generation,
                    },
                )
                .await;
                accepted_tx.send(()).unwrap();

                match dealer_command(receiver.recv().await.unwrap()) {
                    SupervisorCommandV1::Cancel {
                        lease,
                        registry_generation,
                    } => {
                        assert_eq!(registry_generation, registration.registry_generation);
                        assert_eq!(lease.lease_id, first.lease_id);
                        assert_eq!(lease.unit_id, first.unit_id);
                    }
                    SupervisorCommandV1::Work { .. } => panic!("expected Cancel"),
                }
                dealer_event(
                    &mut sender,
                    &WorkerEventV1::Ready {
                        worker_id: registration.worker_id.clone(),
                        registry_generation: registration.registry_generation,
                    },
                )
                .await;
                cancelled_tx.send(()).unwrap();

                let second = match dealer_command(receiver.recv().await.unwrap()) {
                    SupervisorCommandV1::Work { lease, .. } => lease,
                    SupervisorCommandV1::Cancel { .. } => panic!("expected replacement Work"),
                };
                let reference = WorkerLeaseRefV1 {
                    worker_id: second.worker_id.clone(),
                    lease_id: second.lease_id.clone(),
                    unit_id: second.unit_id.clone(),
                };
                dealer_event(
                    &mut sender,
                    &WorkerEventV1::Accepted {
                        lease: reference,
                        registry_generation: registration.registry_generation,
                    },
                )
                .await;
                let unit_id = second.unit_id.parse().unwrap();
                dealer_event(
                    &mut sender,
                    &WorkerEventV1::Completed {
                        completion: WorkerCompletionV1 {
                            worker_id: second.worker_id,
                            lease_id: second.lease_id,
                            unit_id: second.unit_id,
                            finished_body_hex: hex::encode(
                                UnitFinishedV1 {
                                    unit_id,
                                    status: outbe_ocomp_protocol::UnitFinishedStatus::Failed,
                                    exact_staged_bytes: 0,
                                    transport_digest: B256::ZERO,
                                }
                                .encode_body(&limits)
                                .unwrap(),
                            ),
                        },
                        registry_generation: registration.registry_generation,
                    },
                )
                .await;
            });
    });

    let cancelled = Arc::new(AtomicBool::new(false));
    let dispatch_cancelled = Arc::clone(&cancelled);
    let dispatcher = server.dispatcher();
    let first_request = request(0x70);
    let first_dispatch = std::thread::spawn(move || {
        dispatcher.dispatch_with_cancel(&first_request, &dispatch_cancelled)
    });
    accepted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    cancelled.store(true, Ordering::Release);
    assert!(matches!(
        first_dispatch.join().unwrap(),
        Err(WorkerApiErrorV1::DispatchCancelled)
    ));
    cancelled_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let replacement = server.dispatch(&request(0x80)).unwrap();
    assert_eq!(
        replacement.status,
        outbe_ocomp_protocol::UnitFinishedStatus::Failed
    );
    worker.join().unwrap();
}

#[test]
fn expired_lease_is_requeued_and_stale_completion_conflicts() {
    let limits = crate::control::poc_schema_limits();
    let durations = WorkerApiDurations {
        registration: Duration::from_millis(50),
        lease: Duration::from_millis(20),
        idle: Duration::from_secs(1),
        result: Duration::from_secs(1),
    };
    let state = WorkerApiState::new(
        identity(),
        7,
        limits,
        durations,
        "tcp://127.0.0.1:9766".into(),
    );
    let mut first_identity = identity();
    first_identity.boot_nonce = B256::repeat_byte(0x51);
    let first = state
        .register(WorkerRegistrationV1::from_identity(first_identity, limits))
        .unwrap();
    let mut second_identity = identity();
    second_identity.boot_nonce = B256::repeat_byte(0x52);
    let second = state
        .register(WorkerRegistrationV1::from_identity(second_identity, limits))
        .unwrap();
    let first_id = parse_b256(&first.worker_id).unwrap();
    state.ready(first_id, &first.worker_id).unwrap();

    let (tx, _rx) = mpsc::sync_channel(1);
    state.lock().unwrap().pending.push_back(QueuedWork {
        queue_id: 1,
        unit_id: B256::repeat_byte(0x91),
        body: vec![1, 2, 3],
        delivery_attempt: 1,
        completion: tx,
    });
    let first_lease = match state.drain_commands().unwrap().pop().unwrap().1 {
        SupervisorCommandV1::Work { lease, .. } => lease,
        SupervisorCommandV1::Cancel { .. } => panic!("expected work command"),
    };
    assert!(matches!(
        state.heartbeat(WorkerLeaseRefV1 {
            worker_id: first_lease.worker_id.clone(),
            lease_id: first_lease.lease_id.clone(),
            unit_id: first_lease.unit_id.clone(),
        }),
        Err(WorkerApiErrorV1::LeaseNotAccepted)
    ));
    state
        .accepted(WorkerLeaseRefV1 {
            worker_id: first_lease.worker_id.clone(),
            lease_id: first_lease.lease_id.clone(),
            unit_id: first_lease.unit_id.clone(),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(30));
    state
        .lock()
        .unwrap()
        .workers
        .get_mut(&first_id)
        .unwrap()
        .connected = false;
    let second_id = parse_b256(&second.worker_id).unwrap();
    state.ready(second_id, &second.worker_id).unwrap();
    let second_lease = match state.drain_commands().unwrap().pop().unwrap().1 {
        SupervisorCommandV1::Work { lease, .. } => lease,
        SupervisorCommandV1::Cancel { .. } => panic!("expected work command"),
    };
    assert_eq!(second_lease.delivery_attempt, 2);
    assert_ne!(first_lease.lease_id, second_lease.lease_id);
    assert!(matches!(
        state.complete(WorkerCompletionV1 {
            worker_id: first_lease.worker_id,
            lease_id: first_lease.lease_id,
            unit_id: first_lease.unit_id,
            finished_body_hex: hex::encode(
                UnitFinishedV1 {
                    unit_id: B256::repeat_byte(0x91),
                    status: outbe_ocomp_protocol::UnitFinishedStatus::Failed,
                    exact_staged_bytes: 0,
                    transport_digest: B256::ZERO,
                }
                .encode_body(&limits)
                .unwrap(),
            ),
        }),
        Err(WorkerApiErrorV1::StaleLease)
    ));
    state
        .accepted(WorkerLeaseRefV1 {
            worker_id: second_lease.worker_id.clone(),
            lease_id: second_lease.lease_id.clone(),
            unit_id: second_lease.unit_id.clone(),
        })
        .unwrap();
    let completion = WorkerCompletionV1 {
        worker_id: second_lease.worker_id,
        lease_id: second_lease.lease_id,
        unit_id: second_lease.unit_id,
        finished_body_hex: hex::encode(
            UnitFinishedV1 {
                unit_id: B256::repeat_byte(0x91),
                status: outbe_ocomp_protocol::UnitFinishedStatus::Failed,
                exact_staged_bytes: 0,
                transport_digest: B256::ZERO,
            }
            .encode_body(&limits)
            .unwrap(),
        ),
    };
    state.complete(completion.clone()).unwrap();
    state.complete(completion.clone()).unwrap();
    let mut conflicting = completion;
    conflicting.finished_body_hex = hex::encode(
        UnitFinishedV1 {
            unit_id: B256::repeat_byte(0x91),
            status: outbe_ocomp_protocol::UnitFinishedStatus::Success,
            exact_staged_bytes: 1,
            transport_digest: B256::repeat_byte(0x92),
        }
        .encode_body(&limits)
        .unwrap(),
    );
    assert!(matches!(
        state.complete(conflicting),
        Err(WorkerApiErrorV1::ConflictingCompletion)
    ));
}

#[test]
fn stalled_http_connection_does_not_block_health_or_status() {
    let limits = crate::control::poc_schema_limits();
    let server =
        SupervisorWorkerServerV1::start("127.0.0.1:0".parse().unwrap(), identity(), 7, limits)
            .expect("start Axum and ZeroMQ worker server");
    let mut stalled = std::net::TcpStream::connect(server.address())
        .expect("open deliberately incomplete HTTP connection");
    stalled
        .write_all(
            b"POST /v1/workers/register HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4096\r\n",
        )
        .expect("write incomplete HTTP headers");

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let health_response = client
        .get(format!("http://{}/health", server.address()))
        .send()
        .expect("health remains independently serviceable");
    assert_eq!(health_response.status(), reqwest::StatusCode::OK);
    let status: WorkerApiStatusV1 = client
        .get(format!("http://{}/v1/status", server.address()))
        .send()
        .expect("status remains independently serviceable")
        .json()
        .expect("decode status");
    assert_eq!(status.registered_workers, 0);
    assert_eq!(status.connected_workers, 0);
    assert_eq!(status.max_workers, 4);
}
