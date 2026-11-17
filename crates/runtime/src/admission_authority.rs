//! Finalized admission publication and the shared in-memory lifecycle owner.

pub(crate) mod guard;
pub(crate) mod live;

use alloy_primitives::B256;
use outbe_consensus::validators::{LocalTeeRuntimeAdmissionV1, LocalTeeRuntimeRejectionV1};
use outbe_tee::TeeAuthorityState;

use crate::errors::TeeAuthorityError;

/// Only the activation-height founder path may run before a finalized registry lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BootstrapAdmissionPolicy {
    /// Founding TEE DKG completed in this process; block one is not finalized yet.
    AllowFoundingBootstrap,
    /// Recovery startup must prove a finalized registry lease before authority opens.
    RejectBootstrap,
}

/// Interprets finalized admission alongside an already verified resident key.
/// A mismatch is an invariant failure: the caller must close authority and report it.
pub(crate) fn admitted_authority_state(
    admission: LocalTeeRuntimeAdmissionV1,
    enclave_id: B256,
    resident_offer_key: Option<B256>,
    canonical_offer_key: Option<B256>,
    bootstrap_policy: BootstrapAdmissionPolicy,
) -> Result<TeeAuthorityState, TeeAuthorityError> {
    let Some(resident) = resident_offer_key else {
        return Ok(TeeAuthorityState::UnkeyedAwaitingAdmission);
    };
    match canonical_offer_key {
        Some(canonical) if resident != canonical => {
            return Err(TeeAuthorityError::ResidentOfferKeyMismatch {
                resident,
                canonical,
            });
        }
        None if admission != LocalTeeRuntimeAdmissionV1::BootstrapPending => {
            return Err(TeeAuthorityError::CanonicalOfferKeyUnavailable);
        }
        _ => {}
    }
    if admission == LocalTeeRuntimeAdmissionV1::BootstrapPending {
        return Ok(match bootstrap_policy {
            BootstrapAdmissionPolicy::AllowFoundingBootstrap if canonical_offer_key.is_none() => {
                TeeAuthorityState::ArmedBootstrap { enclave_id }
            }
            _ => TeeAuthorityState::SuspendedPolicyMismatch,
        });
    }
    Ok(match admission {
        LocalTeeRuntimeAdmissionV1::Ready { valid_until } => TeeAuthorityState::Armed {
            enclave_id,
            valid_until,
        },
        LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::ValidatorJailed) => {
            TeeAuthorityState::SuspendedJailed
        }
        LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::Expired { .. }) => {
            TeeAuthorityState::SuspendedLeaseExpired
        }
        LocalTeeRuntimeAdmissionV1::BootstrapPending | LocalTeeRuntimeAdmissionV1::Rejected(_) => {
            TeeAuthorityState::SuspendedPolicyMismatch
        }
    })
}

/// Notify authority consumers only when the verdict actually changes.
pub(crate) fn publish_state(channel: &outbe_tee::TeeAuthorityChannel, next: TeeAuthorityState) {
    if channel.snapshot() != next {
        channel.publish(next);
    }
}

