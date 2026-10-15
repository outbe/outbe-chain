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

#[cfg(feature = "ocomp-integration")]
use eyre::{ensure, eyre};

#[cfg(feature = "ocomp-integration")]
pub(crate) struct ProjectionSample<T> {
    pub(crate) before: u64,
    pub(crate) observation: Result<Option<T>, String>,
    pub(crate) after: u64,
}

// The RPC diagnostic is not a stable wire format: unknown spellings fail closed.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn ce_projection_not_ready(error: &str, before: u64, after: u64) -> bool {
    fn fields<'a>(text: &'a str, names: &[&str]) -> Option<Vec<&'a str>> {
        let values: Vec<_> = text.split(", ").collect();
        if values.len() != names.len() {
            return None;
        }
        values
            .into_iter()
            .zip(names)
            .map(|(value, name)| value.strip_prefix(*name)?.strip_prefix(": "))
            .collect()
    }
    let parsed = (|| -> Option<()> {
        let detail = error.strip_prefix(
            "eth_call failed: server returned an error response: error code -32603: Revm error: fatal: compressed-entity tree unavailable: exact parent mismatch: required ExactParentIdentity { ",
        ).or_else(|| error.strip_prefix(
            "eth_call failed: server returned an error response: error code -32603: Revm error: execution stopped on a node-local dependency failure: exact parent mismatch: required ExactParentIdentity { ",
        )).or_else(|| error.strip_prefix(
            "eth_call failed: server returned an error response: error code -32603: Revm error: fatal: fatal: compressed-entity tree corruption: exact parent mismatch: required ExactParentIdentity { ",
        ))?;
        let (required, marker) = detail.split_once(" }, marker FinalizedMarker { ")?;
        let required = fields(
            required,
            &[
                "commitment_scheme_version",
                "block_number",
                "block_hash",
                "root",
            ],
        )?;
        let marker = fields(
            marker.strip_suffix(" }")?,
            &[
                "commitment_scheme_version",
                "height",
                "block_hash",
                "parent_block_hash",
                "parent_root",
                "new_root",
            ],
        )?;
        let required_scheme = required[0].parse::<u32>().ok()?;
        let marker_scheme = marker[0].parse::<u32>().ok()?;
        let required_height = required[1].parse::<u64>().ok()?;
        let marker_height = marker[1].parse::<u64>().ok()?;
        let required_hash = required[2].parse::<alloy_primitives::B256>().ok()?;
        let required_root = required[3].parse::<alloy_primitives::B256>().ok()?;
        let marker_hash = marker[2].parse::<alloy_primitives::B256>().ok()?;
        let parent_hash = marker[3].parse::<alloy_primitives::B256>().ok()?;
        let parent_root = marker[4].parse::<alloy_primitives::B256>().ok()?;
        let new_root = marker[5].parse::<alloy_primitives::B256>().ok()?;
        let projection_behind_head = marker_height < before && before <= after;
        let same_checkpoint = required_height == marker_height && required_hash == marker_hash;
        let same_scheme = required_scheme == outbe_compressed_entities::ACTIVE_COMMITMENT_SCHEME
            && required_scheme == marker_scheme;
        if same_scheme && same_checkpoint && projection_behind_head {
            return Some(());
        }
        let height_range =
            required_height <= before && before <= marker_height && marker_height == after;
        let next_marker = required_height.checked_add(1) == Some(marker_height);
        let same_parent = parent_hash == required_hash && marker_hash != required_hash;
        let unchanged_root = parent_root == required_root && new_root == required_root;
        if !(height_range && same_scheme && next_marker) {
            return None;
        }
        (same_parent && unchanged_root).then_some(())
    })();
    parsed.is_some()
}

// None requests a fresh complete observation. It never means an absent owner.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn projection_read_decision<T>(
    sample: ProjectionSample<T>,
) -> Result<Option<T>, String> {
    match sample.observation {
        Err(error) => {
            if ce_projection_not_ready(&error, sample.before, sample.after) {
                Ok(None)
            } else {
                Err(error)
            }
        }
        Ok(None) if sample.after < sample.before => {
            Err("owner observation head regressed".to_owned())
        }
        Ok(None) => Ok(None),
        Ok(Some(value)) => {
            if sample.after < sample.before {
                Err("owner observation head regressed".to_owned())
            } else if sample.after > sample.before {
                Ok(None)
            } else {
                Ok(Some(value))
            }
        }
    }
}

#[cfg(feature = "ocomp-integration")]
pub(crate) fn observe_projection<T>(
    deadline: std::time::Instant,
    mut sample: impl FnMut() -> eyre::Result<ProjectionSample<T>>,
    mut now: impl FnMut() -> std::time::Instant,
    mut wait: impl FnMut(),
) -> eyre::Result<T> {
    let mut last = "no owner observation".to_owned();
    loop {
        ensure!(
            now() < deadline,
            "owner observation deadline exhausted: {last}"
        );
        let observed = sample()?;
        let outcome = match &observed.observation {
            Ok(Some(_)) => "present",
            Ok(None) => "missing materialized owner",
            Err(error) => error.as_str(),
        };
        last = format!(
            "before={} after={} result={outcome}",
            observed.before, observed.after,
        );
        ensure!(
            now() < deadline,
            "owner observation deadline exhausted: {last}"
        );
        if let Some(value) =
            projection_read_decision(observed).map_err(|error| eyre!("{error}; {last}"))?
        {
            return Ok(value);
        }
        eprintln!("snapshot_owner_observation_retry {last}");
        wait();
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
