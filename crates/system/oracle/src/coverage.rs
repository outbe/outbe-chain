//! `bounds`: the first block of each window hour (0 if none), then the window end block.

use crate::constants::MIN_WINDOW_COVERAGE;

pub fn meets_share(actual: u64, possible: u64) -> bool {
    let (numerator, denominator) = MIN_WINDOW_COVERAGE;
    actual.saturating_mul(denominator) >= possible.saturating_mul(numerator)
}

/// A window hour with blocks and the tally rounds they allowed.
#[derive(Clone, Copy)]
pub(crate) struct HourSpan {
    pub(crate) index: usize,
    first_block: u64,
    possible: u64,
}

/// An hour ends where the next hour with blocks begins.
pub(crate) fn hour_spans(bounds: &[u64], vote_period: u64) -> impl Iterator<Item = HourSpan> + '_ {
    let vote_period = vote_period.max(1);
    let last = bounds.len().saturating_sub(1);
    let window_end = bounds.last().copied().unwrap_or_default();
    bounds[..last]
        .iter()
        .enumerate()
        .filter(|(_, first_block)| **first_block != 0)
        .map(move |(index, &first_block)| {
            let end_block = bounds[index + 1..]
                .iter()
                .copied()
                .find(|block| *block != 0)
                .unwrap_or(window_end);
            HourSpan {
                index,
                first_block,
                possible: end_block.saturating_sub(first_block) / vote_period,
            }
        })
}

/// Whether enough hours count, and whether they hold enough of the window's rounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Verdict {
    pub hours: bool,
    pub rounds: bool,
}

impl Verdict {
    pub fn covered(self) -> bool {
        self.hours && self.rounds
    }
}

pub(crate) struct CoverageTally {
    hours: u64,
    window_end: u64,
    vote_period: u64,
    window_first: Option<u64>,
    counted_hours: u64,
    counted_snapshots: u64,
}

impl CoverageTally {
    pub(crate) fn new(bounds: &[u64], vote_period: u64) -> Self {
        Self {
            hours: bounds.len().saturating_sub(1) as u64,
            window_end: bounds.last().copied().unwrap_or_default(),
            vote_period: vote_period.max(1),
            window_first: None,
            counted_hours: 0,
            counted_snapshots: 0,
        }
    }

    pub(crate) fn record(&mut self, span: HourSpan, snapshots: u64) -> bool {
        self.window_first.get_or_insert(span.first_block);
        if snapshots == 0 || !meets_share(snapshots, span.possible) {
            return false;
        }
        self.counted_hours += 1;
        self.counted_snapshots = self.counted_snapshots.saturating_add(snapshots);
        true
    }

    /// `None` when no hour of the window saw a block.
    pub(crate) fn verdict(&self) -> Option<Verdict> {
        let window_first = self.window_first?;
        let possible = self.window_end.saturating_sub(window_first) / self.vote_period;
        Some(Verdict {
            hours: meets_share(self.counted_hours, self.hours),
            rounds: meets_share(self.counted_snapshots, possible),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowCoverage {
    pub counted: Vec<bool>,
    pub verdict: Verdict,
}

pub fn judge_window(bounds: &[u64], snapshots: &[u64], vote_period: u64) -> Option<WindowCoverage> {
    assert_eq!(
        bounds.len(),
        snapshots.len() + 1,
        "one bound per hour plus the window end"
    );
    let mut tally = CoverageTally::new(bounds, vote_period);
    let mut counted = vec![false; snapshots.len()];
    for span in hour_spans(bounds, vote_period) {
        counted[span.index] = tally.record(span, snapshots[span.index]);
    }
    tally
        .verdict()
        .map(|verdict| WindowCoverage { counted, verdict })
}
