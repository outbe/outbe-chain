//! Cold admission of a joiner: certified follower catch-up before authority.

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use eyre::{ensure, eyre, Result, WrapErr as _};
use outbe_tee::FinalizedJoinAdmissionAnchorV1;

use crate::internal::launch_log::LaunchLog;
use crate::world::localnet::Localnet;
use crate::world::rpc::{FinalizedCheckpoint, Rpc};

const COLD_CATCHUP_TIMEOUT: Duration = Duration::from_secs(240);
const LEASE_GUARD_ARMED: &str = "local TEE lease guard armed at authenticated catch-up anchor";

/// What the certified follower must reach before the joiner's authority starts.
struct ColdCatchup<'a> {
    rpc: &'a Rpc,
    primary: u16,
    port: u16,
    baseline: u64,
    anchor: Option<FinalizedJoinAdmissionAnchorV1>,
    log_path: PathBuf,
}

impl ColdCatchup<'_> {
    /// Pin once: a follower that is consistently a few blocks behind must not
    /// chase a moving head forever.
    fn pinned_target(&self) -> Result<Option<FinalizedCheckpoint>> {
        let upstream = self.rpc.finalized_result(self.primary)?;
        (upstream > self.baseline)
            .then(|| self.rpc.checkpoint_at(self.primary, upstream))
            .transpose()
    }

    /// `None` once certified parity is reached; otherwise the pending observation.
    fn pending(&self, target: Option<FinalizedCheckpoint>, log: &str) -> Result<Option<String>> {
        match (target, self.rpc.finalized_result(self.port)) {
            (Some(expected), Ok(local)) if local >= expected.height => self.parity(expected, log),
            observation => Ok(Some(format!("finalized observations: {observation:?}"))),
        }
    }

    fn parity(&self, expected: FinalizedCheckpoint, log: &str) -> Result<Option<String>> {
        ensure!(
            self.rpc.checkpoint_at(self.port, expected.height)? == expected,
            "cold joiner finalized checkpoint diverged from upstream"
        );
        let Some(anchor) = self.anchor else {
            return Ok(None);
        };
        let observed = self.rpc.checkpoint_at(self.port, anchor.finalized_height)?;
        require_cold_admission_checkpoint(anchor, observed)?;
        Ok((!log.contains(LEASE_GUARD_ARMED))
            .then(|| "finalized parity reached; lease guard not yet armed".to_owned()))
    }
}

impl Localnet {
    /// Cold admission only. Deliberate warm promotion and crash/restart tests
    /// use launch_joiner directly so their fault boundary is not pre-recovered.
    pub fn launch_caught_up_joiner(&mut self, index: usize, extra: &[&str]) -> Result<()> {
        ensure!(
            !self.validators.contains_key(&index),
            "validator-{index} is already owned"
        );
        let name = Self::joiner_full_node_name(index);
        ensure!(
            !self.followers.contains_key(&name),
            "{name} is already owned"
        );
        let argv = self.joiner_validator_args(index, extra)?;
        let vd = self.cfg.validator_dir(index);
        ensure!(
            !self
                .validators
                .values()
                .chain(self.followers.values())
                .any(|child| child.owns_node_data_dir(&vd.join("data"))),
            "cold joiner datadir already has an owned process"
        );
        let anchor = self.cold_admission_anchor(&vd)?;
        let rpc = Rpc::new(self.cfg.clone());
        let primary = self.cfg.http_port(0);
        let port = self.cfg.http_port(index);
        let baseline = rpc.finalized_result(primary)?;
        if let Some(anchor) = anchor {
            verify_upstream_admission(&rpc, primary, baseline, anchor)?;
        }
        let follower_args = super::super::follower::derive_validator_recovery_follower_args(
            &argv,
            &format!("http://127.0.0.1:{primary}"),
        )?;
        let mut log = LaunchLog::arm(&vd.join("node.log"))?;
        self.launch_certified_follower_with_args(&name, index, follower_args)?;
        let catchup = ColdCatchup {
            rpc: &rpc,
            primary,
            port,
            baseline,
            anchor,
            log_path: vd.join("node.log"),
        };
        let result = self.wait_cold_catchup(&name, &mut log, &catchup);
        // Drop/interrupt reaps the sole writer before authority is launched.
        let stopped = self.stop_follower(&name);
        let sealed = log.seal();
        finish_cold_catchup(result, stopped, sealed)?;
        self.spawn_joiner_with_args(index, argv)?;
        rpc.wait_finalized_checkpoint(
            &[primary, port],
            baseline
                .checked_add(1)
                .ok_or_else(|| eyre!("cold joiner finalized height overflow"))?,
            80,
        )?;
        ensure!(
            self.validators
                .get_mut(&index)
                .ok_or_else(|| eyre!("cold joiner authority process is not owned"))?
                .exit_status()?
                .is_none(),
            "cold joiner authority process exited after catch-up"
        );
        Ok(())
    }

