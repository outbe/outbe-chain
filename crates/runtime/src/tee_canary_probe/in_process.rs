//! Context-bound canary adapter; the existing worker owns scheduling and failure metrics.
use super::*;
use outbe_execution::tee_canary::{
    run_tee_canary_worker, CanaryTickOutcome, EnclaveRequester, TeeCanaryConfig, TeeCanaryProbe,
    TeeCanaryTarget,
};
use outbe_tee::{
    in_process::InProcessTeeContext,
    protocol::{EnclaveRequest, EnclaveResponse},
    CallMeta, InProcessDispatchError, TeeEnclaveHealthChannel, TransportError,
};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

/// One resident generation for a canary tick.
///
/// `fallback` is only the startup context used before a runtime is installed.
/// `current` is the single snapshot shared by `GetPublicKeys` and decrypt.
struct ResidentLane {
    fallback: Arc<InProcessTeeContext>,
    current: Mutex<Option<Arc<InProcessTeeContext>>>,
}

impl ResidentLane {
    fn installed_or_fallback(&self) -> Arc<InProcessTeeContext> {
        outbe_tee::in_process_tee_context().unwrap_or_else(|| Arc::clone(&self.fallback))
    }

    fn begin_tick(&self) -> Arc<InProcessTeeContext> {
        let context = self.installed_or_fallback();
        *self.current.lock().expect("canary tick") = Some(Arc::clone(&context));
        context
    }

    fn tick_snapshot(&self) -> Arc<InProcessTeeContext> {
        self.current
            .lock()
            .expect("canary tick")
            .clone()
            .unwrap_or_else(|| self.installed_or_fallback())
    }

    fn end_tick(&self) {
        *self.current.lock().expect("canary tick") = None;
    }
}

fn resident_lane(fallback: Arc<InProcessTeeContext>) -> Arc<ResidentLane> {
    Arc::new(ResidentLane {
        fallback,
        current: Mutex::new(None),
    })
}

struct ResidentCanaryRequester(Arc<ResidentLane>);

impl ResidentCanaryRequester {
    fn dispatch(&self, request: &EnclaveRequest) -> Result<EnclaveResponse, TransportError> {
        let context = self.0.begin_tick();
        match request {
            EnclaveRequest::GetPublicKeys => {
                let ready = context
                    .offer_key_ready()
                    .map_err(InProcessDispatchError::from)?;
                metrics::gauge!("outbe_tee_enclave_uptime_seconds").set(context.uptime_s() as f64);
                let recipient_x25519_pub = if ready {
                    context
                        .network_encryption_public()
                        .map_err(InProcessDispatchError::from)?
                } else {
                    context.recipient_public()
                };
                Ok(EnclaveResponse::PublicKeys {
                    offer_key_ready: ready,
                    recipient_x25519_pub,
                    attestation_pub: context.attestation_public(),
                    noise_static_pub: context.noise_public(),
                    tee_bls_pub: context.tee_bls_public_bytes(),
                    dkg_enc_pub: context.dkg_enc_public(),
                    dkg_enc_sig: Vec::new(),
                })
            }
            _ => Err(InProcessDispatchError::UnsupportedRequest.into()),
        }
    }
}

impl EnclaveRequester for ResidentCanaryRequester {
    fn request(&self, request: &EnclaveRequest) -> Result<EnclaveResponse, TransportError> {
        self.request_with_meta(CallMeta::session_only(request.label()), request)
    }

    fn request_with_meta(
        &self,
        meta: CallMeta,
        request: &EnclaveRequest,
    ) -> Result<EnclaveResponse, TransportError> {
        let started = Instant::now();
        let result = self.dispatch(request);
        outbe_tee::metrics::record_request_duration(meta.label, started.elapsed());
        if let Err(error) = &result {
            outbe_tee::metrics::record_request_error(meta.label, error.metric_class());
        }
        result
    }

    fn attestation_pub(&self) -> Option<[u8; 32]> {
        Some(self.0.tick_snapshot().attestation_public())
    }
}

struct BoundCanary(Arc<ResidentLane>);

impl TeeCanaryProbe for BoundCanary {
    fn probe_decrypt(
        &self,
        requester: &dyn EnclaveRequester,
        offer_pub: &[u8; 32],
    ) -> CanaryTickOutcome {
        let context = self.0.tick_snapshot();
        let outcome = super::decrypt_canary_on(requester, &context, offer_pub);
        self.0.end_tick();
        outcome
    }
}

