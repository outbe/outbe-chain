//! Waits for complete owner reads while the native checkpoint is stable.

pub(crate) fn resolve_live_state_read<T>(
    observation: Result<T, String>,
    head_changed: bool,
) -> Result<Option<T>, String> {
    observation.map(|value| (!head_changed).then_some(value))
}

use eyre::{ensure, eyre};

pub(crate) struct StateSample<T> {
    pub(crate) before: u64,
    pub(crate) observation: Result<Option<T>, String>,
    pub(crate) after: u64,
}

// None requests a fresh complete observation. It never means an absent owner.

pub(crate) fn state_read_decision<T>(sample: StateSample<T>) -> Result<Option<T>, String> {
    match sample.observation {
        Err(error) => Err(error),
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

pub(crate) fn observe_state<T>(
    deadline: std::time::Instant,
    mut sample: impl FnMut() -> eyre::Result<StateSample<T>>,
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
            state_read_decision(observed).map_err(|error| eyre!("{error}; {last}"))?
        {
            return Ok(value);
        }
        eprintln!("state_owner_observation_retry {last}");
        wait();
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_live_state_read;

    #[test]
    fn accepts_only_successful_reads_at_a_stable_checkpoint() {
        assert_eq!(resolve_live_state_read(Ok(42), false), Ok(Some(42)));
        assert_eq!(resolve_live_state_read(Ok(42), true), Ok(None));
        for changed in [false, true] {
            assert_eq!(
                resolve_live_state_read::<u64>(Err("invalid ciphertext".into()), changed),
                Err("invalid ciphertext".into())
            );
        }
    }
}
