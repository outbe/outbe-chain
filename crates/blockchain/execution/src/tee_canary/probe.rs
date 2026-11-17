//! Blocking canary stages. Telemetry and admission verdicts stay distinct.
use super::*;

pub(super) fn run(
    requester: &dyn EnclaveRequester,
    probe: &dyn TeeCanaryProbe,
) -> CanaryTickOutcome {
    match read_offer_key(requester) {
        Ok(key) => probe.probe_decrypt(requester, &key),
        Err(outcome) => outcome,
    }
}

fn read_offer_key(requester: &dyn EnclaveRequester) -> Result<[u8; 32], CanaryTickOutcome> {
    // 2. Offer-key state (fresh each tick, so key epochs are followed).
    match requester.request(&EnclaveRequest::GetPublicKeys) {
        Ok(EnclaveResponse::PublicKeys {
            offer_key_ready,
            recipient_x25519_pub,
            ..
        }) => {
            if !offer_key_ready {
                return Err(CanaryTickOutcome::OfferKeyNotReady);
            }
            Ok(recipient_x25519_pub)
        }
        Ok(other) => Err(CanaryTickOutcome::Failure {
            unreachable: false,
            reason: format!("unexpected GetPublicKeys response: {other:?}"),
        }),
        Err(error) => Err(CanaryTickOutcome::Failure {
            unreachable: error.is_node_local()
                || matches!(
                    error,
                    TransportError::EnclaveError(_) | TransportError::Unavailable(_)
                ),
            reason: format!("GetPublicKeys failed: {}", error.metric_class()),
        }),
    }
}
