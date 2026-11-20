//! Retry only typed chain observation failures, never local identity or journal faults.
use super::engine::SupervisorError;
use outbe_tee::lifecycle::{
    enrollment::EnrollmentError, renewal::RenewalError, LifecyclePortError, ReconcileError,
};

pub(super) fn observation<E, S>(error: &SupervisorError<E, S>) -> bool
where
    E: std::error::Error + 'static,
    S: std::error::Error + 'static,
{
    match error {
        SupervisorError::Enrollment(error) => enrollment(error),
        SupervisorError::Renewal(error) => renewal(error),
        other => observation_port(other).is_some(),
    }
}

pub(super) fn reconcile_port<E: std::error::Error + 'static>(
    error: &ReconcileError<E>,
) -> Option<&LifecyclePortError<E>> {
    match error {
        ReconcileError::Port(port) => Some(port),
        ReconcileError::Journal(_)
        | ReconcileError::Identity(_)
        | ReconcileError::CheckpointMismatch
        | ReconcileError::FinalizedMutationConflict
        | ReconcileError::WrongSender { .. } => None,
    }
}

fn reconcile<E: std::error::Error + 'static>(error: &ReconcileError<E>) -> bool {
    match error {
        ReconcileError::Port(_) | ReconcileError::CheckpointMismatch => true,
        ReconcileError::Journal(_)
        | ReconcileError::Identity(_)
        | ReconcileError::FinalizedMutationConflict
        | ReconcileError::WrongSender { .. } => false,
    }
}

pub(super) fn enrollment_port<E, S>(error: &EnrollmentError<E, S>) -> Option<&LifecyclePortError<E>>
where
    E: std::error::Error + 'static,
    S: std::error::Error + 'static,
{
    match error {
        EnrollmentError::Port(port) => Some(port),
        EnrollmentError::Reconcile(error) => reconcile_port(error),
        EnrollmentError::Journal(_)
        | EnrollmentError::Resident(_)
        | EnrollmentError::Codec(_)
        | EnrollmentError::Anchor(_)
        | EnrollmentError::Signer(_)
        | EnrollmentError::Proof(_)
        | EnrollmentError::CompetingLiveBinding
        | EnrollmentError::IdentityMismatch
        | EnrollmentError::CounterExhausted
        | EnrollmentError::InvalidConfiguration
        | EnrollmentError::WrongServiceTransaction
        | EnrollmentError::TransactionDecode(_)
        | EnrollmentError::AdmissionCodec(_)
        | EnrollmentError::Abi(_)
        | EnrollmentError::Signature(_)
        | EnrollmentError::InvalidSignature
        | EnrollmentError::TransactionHashMismatch => None,
    }
}

fn enrollment<E, S>(error: &EnrollmentError<E, S>) -> bool
where
    E: std::error::Error + 'static,
    S: std::error::Error + 'static,
{
    if let EnrollmentError::Reconcile(error) = error {
        return reconcile(error);
    }
    enrollment_port(error).is_some()
}

pub(super) fn renewal_port<E, S>(error: &RenewalError<E, S>) -> Option<&LifecyclePortError<E>>
where
    E: std::error::Error + 'static,
    S: std::error::Error + 'static,
{
    match error {
        RenewalError::Port(port) => Some(port),
        RenewalError::Reconcile(error) => reconcile_port(error),
        RenewalError::Journal(_)
        | RenewalError::Schedule(_)
        | RenewalError::FinalizedSchedule(_)
        | RenewalError::Resident(_)
        | RenewalError::Codec(_)
        | RenewalError::Collateral(_)
        | RenewalError::Dcap(_)
        | RenewalError::Signing(_)
        | RenewalError::BindingMismatch
        | RenewalError::UpgradeProofCheckpoint(_)
        | RenewalError::CounterOverflow
        | RenewalError::CollateralWindow { .. }
        | RenewalError::FeeOverflow
        | RenewalError::PendingOtherOperation
        | RenewalError::SubmittedHashMismatch
        | RenewalError::StalePending
        | RenewalError::PendingCalldata(_) => None,
    }
}

pub(super) fn renewal<E, S>(error: &RenewalError<E, S>) -> bool
where
    E: std::error::Error + 'static,
    S: std::error::Error + 'static,
{
    if let RenewalError::Reconcile(error) = error {
        return reconcile(error);
    }
    renewal_port(error).is_some()
}

pub(super) fn observation_port<E, S>(
    error: &SupervisorError<E, S>,
) -> Option<&LifecyclePortError<E>>
where
    E: std::error::Error + 'static,
    S: std::error::Error + 'static,
{
    match error {
        SupervisorError::Port(port) => Some(port),
        SupervisorError::Enrollment(error) => enrollment_port(error),
        SupervisorError::Renewal(error) => renewal_port(error),
        SupervisorError::Resident(_)
        | SupervisorError::Codec(_)
        | SupervisorError::Overflow
        | SupervisorError::FeeOverflow => None,
    }
}
