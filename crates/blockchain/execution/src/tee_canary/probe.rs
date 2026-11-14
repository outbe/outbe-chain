//! Blocking canary stages. Telemetry and admission verdicts stay distinct.
use super::*;

struct ProbeTelemetry {
    supported: Option<bool>,
    payload: Option<EnclaveHealthStatusV1>,
}

pub(super) fn run(
    requester: &dyn EnclaveRequester,
    probe: &dyn TeeCanaryProbe,
    supported: Option<bool>,
) -> (
    CanaryTickOutcome,
    Option<bool>,
    Option<EnclaveHealthStatusV1>,
) {
    let telemetry = detect_health(requester, supported);
    let outcome = match read_offer_key(requester) {
        Ok(key) => probe.probe_decrypt(requester, &key),
        Err(outcome) => outcome,
    };
    (outcome, telemetry.supported, telemetry.payload)
}

fn detect_health(requester: &dyn EnclaveRequester, supported: Option<bool>) -> ProbeTelemetry {
    let unchanged = ProbeTelemetry {
        supported,
        payload: None,
    };
    if supported == Some(false) {
        return unchanged;
    }
    // A Health failure is ambiguous on old enclaves. A successful fallback
    // request rules out a dead connection. The probe still reads key readiness
    // afresh.
    match requester.request(&EnclaveRequest::Health) {
        Ok(EnclaveResponse::HealthStatus { status }) => ProbeTelemetry {
            supported: Some(true),
            payload: Some(*status),
        },
        Ok(_) | Err(_) if supported.is_none() => {
            if requester.request(&EnclaveRequest::GetPublicKeys).is_ok() {
                ProbeTelemetry {
                    supported: Some(false),
                    payload: None,
                }
            } else {
                unchanged
            }
        }
        Ok(_) | Err(_) => unchanged,
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
            unreachable: error.is_connection_fault()
                || matches!(
                    error,
                    TransportError::SessionRevoked(_)
                        | TransportError::EnclaveError(_)
                        | TransportError::Unavailable(_)
                ),
            reason: format!("GetPublicKeys failed: {}", error.metric_class()),
        }),
    }
}
