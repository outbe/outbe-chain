//! Pure admission-to-authority contract; lifecycle publication is wired separately.

use alloy_primitives::B256;
use outbe_consensus::validators::{LocalTeeRuntimeAdmissionV1, LocalTeeRuntimeRejectionV1};
use outbe_tee::TeeAuthorityState;

use crate::errors::TeeAuthorityError;

/// Interprets finalized admission alongside an already verified resident key.
/// A mismatch is an invariant failure: the caller must close authority and report it.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "The lease guard wires this admission contract in the next integration wave."
    )
)]
pub(crate) fn admitted_authority_state(
    admission: LocalTeeRuntimeAdmissionV1,
    enclave_id: B256,
    resident_offer_key: Option<B256>,
    canonical_offer_key: B256,
) -> Result<TeeAuthorityState, TeeAuthorityError> {
    let Some(resident) = resident_offer_key else {
        return Ok(TeeAuthorityState::UnkeyedAwaitingAdmission);
    };
    if resident != canonical_offer_key {
        return Err(TeeAuthorityError::ResidentOfferKeyMismatch {
            resident,
            canonical: canonical_offer_key,
        });
    }
    Ok(match admission {
        LocalTeeRuntimeAdmissionV1::Ready { valid_until } => TeeAuthorityState::Armed {
            enclave_id,
            valid_until,
        },
        LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::ValidatorJailed) => {
            TeeAuthorityState::SuspendedJailed {
                has_verified_resident_key: true,
            }
        }
        LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::Expired { .. }) => {
            TeeAuthorityState::SuspendedLeaseExpired {
                has_verified_resident_key: true,
            }
        }
        LocalTeeRuntimeAdmissionV1::BootstrapPending | LocalTeeRuntimeAdmissionV1::Rejected(_) => {
            TeeAuthorityState::SuspendedPolicyMismatch {
                has_verified_resident_key: true,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_consensus::protocol_state::EngineProtocolState;

    #[test]
    fn authority_requires_both_finalized_admission_and_the_canonical_resident_key() {
        use LocalTeeRuntimeAdmissionV1::{BootstrapPending, Ready, Rejected};
        use LocalTeeRuntimeRejectionV1::*;
        let key = B256::repeat_byte(1);
        let enclave_id = B256::repeat_byte(2);
        for admission in [
            Ready { valid_until: 42 },
            Rejected(ValidatorJailed),
            Rejected(Expired { valid_until: 42 }),
            BootstrapPending,
            Rejected(MissingBinding),
            Rejected(EnclaveIdentityMismatch),
            Rejected(ValidatorBindingMismatch),
            Rejected(RetiredEnclave),
        ] {
            assert_eq!(
                admitted_authority_state(admission, enclave_id, None, key).unwrap(),
                TeeAuthorityState::UnkeyedAwaitingAdmission,
            );
            let state = admitted_authority_state(admission, enclave_id, Some(key), key).unwrap();
            let expected = match admission {
                Ready { .. } => TeeAuthorityState::Armed {
                    enclave_id,
                    valid_until: 42,
                },
                Rejected(ValidatorJailed) => TeeAuthorityState::SuspendedJailed {
                    has_verified_resident_key: true,
                },
                Rejected(Expired { .. }) => TeeAuthorityState::SuspendedLeaseExpired {
                    has_verified_resident_key: true,
                },
                _ => TeeAuthorityState::SuspendedPolicyMismatch {
                    has_verified_resident_key: true,
                },
            };
            assert_eq!(state, expected);
            assert!(state.has_verified_resident_key());
            assert_eq!(state.is_authority_open(), matches!(admission, Ready { .. }));
            assert!(matches!(
                admitted_authority_state(admission, enclave_id, Some(B256::ZERO), key),
                Err(TeeAuthorityError::ResidentOfferKeyMismatch { resident: B256::ZERO, canonical }) if canonical == key
            ));
        }
    }

    #[test]
    fn existing_runtime_protocol_defaults_fail_closed() {
        let runtime = crate::OutbeRuntime::new();
        assert!(runtime.tee_authority_watch().is_none());
        assert_eq!(
            runtime.tee_authority_state(),
            TeeAuthorityState::UnkeyedAwaitingAdmission
        );
    }

    #[test]
    fn authority_errors_preserve_typed_causes() {
        use std::error::Error;
        let error = TeeAuthorityError::from(outbe_tee::in_process::InProcessError::NotReady);
        assert!(error
            .source()
            .unwrap()
            .is::<outbe_tee::in_process::InProcessError>());
        let error = TeeAuthorityError::from(outbe_primitives::error::PrecompileError::OutOfGas);
        assert!(error
            .source()
            .unwrap()
            .is::<outbe_primitives::error::PrecompileError>());
    }
}