    /// This API reconciles the durable journal and takes its owner lock.
    /// Read it exactly once, while the node is stopped, never inside polling.
    fn cold_admission_anchor(&self, vd: &Path) -> Result<Option<FinalizedJoinAdmissionAnchorV1>> {
        if !self.cfg.tee_mode.passes_sgx_devices() {
            return Ok(None);
        }
        outbe_tee::load_finalized_join_admission_anchor(&vd.join("data"))?
            .ok_or_else(|| eyre!("cold joiner has no finalized admission anchor"))
            .map(Some)
    }

    fn wait_cold_catchup(
        &mut self,
        name: &str,
        log: &mut LaunchLog,
        catchup: &ColdCatchup<'_>,
    ) -> Result<()> {
        let deadline = Instant::now() + COLD_CATCHUP_TIMEOUT;
        let mut target = None;
        loop {
            self.live_follower_pid(name)?;
            let text = log.read()?;
            if target.is_none() {
                target = catchup.pinned_target()?;
            }
            let Some(last_observation) = catchup.pending(target, &text)? else {
                return Ok(());
            };
            ensure!(
                Instant::now() < deadline,
                "cold joiner certified catch-up timed out: {last_observation}; see {}",
                catchup.log_path.display()
            );
            sleep(Duration::from_millis(200));
        }
    }
}

fn verify_upstream_admission(
    rpc: &Rpc,
    primary: u16,
    baseline: u64,
    anchor: FinalizedJoinAdmissionAnchorV1,
) -> Result<()> {
    ensure!(
        baseline >= anchor.finalized_height,
        "cold joiner upstream has not finalized its admission anchor"
    );
    ensure!(
        rpc.checkpoint_at(primary, 0)?.block_hash == anchor.genesis_hash,
        "cold joiner admission belongs to another genesis"
    );
    let checkpoint = rpc.checkpoint_at(primary, anchor.finalized_height)?;
    require_cold_admission_checkpoint(anchor, checkpoint)
}

/// Preserve catch-up as the primary failure while also reporting every cleanup
/// failure. Authority can start only after all three operations succeed.
pub(super) fn finish_cold_catchup(
    catchup: Result<()>,
    stop: Result<()>,
    seal: Result<()>,
) -> Result<()> {
    let mut result = catchup;
    for (operation, cleanup) in [("stop follower", stop), ("seal follower log", seal)] {
        if let Err(error) = cleanup {
            let context = format!("{operation} failed: {error:#}");
            result = match result {
                Ok(()) => Err(error).wrap_err(operation),
                Err(primary) => Err(primary).wrap_err(context),
            };
        }
    }
    result
}

pub(super) fn require_cold_admission_checkpoint(
    anchor: FinalizedJoinAdmissionAnchorV1,
    checkpoint: FinalizedCheckpoint,
) -> Result<()> {
    eyre::ensure!(
        checkpoint.height == anchor.finalized_height
            && checkpoint.block_hash == anchor.finalized_hash
            && checkpoint.state_root == anchor.finalized_state_root,
        "cold joiner disagrees with its durable admission checkpoint"
    );
    Ok(())
}
