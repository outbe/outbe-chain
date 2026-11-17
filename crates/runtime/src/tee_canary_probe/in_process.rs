//! Context-bound canary adapter; the existing worker owns scheduling and failure metrics.
use super::*;
use outbe_execution::tee_canary::{run_tee_canary_worker, TeeCanaryConfig, TeeCanaryTarget};
use outbe_tee::{
    in_process::InProcessTeeContext,
    protocol::{EnclaveRequest, EnclaveResponse},
    CallMeta, InProcessDispatchError, TeeEnclaveHealthChannel, TransportError,
};
use std::{sync::Arc, time::Instant};
use tokio_util::sync::CancellationToken;

struct ResidentCanaryRequester(Arc<InProcessTeeContext>);

impl EnclaveRequester for ResidentCanaryRequester {
    fn request(&self, request: &EnclaveRequest) -> Result<EnclaveResponse, TransportError> {
        match request {
            // Health is sidecar-only. The worker feature-detects this locally,
            // then uses GetPublicKeys and Canary payloads on subsequent ticks.
            EnclaveRequest::Health => Err(InProcessDispatchError::UnsupportedRequest.into()),
            EnclaveRequest::GetPublicKeys => {
                let context = &self.0;
                let ready = context
                    .offer_key_ready()
                    .map_err(InProcessDispatchError::from)?;
                metrics::gauge!("outbe_tee_enclave_uptime_seconds").set(context.uptime_s() as f64);
                // These series remain available, but no sidecar heap/request probe exists.
                metrics::gauge!("outbe_tee_enclave_heap_bytes", "kind" => "current").set(0.0);
                metrics::gauge!("outbe_tee_enclave_heap_bytes", "kind" => "peak").set(0.0);
                metrics::gauge!("outbe_tee_enclave_errored_requests").set(0.0);
                let recipient_x25519_pub = if ready {
                    context
                        .network_encryption_public()
                        .map_err(InProcessDispatchError::from)?
                } else {
                    context.recipient_public()
                };
                let manifest = context.manifest();
                Ok(EnclaveResponse::PublicKeys {
                    offer_key_ready: ready,
                    recipient_x25519_pub,
                    attestation_pub: manifest.attestation_ed25519,
                    noise_static_pub: manifest.noise_responder_x25519,
                    tee_bls_pub: context.tee_bls_public_bytes(),
                    dkg_enc_pub: context.dkg_enc_public(),
                    dkg_enc_sig: Vec::new(),
                })
            }
            EnclaveRequest::Payload {
                lane: PayloadLane::Canary,
                payload,
            } => outbe_enclave::host::domain_in_process::dispatch_payload(
                &self.0,
                PayloadLane::Canary,
                payload,
            ),
            _ => Err(InProcessDispatchError::LaneDenied {
                lane: PayloadLane::Canary,
            }
            .into()),
        }
    }

    fn request_with_meta(
        &self,
        meta: CallMeta,
        request: &EnclaveRequest,
    ) -> Result<EnclaveResponse, TransportError> {
        let started = Instant::now();
        let result = self.request(request);
        outbe_tee::metrics::record_request_duration(meta.label, started.elapsed());
        if let Err(error) = &result {
            outbe_tee::metrics::record_request_error(meta.label, error.metric_class());
        }
        result
    }

    fn attestation_pub(&self) -> Option<[u8; 32]> {
        Some(self.0.attestation_public())
    }
}

impl TributeCanaryProbe {
    /// Run the existing bounded periodic worker against the resident Canary lane.
    pub async fn run_in_process_worker(
        context: Arc<InProcessTeeContext>,
        config: TeeCanaryConfig,
        status: TeeEnclaveHealthChannel,
        shutdown: CancellationToken,
    ) {
        run_tee_canary_worker(
            TeeCanaryTarget::new(ResidentCanaryRequester(context), Self),
            config,
            status,
            shutdown,
        )
        .await;
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
        let _context_guard = outbe_tee::client_global::set_in_process_tee_context_for_test(Some((
            Arc::clone(&context),
            outbe_enclave::host::domain_in_process::dispatch_payload,
        )));
        let channel = TeeEnclaveHealthChannel::default();
        let shutdown = CancellationToken::new();
        let worker = tokio::spawn(TributeCanaryProbe::run_in_process_worker(
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
