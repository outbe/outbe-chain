//! Check fixture observation coverage; the production Oracle remains the price authority.

/// Counts use the production hourly and whole-window two-thirds requirement.
/// `bounds` contains the first block of each hour (zero for empty hours), then
/// a prospective cutoff block that includes the caller's restart margin.
pub(crate) fn pricing_coverage_ready(bounds: &[u64], counts: &[u64], period: u64) -> bool {
    assert_eq!(bounds.len(), counts.len() + 1);
    assert!(period > 0);
    let end = bounds[counts.len()];
    let covered =
        |actual: u64, possible: u64| actual.saturating_mul(3) >= possible.saturating_mul(2);
    let first = bounds[..counts.len()]
        .iter()
        .copied()
        .find(|height| *height != 0);
    let counted = counts
        .iter()
        .enumerate()
        .filter_map(|(index, &observations)| {
            let block = bounds[index];
            if block == 0 || observations == 0 {
                return None;
            }
            let next = bounds[index + 1..]
                .iter()
                .copied()
                .find(|height| *height != 0)
                .unwrap_or(end);
            covered(observations, next.saturating_sub(block) / period).then_some(observations)
        })
        .fold(0_u64, u64::saturating_add);
    first.is_some_and(|first| counted > 0 && covered(counted, end.saturating_sub(first) / period))
}

#[cfg(test)]
mod tests {
    use super::pricing_coverage_ready;

    #[test]
    fn rejects_the_observed_late_feeder_window_even_with_one_valid_quote() {
        assert!(!pricing_coverage_ready(&[0, 1, 41], &[0, 1], 8));
        assert!(!pricing_coverage_ready(&[1, 57], &[4], 8));
        assert!(pricing_coverage_ready(&[1, 57], &[5], 8));
    }

    #[test]
    fn requires_coverage_of_the_whole_window_after_discarding_sparse_hours() {
        assert!(!pricing_coverage_ready(&[1, 81, 161], &[6, 10], 8));
        assert!(pricing_coverage_ready(&[1, 81, 161], &[7, 7], 8));
    }

    #[test]
    fn empty_hours_allow_neither_free_observations_nor_an_empty_price() {
        assert!(pricing_coverage_ready(&[0, 1, 0, 25], &[0, 2, 0], 8));
        assert!(!pricing_coverage_ready(&[0, 25], &[0], 8));
        assert!(!pricing_coverage_ready(&[1, 8], &[0], 8));
    }
}
