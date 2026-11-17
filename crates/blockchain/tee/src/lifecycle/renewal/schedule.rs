use super::RenewalStep;

#[derive(Debug, thiserror::Error)]
pub enum RenewalScheduleError {
    #[error("maximum lease must be positive and even")]
    InvalidLeasePeriod,
}

/// None means due; only non-submission outcomes escape the scheduler.
pub fn renewal_schedule_v1(
    now: u64,
    valid_until: u64,
    maximum_lease: u64,
) -> Result<Option<RenewalStep>, RenewalScheduleError> {
    if now >= valid_until {
        return Ok(Some(RenewalStep::LeaseAlreadyExpiredNeedsRejoin));
    }
    if maximum_lease == 0 || !maximum_lease.is_multiple_of(2) {
        return Err(RenewalScheduleError::InvalidLeasePeriod);
    }
    let opens_at_timestamp = valid_until.saturating_sub(maximum_lease / 2);
    Ok(if now < opens_at_timestamp {
        Some(RenewalStep::NotDue { opens_at_timestamp })
    } else {
        None
    })
}
