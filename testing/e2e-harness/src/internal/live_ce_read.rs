//! Accept a live CE observation only while its canonical checkpoint is stable.

/// A latest-header/CE-marker race can fail before the caller checks the head again.
/// Retry that specific error only if the caller independently observed a head change.
pub(crate) fn resolve_live_ce_read<T>(
    observation: Result<T, String>,
    head_changed: bool,
) -> Result<Option<T>, String> {
    match observation {
        Ok(value) => Ok((!head_changed).then_some(value)),
        Err(error)
            if head_changed
                && error.contains("compressed-entity tree unavailable: exact parent mismatch:") =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_live_ce_read;

    const PARENT_RACE: &str = "eth_call failed: Revm error: fatal: compressed-entity tree unavailable: exact parent mismatch: required parent135, marker136";

    #[test]
    fn accepts_only_a_successful_observation_at_a_stable_checkpoint() {
        assert_eq!(resolve_live_ce_read(Ok(42), false), Ok(Some(42)));
        assert_eq!(resolve_live_ce_read(Ok(42), true), Ok(None));
    }

    #[test]
    fn retries_a_parent_race_only_after_an_observed_head_change() {
        assert_eq!(
            resolve_live_ce_read::<u64>(Err(PARENT_RACE.into()), true),
            Ok(None)
        );
        assert_eq!(
            resolve_live_ce_read::<u64>(Err(PARENT_RACE.into()), false),
            Err(PARENT_RACE.into())
        );
    }

    #[test]
    fn never_retries_unrelated_rpc_or_authentication_failures() {
        for error in [
            "connection refused",
            "invalid ciphertext",
            "execution reverted",
            "compressed-entity body authentication failed",
        ] {
            for changed in [false, true] {
                assert_eq!(
                    resolve_live_ce_read::<u64>(Err(error.into()), changed),
                    Err(error.into())
                );
            }
        }
    }
}
