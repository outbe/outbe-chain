//! Retry only typed chain observation failures, never local identity or journal faults.
use super::engine::SupervisorError;
use outbe_tee::lifecycle::{enrollment::EnrollmentError, renewal::RenewalError, ReconcileError};

pub(super) fn observation<E, S>(error: &SupervisorError<E, S>) -> bool
where
    E: std::error::Error + 'static,
    S: std::error::Error + 'static,
{
    match error {
        SupervisorError::Port(_) => true,
        SupervisorError::Enrollment(error) => enrollment(error),
        SupervisorError::Renewal(error) => renewal(error),
        _ => false,
    }
}

fn reconcile<E: std::error::Error + 'static>(error: &ReconcileError<E>) -> bool {
    matches!(
        error,
        ReconcileError::Port(_) | ReconcileError::CheckpointMismatch
    )
}

fn enrollment<E, S>(error: &EnrollmentError<E, S>) -> bool
where
    E: std::error::Error + 'static,
    S: std::error::Error + 'static,
{
    match error {
        EnrollmentError::Port(_) => true,
        EnrollmentError::Reconcile(error) => reconcile(error),
        _ => false,
    }
}

pub(super) fn renewal<E, S>(error: &RenewalError<E, S>) -> bool
where
    E: std::error::Error + 'static,
    S: std::error::Error + 'static,
{
    match error {
        RenewalError::Port(_) => true,
        RenewalError::Reconcile(error) => reconcile(error),
        _ => false,
    }
}