impl OfferCanaryProbe {
    /// Run the existing bounded periodic worker against the resident Canary lane.
    pub async fn run_in_process_worker(
        context: Arc<InProcessTeeContext>,
        config: TeeCanaryConfig,
        status: TeeEnclaveHealthChannel,
        shutdown: CancellationToken,
    ) {
        let lane = resident_lane(context);
        run_tee_canary_worker(
            TeeCanaryTarget::new(
                ResidentCanaryRequester(Arc::clone(&lane)),
                BoundCanary(lane),
            ),
            config,
            status,
            shutdown,
        )
        .await;
    }
}

#[cfg(test)]
mod bound_canary_tests {
    use super::*;
    use outbe_execution::tee_canary::CanaryTickOutcome;
    use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};

    struct Silent;

    impl EnclaveRequester for Silent {
        fn request(&self, _request: &EnclaveRequest) -> Result<EnclaveResponse, TransportError> {
            Err(TransportError::UnexpectedResponse)
        }

        fn attestation_pub(&self) -> Option<[u8; 32]> {
            None
        }
    }

    fn offer_material(
        context: &InProcessTeeContext,
        seed: u8,
    ) -> outbe_tee::resident::ResidentOfferMaterial {
        outbe_tee::resident::ResidentOfferMaterial::for_test(
            vec![seed; 96],
            alloy_primitives::B256::from(context.manifest().chain_id),
            0,
            0,
        )
        .expect("offer material")
    }

    #[test]
    fn bound_canary_rejects_a_cold_key_and_opens_after_activation() {
        let cold = crate::tee_canary_probe::test_support::context(6);
        assert!(matches!(
            BoundCanary(super::resident_lane(Arc::clone(&cold)))
                .probe_decrypt(&Silent, &[0x44; 32]),
            CanaryTickOutcome::Failure {
                unreachable: true,
                ..
            }
        ));
        crate::tee_canary_probe::test_support::activate(&cold);
        let offer_pub = cold.network_encryption_public().expect("offer public");
        let lane = super::resident_lane(cold);
        let requester = ResidentCanaryRequester(Arc::clone(&lane));
        assert!(matches!(
            BoundCanary(lane).probe_decrypt(&requester, &offer_pub),
            CanaryTickOutcome::Success { .. }
        ));
    }

    #[test]
    fn bound_canary_decrypts_the_promoted_resident() {
        let old = crate::tee_canary_probe::test_support::context(3);
        old.activate_for_test(offer_material(&old, 5))
            .expect("old offer");
        let promoted = crate::tee_canary_probe::test_support::context(9);
        promoted
            .activate_for_test(offer_material(&promoted, 9))
            .expect("promoted offer");
        let cold = crate::tee_canary_probe::test_support::context(11);
        let old_pub = old.network_encryption_public().expect("old key");
        let promoted_pub = promoted.network_encryption_public().expect("promoted key");
        assert_ne!(old_pub, promoted_pub);
        assert!(old.offer_key_ready().expect("old readiness"));
        assert!(!cold.offer_key_ready().expect("cold readiness"));

        let shared = super::resident_lane(Arc::clone(&old));
        let requester = ResidentCanaryRequester(Arc::clone(&shared));
        let probe = BoundCanary(Arc::clone(&shared));
        let _guard = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(
            Arc::clone(&old),
        ));

        requester
            .request(&EnclaveRequest::GetPublicKeys)
            .expect("old keys");
        let pinned = shared
            .current
            .lock()
            .expect("canary tick")
            .clone()
            .expect("snapshot");
        assert!(Arc::ptr_eq(&pinned, &old));
        assert!(matches!(
            super::decrypt_canary_on(&requester, &pinned, &promoted_pub),
            CanaryTickOutcome::Failure {
                unreachable: false,
                ..
            }
        ));
        assert!(matches!(
            probe.probe_decrypt(&requester, &old_pub),
            CanaryTickOutcome::Success { .. }
        ));

        outbe_tee::replace_in_process_tee_context::<InProcessDispatchError>(
            &old,
            Arc::clone(&promoted),
            || Ok(()),
        )
        .expect("promote hot resident");
        requester
            .request(&EnclaveRequest::GetPublicKeys)
            .expect("promoted keys");
        let pinned = shared
            .current
            .lock()
            .expect("canary tick")
            .clone()
            .expect("promoted snapshot");
        assert!(Arc::ptr_eq(&pinned, &promoted));
        assert!(matches!(
            super::decrypt_canary_on(&requester, &pinned, &old_pub),
            CanaryTickOutcome::Failure {
                unreachable: false,
                ..
            }
        ));
        assert!(matches!(
            probe.probe_decrypt(&requester, &promoted_pub),
            CanaryTickOutcome::Success { .. }
        ));

        outbe_tee::replace_in_process_tee_context::<InProcessDispatchError>(
            &promoted,
            Arc::clone(&cold),
            || Ok(()),
        )
        .expect("promote cold resident");
        assert!(old.offer_key_ready().expect("old key stays ready"));
        requester
            .request(&EnclaveRequest::GetPublicKeys)
            .expect("cold keys");
        let pinned = shared
            .current
            .lock()
            .expect("canary tick")
            .clone()
            .expect("cold snapshot");
        assert!(Arc::ptr_eq(&pinned, &cold));
        assert!(matches!(
            probe.probe_decrypt(&requester, &old_pub),
            CanaryTickOutcome::Failure {
                unreachable: true,
                ..
            }
        ));
    }
}

