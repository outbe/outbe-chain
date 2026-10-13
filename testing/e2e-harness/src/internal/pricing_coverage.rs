//! Plan a covered pricing window with the Oracle's own coverage rule; the Oracle still decides.

use outbe_oracle::coverage::{judge_window, meets_share};

pub(crate) const HOUR: u64 = 3_600;
/// Snapshots an hour holds before the fixture leaves it.
pub(crate) const MIN_HOUR_SNAPSHOTS: u64 = 10;
/// Tally rounds a restart is assumed to cost the hour it leaves, until one is measured.
pub(crate) const RESTART_ROUNDS: u64 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PricingStep {
    Dwell,
    Advance,
    Close,
}

/// Decides on the prospective window ending with the current hour; `counts` holds
/// each currency's snapshots per hour.
pub(crate) fn pricing_step(bounds: &[u64], counts: &[Vec<u64>], period: u64) -> PricingStep {
    assert!(
        !counts.is_empty(),
        "a pricing window prices at least one currency"
    );
    let mut step = PricingStep::Close;
    for snapshots in counts {
        match currency_step(bounds, snapshots, period) {
            PricingStep::Dwell => return PricingStep::Dwell,
            PricingStep::Advance => step = PricingStep::Advance,
            PricingStep::Close => {}
        }
    }
    step
}

fn currency_step(bounds: &[u64], snapshots: &[u64], period: u64) -> PricingStep {
    let Some(coverage) = judge_window(bounds, snapshots, period) else {
        return PricingStep::Dwell;
    };
    let current = snapshots.len() - 1;
    if !coverage.counted[current] || snapshots[current] < MIN_HOUR_SNAPSHOTS {
        return PricingStep::Dwell;
    }
    if coverage.verdict.covered() {
        PricingStep::Close
    } else if coverage.verdict.hours {
        // Every voted round adds a snapshot and a possible round, so the share only rises.
        PricingStep::Dwell
    } else {
        PricingStep::Advance
    }
}

/// The first hour to cover: late enough that the head hour, whose early blocks carry no
/// votes, has left the window that closes after the fewest covered hours the rule accepts.
pub(crate) fn first_stepped_hour(head_time: u64, lookback: u64) -> u64 {
    let hours = lookback / HOUR;
    head_time - head_time % HOUR + (hours + 1 - required_hours(hours)) * HOUR
}

/// The block the current hour is judged up to: the measured restart tail lies ahead of it.
pub(crate) fn prospective_end(height: u64, tail_blocks: u64) -> u64 {
    height + tail_blocks + 1
}

pub(crate) fn step_limit(lookback: u64) -> usize {
    required_hours(lookback / HOUR) as usize + 2
}

