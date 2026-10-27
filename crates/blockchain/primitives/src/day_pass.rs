//! Scheduling of a pass that walks one closed UTC day at a time, every closed day in
//! order.

use crate::error::{PrecompileError, Result};
use crate::storage::dsl::Value;
use crate::time::{next_date_key, TimeError};

/// A pass step failed in storage or in the date helper.
#[derive(Debug)]
pub enum DayPassError {
    Storage(PrecompileError),
    Time(TimeError),
}

/// The day a pass is walking, 0 for none, and the newest closed day scheduled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassDays {
    pub current: u32,
    pub latest: u32,
}

/// What scheduling a closed day did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheduled {
    /// Nothing was running, so the first day not yet walked starts now.
    Opened,
    /// An earlier day is still being walked, so the day waits behind it.
    Queued,
    /// The day is already scheduled.
    Ignored,
}

/// This enum reports completion explicitly. Work can remain queued even when no economic state changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Pending,
    Complete,
}

impl PassDays {
    /// This method schedules `day` without changing the active pass.
    /// The pass processes each subsequent closed day in order, without skipping days.
    /// The pass cannot reopen completed days.
    pub fn schedule(mut self, day: u32) -> std::result::Result<(Self, Scheduled), TimeError> {
        let idle = self.current == 0;
        let scheduled = if day <= self.latest || (!idle && day <= self.current) {
            Scheduled::Ignored
        } else if idle {
            let first = match self.latest {
                0 => day,
                latest => next_date_key(latest)?,
            };
            self = Self {
                current: first,
                latest: day,
            };
            Scheduled::Opened
        } else {
            self.latest = day;
            Scheduled::Queued
        };
        Ok((self, scheduled))
    }

    /// The current day was walked to its end, so the next one starts, or the pass idles.
    pub fn finish(self) -> std::result::Result<Self, TimeError> {
        if self.current == 0 {
            return Ok(self);
        }
        let current = if self.current < self.latest {
            next_date_key(self.current)?
        } else {
            0
        };
        Ok(Self {
            current,
            latest: self.latest,
        })
    }
}

/// The stored current and newest scheduled days of one pass.
pub struct PinnedDay<'a, 'storage> {
    pub current: &'a Value<'storage, u32>,
    pub latest: &'a Value<'storage, u32>,
}

impl PinnedDay<'_, '_> {
    fn days(&self) -> std::result::Result<PassDays, DayPassError> {
        Ok(PassDays {
            current: self.current.read().map_err(DayPassError::Storage)?,
            latest: self.latest.read().map_err(DayPassError::Storage)?,
        })
    }

    /// Schedules `day` and stores the newest day scheduled. An opened day is stored by
    /// [`Self::open`].
    pub fn schedule(
        &self,
        day: u32,
    ) -> std::result::Result<(PassDays, Scheduled), DayPassError> {
        let (next, scheduled) = self.days()?.schedule(day).map_err(DayPassError::Time)?;
        if scheduled == Scheduled::Queued {
            self.latest
                .write(next.latest)
                .map_err(DayPassError::Storage)?;
        }
        Ok((next, scheduled))
    }

    /// Marks `day` walked by an idle pass with nothing to walk.
    pub fn pass(&self, day: u32) -> Result<()> {
        if day > self.latest.read()? {
            self.latest.write(day)?;
        }
        Ok(())
    }

    pub fn open(&self, days: PassDays) -> Result<()> {
        self.current.write(days.current)?;
        self.latest.write(days.latest)
    }

    /// Ends the walk of `pinned`. Returns the days to open next, or `None` once idle.
    pub fn finish(&self, pinned: u32) -> std::result::Result<Option<PassDays>, DayPassError> {
        let days = self.days()?;
        if pinned == 0 || days.current != pinned || days.latest < pinned {
            return Err(DayPassError::Storage(PrecompileError::revert(
                "invalid daily sweep completion",
            )));
        }
        let next = days.finish().map_err(DayPassError::Time)?;
        if next.current == 0 {
            self.current.write(0).map_err(DayPassError::Storage)?;
            return Ok(None);
        }
        Ok(Some(next))
    }
}