#[cfg(test)]
mod rpc_tests {
    use super::*;
    use outbe_rpc::OutbeApiServer;
    use reth_provider::test_utils::NoopProvider;

    #[tokio::test]
    async fn periodic_canary_publishes_pre_key_and_ready_json_with_context_uptime() {
        let context = crate::tee_canary_probe::test_support::context(8);
        let _context_guard = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(
            Some(Arc::clone(&context)),
        );
        let channel = TeeEnclaveHealthChannel::default();
        let shutdown = CancellationToken::new();
        let worker = tokio::spawn(OfferCanaryProbe::run_in_process_worker(
            Arc::clone(&context),
            TeeCanaryConfig {
                interval: std::time::Duration::from_millis(5),
                failure_threshold: 1,
            },
            channel.clone(),
            shutdown.clone(),
        ));
        let provider: Arc<
            NoopProvider<
                reth_chainspec::ChainSpec<outbe_primitives::OutbeHeader>,
                outbe_primitives::OutbePrimitives,
            >,
        > = Arc::new(NoopProvider::new(Arc::new(
            reth_chainspec::ChainSpec::default(),
        )));
        let rpc = outbe_rpc::OutbeApiHandler::new(provider, Arc::new(crate::OutbeRuntime::new()))
            .with_tee_enclave_health(channel.clone());
        for ready in [false, true] {
            if ready {
                crate::tee_canary_probe::test_support::activate(&context);
            }
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let snapshot = channel.snapshot();
                    if snapshot.health_probe_supported == Some(false)
                        && snapshot.offer_key_ready == ready
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("worker publishes resident key readiness");
            let info = rpc.consensus_status().await.unwrap().enclave;
            assert_eq!(info.uptime_s, Some(context.uptime_s()));
            assert_eq!(info.ready, ready);
            assert_eq!(info.offer_key_ready, ready);
            assert_eq!(info.consecutive_failures, 0);
            assert_eq!(info.last_failure_class, None);
            assert_eq!(info.heap_current_bytes, None);
            assert_eq!(info.heap_peak_bytes, None);
            assert_eq!(info.health_probe_supported, Some(false));
            assert_eq!(info.last_canary_latency_ms.is_some(), ready);
            assert_eq!(info.last_ok_ago_millis.is_some(), ready);
        }
        shutdown.cancel();
        worker.await.unwrap();
    }
}

