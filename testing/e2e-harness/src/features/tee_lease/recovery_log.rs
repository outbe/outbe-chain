//! Recovery and DKG activation evidence read from owned process logs.

use super::*;

pub(super) fn recovery_follower_has_post_start_progress(
    baseline: u64,
    current: Option<u64>,
) -> bool {
    current.is_some_and(|height| height > baseline)
}

pub(super) fn validate_interrupted_recovery_log(log: &str) -> Result<()> {
    ensure!(
        log.contains(FOLLOWER_ENGINE_STARTED_MARKER),
        "interrupted recovery follower never started"
    );
    ensure!(
        !log.contains("local TEE lease guard armed at authenticated catch-up anchor"),
        "recovery completed admission before interruption; interrupted catch-up was not exercised"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecoveryFollowerStartupProbeV1 {
    Ready,
    Waiting,
    Exited,
}

pub(super) fn classify_recovery_follower_startup(
    running: bool,
    post_start_log: &str,
) -> RecoveryFollowerStartupProbeV1 {
    if !running {
        RecoveryFollowerStartupProbeV1::Exited
    } else if post_start_log.contains(FOLLOWER_ENGINE_STARTED_MARKER) {
        RecoveryFollowerStartupProbeV1::Ready
    } else {
        RecoveryFollowerStartupProbeV1::Waiting
    }
}

pub(super) fn recovery_log_tail(log: &str, max_lines: usize) -> String {
    let mut lines: Vec<_> = log.lines().rev().take(max_lines).collect();
    lines.reverse();
    lines.join("\n")
}

pub(super) fn wait_for_recovery_follower_startup(
    world: &mut World,
    follower_name: &str,
    log: &mut LeaseLogCapture,
    attempts: u32,
    label: &str,
) {
    for _ in 0..attempts {
        let post_start_log = log.read();
        let running = world.localnet.follower_running(follower_name);
        match classify_recovery_follower_startup(running, &post_start_log) {
            RecoveryFollowerStartupProbeV1::Ready => return,
            RecoveryFollowerStartupProbeV1::Waiting => sleep(Duration::from_secs(2)),
            RecoveryFollowerStartupProbeV1::Exited => panic!(
                "{label} exited before `{FOLLOWER_ENGINE_STARTED_MARKER}`; post-offset log tail:\n{}",
                recovery_log_tail(&post_start_log, 80)
            ),
        }
    }

    let post_start_log = log.read();
    let running = world.localnet.follower_running(follower_name);
    match classify_recovery_follower_startup(running, &post_start_log) {
        RecoveryFollowerStartupProbeV1::Ready => {}
        RecoveryFollowerStartupProbeV1::Exited => panic!(
            "{label} exited before `{FOLLOWER_ENGINE_STARTED_MARKER}`; post-offset log tail:\n{}",
            recovery_log_tail(&post_start_log, 80)
        ),
        RecoveryFollowerStartupProbeV1::Waiting => panic!(
            "timed out waiting for {label}; running={running}; post-offset log tail:\n{}",
            recovery_log_tail(&post_start_log, 80)
        ),
    }
}

pub(super) fn requires_certified_follower_recovery(evidence: &str) -> bool {
    CERTIFIED_FOLLOWER_RECOVERY_MARKERS
        .iter()
        .all(|marker| evidence.contains(marker))
}

pub(super) fn pre_readiness_recovery_is_authority_free(
    validator_status: u8,
    has_bls_share: bool,
    post_restart_log: &str,
) -> bool {
    validator_status == STATUS_PENDING
        && !has_bls_share
        && SHARELESS_MARKERS
            .iter()
            .all(|marker| has_info_message(post_restart_log, "outbe_engine::stack", marker))
        && AUTHORITY_MARKERS
            .iter()
            .all(|marker| !post_restart_log.contains(marker))
}

/// DKG material versions before the restart, on the primary and on the restarted node.
#[derive(Clone, Copy)]
pub(super) struct MaterialVersions {
    pub previous: u64,
    pub primary: u64,
    pub restarted: u64,
}

pub(super) fn fresh_dkg_activation_is_complete(
    validator_status: u8,
    has_bls_share: bool,
    versions: MaterialVersions,
    post_restart_log: &str,
) -> bool {
    let active = validator_status == STATUS_ACTIVE && has_bls_share;
    let advanced = versions.primary > versions.previous && versions.restarted == versions.primary;
    active && advanced && activation_record(post_restart_log, versions.restarted).is_some()
}

pub(super) fn info_message<'a>(line: &'a str, target: &str) -> Option<&'a str> {
    let (prefix, message) = line.split_once(&format!("{target}: "))?;
    (prefix.split_whitespace().last() == Some("INFO")).then_some(message)
}

pub(super) fn has_info_message(log: &str, target: &str, message: &str) -> bool {
    log.lines().any(|line| {
        info_message(line, target).is_some_and(|body| {
            body == message
                || body
                    .strip_prefix(message)
                    .is_some_and(|tail| tail.starts_with(' '))
        })
    })
}

pub(super) fn log_u64(line: &str, key: &str) -> Option<u64> {
    let prefix = format!("{key}=");
    let mut values = line
        .split_whitespace()
        .filter_map(|word| word.strip_prefix(&prefix));
    let value = values.next()?.parse().ok()?;
    values.next().is_none().then_some(value)
}

/// Completion must precede activation within the post-readiness process interval.
/// Public material alone never supplies the local-private-share assertion.
pub(super) fn activation_record(log: &str, version: u64) -> Option<(u64, u64)> {
    let mut completed = false;
    for line in log.lines() {
        if info_message(
            line,
            "outbe_consensus::dkg_actor::actor::ceremony::completion",
        ) == Some(DKG_COMPLETE_MARKER)
        {
            completed = true;
        }
        if completed
            && info_message(line, "outbe_engine::stack")
                .is_some_and(|body| body.starts_with("VRF/DKG material activated "))
            && log_u64(line, "vrf_material_version") == Some(version)
        {
            return Some((
                log_u64(line, "dkg_cycle")?,
                log_u64(line, "activation_height")?,
            ));
        }
    }
    None
}