/// Index of the currency the cursor names, or the head when the registry dropped it.
pub fn currency_position(currencies: &[u16], cursor: u32) -> usize {
    u16::try_from(cursor)
        .ok()
        .and_then(|iso| currencies.iter().position(|&code| code == iso))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_AGO: u32 = 20251231;
    const YESTERDAY: u32 = 20260101;
    const TODAY: u32 = 20260102;

    fn days(current: u32, latest: u32) -> PassDays {
        PassDays { current, latest }
    }

    #[test]
    fn a_closed_day_opens_an_idle_sweep_and_waits_behind_a_running_one() {
        assert_eq!(
            PassDays::default().schedule(TODAY).unwrap(),
            (days(TODAY, TODAY), Scheduled::Opened)
        );
        assert_eq!(
            days(YESTERDAY, YESTERDAY).schedule(TODAY).unwrap(),
            (days(YESTERDAY, TODAY), Scheduled::Queued)
        );
    }

    #[test]
    fn a_day_already_scheduled_is_left_alone() {
        for scheduled in [days(TODAY, TODAY), days(YESTERDAY, TODAY)] {
            assert_eq!(
                scheduled.schedule(TODAY).unwrap(),
                (scheduled, Scheduled::Ignored)
            );
        }
        for scheduled in [days(TODAY, TODAY), days(0, TODAY)] {
            assert_eq!(
                scheduled.schedule(YESTERDAY).unwrap(),
                (scheduled, Scheduled::Ignored)
            );
        }
    }

    #[test]
    fn a_finished_day_cannot_be_walked_again() {
        let finished = days(TODAY, TODAY).finish().unwrap();
        assert_eq!(
            finished.schedule(TODAY).unwrap(),
            (finished, Scheduled::Ignored)
        );
    }

    #[test]
    fn every_day_a_stalled_sweep_missed_is_walked_in_order() {
        let (running, _) = days(TWO_AGO, YESTERDAY).schedule(TODAY).unwrap();
        assert_eq!(running, days(TWO_AGO, TODAY));
        assert_eq!(running.finish().unwrap(), days(YESTERDAY, TODAY));
        assert_eq!(
            running.finish().unwrap().finish().unwrap(),
            days(TODAY, TODAY)
        );
        assert_eq!(
            running
                .finish()
                .unwrap()
                .finish()
                .unwrap()
                .finish()
                .unwrap(),
            days(0, TODAY)
        );

        // An idle pass picks up after the newest day it scheduled.
        assert_eq!(
            days(0, TWO_AGO).schedule(TODAY).unwrap(),
            (days(YESTERDAY, TODAY), Scheduled::Opened)
        );
    }

    #[test]
    fn finishing_starts_the_next_day_or_goes_idle() {
        assert_eq!(days(YESTERDAY, TODAY).finish().unwrap(), days(TODAY, TODAY));
        assert_eq!(days(TODAY, TODAY).finish().unwrap(), days(0, TODAY));
        assert_eq!(days(0, TODAY).finish().unwrap(), days(0, TODAY));
    }

    fn with_pinned(test: impl FnOnce(&PinnedDay<'_, '_>)) {
        use crate::storage::{hashmap::HashMapStorageProvider, StorageHandle};
        use alloy_primitives::{address, U256};
        let contract = address!("0x0000000000000000000000000000000000001003");
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let current = Value::new(U256::from(1), contract, storage.clone());
            let latest = Value::new(U256::from(2), contract, storage);
            test(&PinnedDay {
                current: &current,
                latest: &latest,
            });
        });
    }

    #[test]
    fn a_pinned_day_stores_the_newest_day_and_leaves_opening_to_the_caller() {
        with_pinned(|pinned| {
            assert_eq!(
                pinned.schedule(YESTERDAY).unwrap(),
                (days(YESTERDAY, YESTERDAY), Scheduled::Opened)
            );
            assert_eq!(pinned.current.read().unwrap(), 0);
            pinned.open(days(YESTERDAY, YESTERDAY)).unwrap();
            assert_eq!(
                pinned.schedule(TODAY).unwrap(),
                (days(YESTERDAY, TODAY), Scheduled::Queued)
            );
            assert_eq!(
                (
                    pinned.current.read().unwrap(),
                    pinned.latest.read().unwrap()
                ),
                (YESTERDAY, TODAY)
            );
            assert_eq!(pinned.schedule(TODAY).unwrap().1, Scheduled::Ignored);
        });
    }

    #[test]
    fn finishing_a_pinned_day_hands_over_the_next_one_or_goes_idle() {
        with_pinned(|pinned| {
            pinned.open(days(YESTERDAY, TODAY)).unwrap();
            assert_eq!(pinned.finish(YESTERDAY).unwrap(), Some(days(TODAY, TODAY)));
            pinned.open(days(TODAY, TODAY)).unwrap();
            assert_eq!(pinned.finish(TODAY).unwrap(), None);
            assert_eq!(pinned.current.read().unwrap(), 0);
            assert_eq!(pinned.latest.read().unwrap(), TODAY);
        });
    }

    #[test]
    fn an_idle_pass_moves_the_newest_day_forward_only() {
        with_pinned(|pinned| {
            pinned.pass(TODAY).unwrap();
            pinned.pass(YESTERDAY).unwrap();
            assert_eq!(pinned.latest.read().unwrap(), TODAY);
            assert_eq!(pinned.current.read().unwrap(), 0);
        });
    }

    #[test]
    fn completion_rejects_a_stale_or_missing_pinned_day() {
        with_pinned(|pinned| {
            assert!(pinned.finish(0).is_err());
            pinned.open(days(YESTERDAY, TODAY)).unwrap();
            assert!(pinned.finish(TODAY).is_err());
            assert_eq!(pinned.current.read().unwrap(), YESTERDAY);
        });
    }

    #[test]
    fn a_cursor_names_its_currency_or_falls_back_to_the_head() {
        let currencies = [840, 978];
        assert_eq!(currency_position(&currencies, 978), 1);
        assert_eq!(currency_position(&currencies, 392), 0);
        assert_eq!(currency_position(&currencies, u32::MAX), 0);
    }
}