#[cfg(test)]
mod metrics_tests {
    use super::*;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    #[test]
    fn resident_requests_report_real_metrics_once_without_synthetic_gauges() {
        let context = crate::tee_canary_probe::test_support::context(8);
        let requester = ResidentCanaryRequester(super::resident_lane(Arc::clone(&context)));
        let recorder = DebuggingRecorder::new();
        let snapshots = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            let error = requester
                .request_with_meta(
                    CallMeta::session_only("denied_probe"),
                    &EnclaveRequest::DkgStartDealer {
                        ceremony_id: alloy_primitives::B256::ZERO,
                    },
                )
                .unwrap_err();
            assert!(matches!(
                error,
                TransportError::InProcess(InProcessDispatchError::UnsupportedRequest)
            ));
            let response = requester.request(&EnclaveRequest::GetPublicKeys).unwrap();
            assert!(matches!(response, EnclaveResponse::PublicKeys {
                attestation_pub, noise_static_pub, ..
            } if attestation_pub == context.attestation_public() && noise_static_pub == context.noise_public()));
            requester
                .request_with_meta(
                    CallMeta::session_only("keys_probe"),
                    &EnclaveRequest::GetPublicKeys,
                )
                .unwrap();
        });
        let values = snapshots.snapshot().into_vec();
        for absent in [
            "outbe_tee_enclave_heap_bytes",
            "outbe_tee_enclave_errored_requests",
        ] {
            assert!(
                !values
                    .iter()
                    .any(|(key, _, _, _)| key.key().name() == absent),
                "synthetic {absent}"
            );
        }
        for label in ["denied_probe", "get_public_keys", "keys_probe"] {
            let value = &values
                .iter()
                .find(|(key, _, _, _)| {
                    key.key().name() == "outbe_tee_client_request_duration_ms"
                        && key
                            .key()
                            .labels()
                            .any(|item| item.key() == "request" && item.value() == label)
                })
                .unwrap_or_else(|| panic!("missing duration for {label}"))
                .3;
            assert!(
                matches!(value, DebugValue::Histogram(samples) if samples.len() == 1),
                "{label}: {value:?}"
            );
        }
        let errors = values
            .iter()
            .filter(|(key, _, _, _)| key.key().name() == "outbe_tee_client_request_errors_total")
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 1);
        assert!(errors[0]
            .0
            .key()
            .labels()
            .any(|item| item.key() == "request" && item.value() == "denied_probe"));
        assert_eq!(errors[0].3, DebugValue::Counter(1));
    }

    #[test]
    fn bound_canary_emits_offer_batch_request_metrics() {
        struct Silent;

        impl EnclaveRequester for Silent {
            fn request(
                &self,
                _request: &EnclaveRequest,
            ) -> Result<EnclaveResponse, TransportError> {
                Err(TransportError::UnexpectedResponse)
            }

            fn attestation_pub(&self) -> Option<[u8; 32]> {
                None
            }
        }

        let cold = crate::tee_canary_probe::test_support::context(6);
        assert_offer_batch(Some("in_process"), || {
            assert!(matches!(
                BoundCanary(super::resident_lane(Arc::clone(&cold)))
                    .probe_decrypt(&Silent, &[0x44; 32]),
                CanaryTickOutcome::Failure {
                    unreachable: true,
                    ..
                }
            ));
        });
        crate::tee_canary_probe::test_support::activate(&cold);
        let offer_pub = cold.network_encryption_public().expect("offer public");
        let lane = super::resident_lane(cold);
        let requester = ResidentCanaryRequester(Arc::clone(&lane));
        assert_offer_batch(None, || {
            assert!(matches!(
                BoundCanary(lane).probe_decrypt(&requester, &offer_pub),
                CanaryTickOutcome::Success { .. }
            ));
        });
        assert_offer_batch(Some("unavailable"), || {
            assert!(matches!(
                OfferCanaryProbe.probe_decrypt(&Silent, &[0x44; 32]),
                CanaryTickOutcome::Failure {
                    unreachable: true,
                    ..
                }
            ));
        });
    }

    fn assert_offer_batch(class: Option<&str>, probe: impl FnOnce()) {
        let recorder = DebuggingRecorder::new();
        let snapshots = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, probe);
        let values = snapshots.snapshot().into_vec();
        let duration = values
            .iter()
            .find(|(key, _, _, _)| offer_batch(key.key(), "outbe_tee_client_request_duration_ms"))
            .unwrap_or_else(|| panic!("missing offer-batch duration"));
        assert!(
            matches!(&duration.3, DebugValue::Histogram(samples) if samples.len() == 1),
            "{:?}",
            duration.3
        );
        let errors = values
            .iter()
            .filter(|(key, _, _, _)| {
                offer_batch(key.key(), "outbe_tee_client_request_errors_total")
            })
            .collect::<Vec<_>>();
        match class {
            None => assert!(errors.is_empty()),
            Some(class) => {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].3, DebugValue::Counter(1));
                assert!(errors[0]
                    .0
                    .key()
                    .labels()
                    .any(|item| { item.key() == "class" && item.value() == class }));
            }
        }
    }

    fn offer_batch(key: &metrics::Key, name: &str) -> bool {
        key.name() == name
            && key
                .labels()
                .any(|item| item.key() == "request" && item.value() == "open_offer")
    }
}