fn required_hours(hours: u64) -> u64 {
    (0..=hours)
        .find(|&counted| meets_share(counted, hours))
        .unwrap_or(hours)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: u64 = 8;

    /// After the jump: two one-block hours, `covered` stepped hours, then the current hour.
    fn stepped(
        covered: usize,
        blocks: u64,
        snapshots: u64,
        current_blocks: u64,
        current: u64,
    ) -> (Vec<u64>, Vec<u64>) {
        let mut bounds = vec![0; 8 - covered - 3];
        let mut counts = vec![0; bounds.len()];
        let mut block = 100;
        for _ in 0..2 {
            bounds.push(block);
            counts.push(0);
            block += 1;
        }
        for _ in 0..covered {
            bounds.push(block);
            counts.push(snapshots);
            block += blocks;
        }
        bounds.push(block);
        counts.push(current);
        bounds.push(prospective_end(
            block + current_blocks,
            RESTART_ROUNDS * PERIOD,
        ));
        (bounds, counts)
    }

    fn step(bounds: &[u64], counts: &[u64]) -> PricingStep {
        pricing_step(bounds, &[counts.to_vec()], PERIOD)
    }

    #[test]
    fn an_hour_is_dwelt_in_until_it_counts() {
        let (bounds, counts) = stepped(0, 0, 0, 80, 3);
        assert_eq!(step(&bounds, &counts), PricingStep::Dwell);
        let (bounds, counts) = stepped(0, 0, 0, 144, 16);
        assert_eq!(step(&bounds, &counts), PricingStep::Advance);
    }

    #[test]
    fn rounds_lost_after_a_restart_are_repaid_before_advancing() {
        // Eight rounds without votes, then voting every round.
        let (bounds, counts) = stepped(2, 160, 18, 160, 12);
        assert_eq!(step(&bounds, &counts), PricingStep::Dwell);
        let (bounds, counts) = stepped(2, 160, 18, 224, 20);
        assert_eq!(step(&bounds, &counts), PricingStep::Advance);
    }

    #[test]
    fn a_counting_hour_below_the_snapshot_floor_is_dwelt_in() {
        let (bounds, counts) = stepped(3, 160, 18, 32, 4);
        assert_eq!(step(&bounds, &counts), PricingStep::Dwell);
    }

    #[test]
    fn six_covered_hours_close_the_window() {
        let (bounds, counts) = stepped(5, 160, 18, 144, 16);
        assert_eq!(step(&bounds, &counts), PricingStep::Close);
        let (bounds, counts) = stepped(4, 160, 18, 144, 16);
        assert_eq!(step(&bounds, &counts), PricingStep::Advance);
    }

    #[test]
    fn the_window_observed_after_the_old_jump_never_closes() {
        let bounds = [10, 11, 12, 13, 14, 15, 16, 17, 97];
        assert_eq!(
            step(&bounds, &[0, 0, 0, 0, 0, 0, 1, 10]),
            PricingStep::Advance
        );
    }

    #[test]
    fn a_short_earlier_hour_costs_one_more_hour() {
        // The second stepped hour fell short and never counts.
        let (mut bounds, mut counts) = stepped(5, 160, 18, 144, 16);
        counts[4] = 6;
        assert_eq!(step(&bounds, &counts), PricingStep::Advance);
        // One hour later the first jump hour has left the window.
        bounds.remove(0);
        counts.remove(0);
        let end = bounds.pop().unwrap();
        bounds.push(end - RESTART_ROUNDS * PERIOD - 1);
        counts.push(16);
        bounds.push(end + 144);
        assert_eq!(step(&bounds, &counts), PricingStep::Close);
    }

    #[test]
    fn rounds_short_with_enough_hours_dwell_until_the_share_rises() {
        // Eight hours of 21 rounds; one got no snapshot, so its rounds stay uncovered.
        let mut bounds: Vec<u64> = (0..=8).map(|k| 100 + k * 168).collect();
        let mut counts = vec![14, 14, 0, 14, 14, 14, 14, 14];
        assert_eq!(step(&bounds, &counts), PricingStep::Dwell);
        bounds[8] += 480;
        counts[7] += 60;
        assert_eq!(step(&bounds, &counts), PricingStep::Close);
    }

    #[test]
    fn every_currency_has_to_close() {
        let (bounds, closed) = stepped(5, 160, 18, 144, 16);
        let (_, short) = stepped(4, 160, 18, 144, 16);
        let mut short = short;
        short[2] = 0;
        assert_eq!(
            pricing_step(&bounds, &[closed.clone(), short], PERIOD),
            PricingStep::Advance
        );
        let mut dwelling = closed.clone();
        *dwelling.last_mut().unwrap() = 2;
        assert_eq!(
            pricing_step(&bounds, &[closed, dwelling], PERIOD),
            PricingStep::Dwell
        );
    }

    #[test]
    fn the_first_stepped_hour_leaves_the_head_hour_out_of_the_closing_window() {
        let head = 1_791_932_100;
        let hour = head - head % HOUR;
        assert_eq!(first_stepped_hour(head, 8 * HOUR), hour + 3 * HOUR);
        assert_eq!(first_stepped_hour(head, HOUR), hour + HOUR);
        assert_eq!(first_stepped_hour(head, 24 * HOUR), hour + 9 * HOUR);
        // After six stepped hours the closing window starts right after the head hour.
        assert_eq!(
            first_stepped_hour(head, 8 * HOUR) + 6 * HOUR - 8 * HOUR,
            hour + HOUR
        );
    }

    #[test]
    fn the_step_limit_allows_two_hours_beyond_the_rule() {
        assert_eq!(step_limit(8 * HOUR), 8);
        assert_eq!(step_limit(HOUR), 3);
    }

    #[test]
    fn a_closed_window_stays_covered_as_live_hours_roll_in() {
        let (mut bounds, mut counts) = stepped(5, 160, 18, 144, 16);
        assert_eq!(step(&bounds, &counts), PricingStep::Close);
        let mut block = bounds[bounds.len() - 1];
        for _ in 0..8 {
            bounds.remove(0);
            counts.remove(0);
            bounds.pop();
            bounds.push(block);
            counts.push(1_200);
            block += 1_800;
            bounds.push(block);
            assert!(judge_window(&bounds, &counts, PERIOD)
                .is_some_and(|coverage| coverage.verdict.covered()));
        }
    }
}