/// Failure never erases a previously verified execution key.
fn publish_failure(channel: &outbe_tee::TeeAuthorityChannel, has_resident: bool) {
    publish_state(
        channel,
        if has_resident || channel.snapshot().has_verified_resident_key() {
            TeeAuthorityState::SuspendedPolicyMismatch
        } else {
            TeeAuthorityState::UnkeyedAwaitingAdmission
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_consensus::protocol_state::EngineProtocolState;
    use BootstrapAdmissionPolicy::{AllowFoundingBootstrap, RejectBootstrap};

    #[test]
    fn finalized_authority_requires_admission_and_the_canonical_resident_key() {
        use LocalTeeRuntimeAdmissionV1::{Ready, Rejected};
        use LocalTeeRuntimeRejectionV1::*;
        let key = B256::repeat_byte(1);
        let enclave_id = B256::repeat_byte(2);
        for (admission, expected) in [
            (
                Ready { valid_until: 42 },
                TeeAuthorityState::Armed {
                    enclave_id,
                    valid_until: 42,
                },
            ),
            (
                Rejected(ValidatorJailed),
                TeeAuthorityState::SuspendedJailed,
            ),
            (
                Rejected(Expired { valid_until: 42 }),
                TeeAuthorityState::SuspendedLeaseExpired,
            ),
            (
                Rejected(MissingBinding),
                TeeAuthorityState::SuspendedPolicyMismatch,
            ),
            (
                Rejected(EnclaveIdentityMismatch),
                TeeAuthorityState::SuspendedPolicyMismatch,
            ),
            (
                Rejected(ValidatorBindingMismatch),
                TeeAuthorityState::SuspendedPolicyMismatch,
            ),
            (
                Rejected(RetiredEnclave),
                TeeAuthorityState::SuspendedPolicyMismatch,
            ),
        ] {
            for policy in [AllowFoundingBootstrap, RejectBootstrap] {
                assert_eq!(
                    admitted_authority_state(admission, enclave_id, None, Some(key), policy)
                        .unwrap(),
                    TeeAuthorityState::UnkeyedAwaitingAdmission,
                );
                assert_eq!(
                    admitted_authority_state(admission, enclave_id, Some(key), Some(key), policy)
                        .unwrap(),
                    expected
                );
                assert!(matches!(
                    admitted_authority_state(admission, enclave_id, Some(B256::ZERO), Some(key), policy),
                    Err(TeeAuthorityError::ResidentOfferKeyMismatch { resident: B256::ZERO, canonical }) if canonical == key
                ));
                assert!(matches!(
                    admitted_authority_state(admission, enclave_id, Some(key), None, policy),
                    Err(TeeAuthorityError::CanonicalOfferKeyUnavailable)
                ));
            }
        }
    }

    #[test]
    fn founding_key_can_authorize_before_block_one_finalizes_but_recovery_cannot() {
        let key = B256::repeat_byte(1);
        let enclave_id = B256::repeat_byte(2);
        for (resident, canonical, policy, expected) in [
            (
                None,
                None,
                AllowFoundingBootstrap,
                TeeAuthorityState::UnkeyedAwaitingAdmission,
            ),
            (
                None,
                None,
                RejectBootstrap,
                TeeAuthorityState::UnkeyedAwaitingAdmission,
            ),
            (
                Some(key),
                None,
                AllowFoundingBootstrap,
                TeeAuthorityState::ArmedBootstrap { enclave_id },
            ),
            (
                Some(key),
                None,
                RejectBootstrap,
                TeeAuthorityState::SuspendedPolicyMismatch,
            ),
            (
                Some(key),
                Some(key),
                AllowFoundingBootstrap,
                TeeAuthorityState::SuspendedPolicyMismatch,
            ),
            (
                Some(key),
                Some(key),
                RejectBootstrap,
                TeeAuthorityState::SuspendedPolicyMismatch,
            ),
        ] {
            let state = admitted_authority_state(
                LocalTeeRuntimeAdmissionV1::BootstrapPending,
                enclave_id,
                resident,
                canonical,
                policy,
            )
            .unwrap();
            assert_eq!(state, expected);
            assert_eq!(
                state.is_authority_open(),
                matches!(expected, TeeAuthorityState::ArmedBootstrap { .. })
            );
            assert_eq!(state.has_verified_resident_key(), resident.is_some());
            assert!(!state.has_finalized_lease());
        }
        for policy in [AllowFoundingBootstrap, RejectBootstrap] {
            assert!(matches!(
                admitted_authority_state(
                    LocalTeeRuntimeAdmissionV1::BootstrapPending,
                    enclave_id,
                    Some(key),
                    Some(B256::ZERO),
                    policy
                ),
                Err(TeeAuthorityError::ResidentOfferKeyMismatch { .. })
            ));
        }
    }

    #[test]
    fn runtime_protocol_and_rpc_infrastructure_share_one_scoped_authority_port() {
        let _guard = outbe_tee::tee_authority_test_guard();
        let runtime = crate::OutbeRuntime::new();
        assert!(runtime.tee_authority_watch().is_none());
        assert_eq!(
            runtime.tee_authority_state(),
            TeeAuthorityState::UnkeyedAwaitingAdmission
        );
        let channel = outbe_tee::TeeAuthorityChannel::default();
        outbe_consensus::protocol_state::install_tee_authority_watch(channel.subscribe()).unwrap();
        let watch = runtime.tee_authority_watch().unwrap();
        for state in [
            TeeAuthorityState::ArmedBootstrap {
                enclave_id: B256::repeat_byte(2),
            },
            TeeAuthorityState::Armed {
                enclave_id: B256::repeat_byte(2),
                valid_until: 42,
            },
            TeeAuthorityState::SuspendedJailed,
        ] {
            channel.publish(state);
            assert_eq!(runtime.tee_authority_state(), state);
            assert_eq!(watch.snapshot(), state);
            assert_eq!(outbe_tee::current_tee_authority_state(), state);
        }
        assert!(matches!(
            outbe_consensus::protocol_state::install_tee_authority_watch(channel.subscribe()),
            Err(
                outbe_consensus::protocol_state::EngineProtocolError::TeeAuthority(
                    outbe_tee::TeeAuthorityInstallError::AlreadyInstalled
                )
            )
        ));
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

#[cfg(test)]
pub(crate) mod test_support;
