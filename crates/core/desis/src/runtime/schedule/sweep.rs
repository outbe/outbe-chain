//! Visit order of the `advance_auctions` sweep over the dense schedule set.

/// The dense indices that one sweep did not visit yet: `[index, end)`, then `[0, front)`.
/// The first lap goes from the cursor to the tail. The second lap goes from 0 to the cursor.
///
/// Swap-remove moves the tail day into the slot of the removed day. When the sweep did not
/// visit the moved day yet, the sweep visits that slot again. Otherwise it goes to the next slot.
/// So the sweep visits each day that was in the set at the start one time only.
pub(super) struct Sweep {
    index: u32,
    end: u32,
    front: u32,
}

impl Sweep {
    pub(super) fn new(start: u32, count: u32) -> Self {
        Self {
            index: start,
            end: count,
            front: start,
        }
    }

    pub(super) fn next(&mut self) -> Option<u32> {
        if self.index >= self.end {
            if self.front == 0 {
                return None;
            }
            (self.index, self.end, self.front) = (0, self.front, 0);
        }
        Some(self.index)
    }

    /// The visited day stays in the set.
    pub(super) fn kept(&mut self) {
        self.index += 1;
    }

    /// The visited day left the set, which now holds `count` days. The old tail was at `count`.
    pub(super) fn removed(&mut self, count: u32) {
        if count < self.end {
            // The sweep did not visit the old tail, or the old tail was the removed day.
            self.end = count;
        } else {
            self.index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Sweep;

    /// The days that a sweep from `start` visits in a set of `count` days. A day in the bit
    /// set `leaving` leaves the set when visited. `Vec::swap_remove` moves the tail as the set does.
    fn visits(count: u32, start: u32, leaving: u32) -> Vec<u32> {
        let mut set: Vec<u32> = (0..count).collect();
        let mut visited = Vec::new();
        let mut sweep = Sweep::new(start, count);
        while let Some(index) = sweep.next() {
            let day = set[index as usize];
            visited.push(day);
            if leaving & (1 << day) == 0 {
                sweep.kept();
                continue;
            }
            set.swap_remove(index as usize);
            sweep.removed(u32::try_from(set.len()).unwrap());
        }
        visited
    }

    /// For each start cursor and each choice of days that leave the set, the sweep visits each
    /// day of the set at the start one time.
    #[test]
    fn a_sweep_visits_each_day_once_under_swap_remove() {
        let cases = (1..=6u32).flat_map(|count| {
            (0..count).flat_map(move |start| {
                (0..(1u32 << count)).map(move |leaving| (count, start, leaving))
            })
        });
        for (count, start, leaving) in cases {
            let mut visited = visits(count, start, leaving);
            visited.sort_unstable();
            assert_eq!(
                visited,
                (0..count).collect::<Vec<_>>(),
                "count={count} start={start} leaving={leaving:b}"
            );
        }
    }
}
