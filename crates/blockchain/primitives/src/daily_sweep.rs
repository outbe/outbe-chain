//! Scheduling of a sweep that walks one closed UTC day at a time.

/// The day a sweep is walking and the one waiting behind it, 0 for none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepDays {
    pub current: u32,
    pub pending: u32,
}

/// What scheduling a closed day did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheduled {
    /// Nothing was running, so the day starts now.
    Opened,
    /// An earlier day is still being walked, so the day waits behind it.
    Queued,
    /// The day took the place of `skipped`, which will never be walked.
    Replaced { skipped: u32 },
    /// The day is already being walked or already waits.
    Ignored,
}

impl SweepDays {
    /// Schedules `day` without disturbing a sweep in flight. One day waits at most,
    /// so a backlog shows up as skipped days rather than as a growing queue.
    pub fn schedule(mut self, day: u32) -> (Self, Scheduled) {
        let scheduled = if day <= self.current.max(self.pending) {
            Scheduled::Ignored
        } else if self.current == 0 {
            self = Self {
                current: day,
                pending: 0,
            };
            Scheduled::Opened
        } else if self.pending == 0 {
            self.pending = day;
            Scheduled::Queued
        } else {
            let skipped = core::mem::replace(&mut self.pending, day);
            Scheduled::Replaced { skipped }
        };
        (self, scheduled)
    }

    /// The current day was walked to its end, so the waiting one starts.
    pub fn finish(self) -> Self {
        Self {
            current: self.pending,
            pending: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_AGO: u32 = 20251231;
    const YESTERDAY: u32 = 20260101;
    const TODAY: u32 = 20260102;

    fn days(current: u32, pending: u32) -> SweepDays {
        SweepDays { current, pending }
    }

    #[test]
    fn a_closed_day_opens_an_idle_sweep_and_waits_behind_a_running_one() {
        assert_eq!(
            SweepDays::default().schedule(TODAY),
            (days(TODAY, 0), Scheduled::Opened)
        );
        assert_eq!(
            days(YESTERDAY, 0).schedule(TODAY),
            (days(YESTERDAY, TODAY), Scheduled::Queued)
        );
    }

    #[test]
    fn a_day_already_walked_or_waiting_is_left_alone() {
        for scheduled in [days(TODAY, 0), days(YESTERDAY, TODAY)] {
            assert_eq!(scheduled.schedule(TODAY), (scheduled, Scheduled::Ignored));
        }
        assert_eq!(
            days(TODAY, 0).schedule(YESTERDAY),
            (days(TODAY, 0), Scheduled::Ignored)
        );
    }

    #[test]
    fn a_newer_day_replaces_the_one_waiting_and_names_it() {
        assert_eq!(
            days(TWO_AGO, YESTERDAY).schedule(TODAY),
            (
                days(TWO_AGO, TODAY),
                Scheduled::Replaced { skipped: YESTERDAY }
            )
        );
    }

    #[test]
    fn finishing_starts_the_waiting_day_or_goes_idle() {
        assert_eq!(days(YESTERDAY, TODAY).finish(), days(TODAY, 0));
        assert_eq!(days(TODAY, 0).finish(), SweepDays::default());
    }

    #[test]
    fn a_finished_day_can_be_walked_again() {
        let finished = days(TODAY, 0).finish();
        assert_eq!(finished.schedule(TODAY).1, Scheduled::Opened);
    }
}
