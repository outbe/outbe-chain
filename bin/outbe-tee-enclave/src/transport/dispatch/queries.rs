//! Handle queries requests after session admission.

use super::requests::RequestContext;
use crate::transport::*;

pub(super) fn dispatch(req: EnclaveRequest, context: RequestContext<'_>) -> EnclaveResponse {
    let RequestContext {
        keys, offer_key, ..
    } = context;
    match req {
        EnclaveRequest::Health => {
            let (
                requests_total,
                requests_errored,
                requests_denied,
                class_initialized,
                class_founding_keyless,
                class_keyless_onboarding,
                class_ready,
                class_dev_source_seal,
                class_dev_recipient_ingest,
            ) = crate::telemetry::counters_snapshot();
            let (heap_current_bytes, heap_peak_bytes) = crate::telemetry::heap_snapshot();
            EnclaveResponse::HealthStatus {
                status: Box::new(outbe_tee::protocol::EnclaveHealthStatusV1 {
                    uptime_s: crate::telemetry::uptime_s(),
                    offer_key_ready: offer_key.get().is_some(),
                    heap_current_bytes,
                    heap_peak_bytes,
                    requests_total,
                    requests_errored,
                    requests_denied,
                    class_initialized,
                    class_founding_keyless,
                    class_keyless_onboarding,
                    class_ready,
                    class_dev_source_seal,
                    class_dev_recipient_ingest,
                }),
            }
        }
        EnclaveRequest::GetPublicKeys => EnclaveResponse::PublicKeys {
            offer_key_ready: offer_key.get().is_some(),
            // Advertise the DKG-derived offer key once available, so clients
            // encrypt to it. Before readiness the same field carries only the
            // one-time onboarding recipient and is never permanent chain state.
            recipient_x25519_pub: offer_key
                .get()
                .map(|k| k.public())
                .unwrap_or_else(|| keys.tribute_offer_public()),
            attestation_pub: keys.attestation_pub(),
            noise_static_pub: keys.noise_public(),
            tee_bls_pub: keys.tee_bls_public_bytes(),
            dkg_enc_pub: keys.dkg_enc_public(),
            // A share-recipient key is trusted only through the scoped
            // DkgParticipantAnnounceV1 response below.
            dkg_enc_sig: Vec::new(),
        },
        _ => unreachable!("request family is checked by the exhaustive dispatcher"),
    }
}
