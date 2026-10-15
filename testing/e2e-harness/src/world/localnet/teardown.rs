//! Owned-process shutdown, scoped cleanup backstops, and scenario data removal.

use std::fs;
use std::path::{Path, PathBuf};

use eyre::{Result, WrapErr};

use super::Localnet;
use crate::internal::proc::ChildGuard;
use crate::world::state::DkgExpiryExpectedExit;

impl Localnet {
    pub(super) fn clear_owned_nodes(&mut self) {
        self.validators.clear();
        self.followers.clear();
        self.follower_startup_probes.clear();
        self.node_launch_logs.clear();
        #[cfg(feature = "ocomp-integration")]
        self.node_launch_recipes.clear();
    }

    /// Drop the owned node handles (killing nodes + `docker rm -f`ing enclaves),
    /// then run a stateless backstop sweep. Its primary role is the SIGINT path,
    /// where the `World` is never dropped so the guards never fire (plus
    /// intra-run belt-and-suspenders between scenarios). It is scoped to this
    /// run's unique data subdir + enclave run tag, so it never touches another
    /// run's nodes/containers.
    pub(super) fn shutdown(&mut self) -> Result<()> {
        self.shutdown_with_expected_exits(&[], &[])
    }

    pub(super) fn stop_owned_nodes(
        &mut self,
        expected_failed_slots: &[usize],
        expected_dkg_expiry_exits: &[DkgExpiryExpectedExit],
    ) -> Result<()> {
        let mut failures = Vec::new();
        // Check every retained slot, including missing handles that the stop
        // loop cannot visit. A replacement launch cannot inherit this proof.
        for proof in expected_dkg_expiry_exits {
            let owned_pid = self.validators.get(&proof.slot).map(ChildGuard::pid);
            let launch_pid = self.node_launch_logs.get(&proof.slot).map(|(pid, _)| *pid);
            if proof.node_pid == 0
                || owned_pid != Some(proof.node_pid)
                || launch_pid != Some(proof.node_pid)
            {
                failures.push(format!(
                    "DKG expiry slot {} expected PID {}, owned {owned_pid:?}, launch {launch_pid:?}",
                    proof.slot, proof.node_pid
                ));
            }
        }
        for (slot, child) in self
            .validators
            .iter_mut()
            .map(|(slot, child)| (Some(*slot), child))
            .chain(self.followers.values_mut().map(|child| (None, child)))
        {
            let pid = child.pid();
            let expiry = expected_dkg_expiry_exits
                .iter()
                .find(|proof| Some(proof.slot) == slot);
            let expected = self.node_launch_logs.iter().any(|(slot, (owned_pid, _))| {
                *owned_pid == pid && expected_failed_slots.contains(slot)
            });
            let result = (|| -> Result<()> {
                let before = child.exit_status()?;
                let status = child.stop_and_reap()?;
                if let Some(proof) = expiry {
                    eyre::ensure!(
                        proof.node_pid == pid
                            && before.is_some_and(|status| status.code() == Some(1))
                            && status.code() == Some(1),
                        "DKG expiry slot {} PID {pid} did not retain its witnessed natural exit 1 before cleanup: before={before:?}, after={status}",
                        proof.slot
                    );
                    return Ok(());
                }
                // An intentional protocol rejection must have happened before
                // cleanup. The after-hook independently requires its exact
                // cause in both runtime log sinks. This is not a log waiver.
                eyre::ensure!(
                    status.success() || (expected && before.is_some() && status.code() == Some(1)),
                    "owned node PID {pid} exited with {status}"
                );
                Ok(())
            })();
            if let Err(error) = result {
                failures.push(format!("{error:#}"));
            }
        }
        self.clear_owned_nodes();
        eyre::ensure!(
            failures.is_empty(),
            "node teardown failed: {}",
            failures.join("; ")
        );
        Ok(())
    }

    fn shutdown_with_expected_exits(
        &mut self,
        expected_failed_slots: &[usize],
        expected_dkg_expiry_exits: &[DkgExpiryExpectedExit],
    ) -> Result<()> {
        // Stateless signal-path backstop for the harness-owned price feeder.
        // Its config argv is rooted under this run/scenario directory.
        let feeder = format!("outbe-feeder.*{}", self.dir());
        self.sh().sudo_best_effort("pkill", &["-9", "-f", &feeder]);
        // Nodes first (release MDBX locks), then their enclaves - matching the
        // stop-nodes-then-teardown-enclaves ordering `run-testnet.sh` used.
        let node_result = self.stop_owned_nodes(expected_failed_slots, expected_dkg_expiry_exits);
        self.radicle_sidecars.clear();
        self.user_radicle = None;
        self.enclaves.clear();
        // No settle is needed here. Clearing the maps dropped every guard, which
        // synchronously `kill()`s + `wait()`s the owned nodes/enclaves. The sweep
        // below is a fire-and-forget backstop.

        let nodes = format!("outbe-chain node.*{}", self.dir());
        self.sh().sudo_best_effort("pkill", &["-9", "-f", &nodes]);
        let radicle = format!("outbe-radicle.*{}", self.dir());
        self.sh().sudo_best_effort("pkill", &["-9", "-f", &radicle]);
        if self.cfg.tee_mode.runs_native_host_enclave() {
            // Native enclaves are processes, not containers. Their `--tee-dir`
            // argv carries this run's dir, so the pattern is run-scoped exactly
            // like the node sweep above.
            let enclaves = format!("outbe-tee-enclave.*{}", self.dir());
            self.sh()
                .sudo_best_effort("pkill", &["-9", "-f", &enclaves]);
        } else {
            let tee_sweep = format!(
                "docker ps -aq --filter name=outbe-tee-gramine-{}- | xargs -r docker rm -f",
                self.cfg.run_tag
            );
            self.sh().sudo_best_effort("bash", &["-c", &tee_sweep]);
        }
        let runtime_result = self.cleanup_radicle_runtime();
        node_result.and(runtime_result)
    }

    /// Remove `cfg.dir` (this localnet's scenario dir, or the whole run dir when
    /// built from the run-level [`Config`](crate::internal::config::Config)). Already-gone is success.
    ///
    /// `validator-<i>/tee` is the enclave container's only writable mount
    /// (`proc::spawn_enclave`), so under `--sudo` it is the only thing this user
    /// can't unlink. Drop those with `sudo rm` first. Everything else the
    /// harness created itself, and a failure to remove it is a real error.
    pub fn wipe(&self) -> Result<()> {
        self.cleanup_radicle_runtime()?;
        if !self.cfg.dir.exists() {
            return Ok(());
        }
        if self.cfg.sudo {
            for tee in sealed_dirs(&self.cfg.dir) {
                self.sh()
                    .sudo_best_effort("rm", &["-rf", &tee.display().to_string()]);
            }
        }
        fs::remove_dir_all(&self.cfg.dir)
            .wrap_err_with(|| format!("wiping data dir {}", self.dir()))
    }

    /// Post-run teardown: shut down all nodes + enclave containers, leaving the
    /// data dir (logs/chain state) intact for inspection. Invoked from the
    /// cucumber `after` hook (and the SIGINT handler).
    pub fn teardown(&mut self) -> Result<()> {
        self.shutdown()
    }

    /// Expected exit-1 slots still require the after-hook's exact fault audit.
    /// DKG expiry additionally requires its retained slot/PID halt evidence.
    pub(crate) fn teardown_with_expected_exits(
        &mut self,
        slots: &[usize],
        dkg_expiry_exits: &[DkgExpiryExpectedExit],
    ) -> Result<()> {
        self.shutdown_with_expected_exits(slots, dkg_expiry_exits)
    }

    /// Stop the localnet (alias for [`teardown`](Self::teardown)).
    pub fn stop(&mut self) -> Result<()> {
        self.shutdown()
    }
}

/// The sealed-state dirs under `root` - the only paths the (root) enclave
/// container writes. `root` is either a scenario dir (`validator-<i>/tee`) or a
/// run dir (`scenario-<n>/validator-<i>/tee`). This function checks both shapes.
///
/// Deliberately not a recursive walk: `validator-<i>/data` holds the reth MDBX
/// store, and descending into it would cost far more than the two `read_dir`s
/// this needs.
pub(super) fn sealed_dirs(root: &Path) -> Vec<PathBuf> {
    fn push_validator_tee(base: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(base) else {
            return;
        };
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with("validator-") {
                let tee = e.path().join("tee");
                if tee.is_dir() {
                    out.push(tee);
                }
            }
        }
    }

    let mut out = Vec::new();
    push_validator_tee(root, &mut out);
    if let Ok(entries) = fs::read_dir(root) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with("scenario-") {
                push_validator_tee(&e.path(), &mut out);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Environment;
    use crate::internal::{config::Config, proc};
    use std::process::Command;

    #[test]
    fn owned_node_cleanup_rejects_silent_failure_and_checks_expected_slot() {
        for (code, expected_slots, accepted) in [
            (0, vec![], true),
            (1, vec![], false),
            (1, vec![0], true),
            (1, vec![1], false),
            (2, vec![0], false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let env = Environment {
                data_dir: dir.path().canonicalize().unwrap(),
                ..Environment::default()
            };
            let mut localnet = Localnet::new(Config::resolve(&env));
            let node_dir = localnet.cfg.validator_dir(0);
            fs::create_dir_all(&node_dir).unwrap();
            let mut command = Command::new("sh");
            command.args(["-c", &format!("exit {code}")]);
            proc::attach_log(&mut command, &node_dir).unwrap();
            let mut child = localnet
                .spawn_node("exit-fixture", 0, &node_dir, command)
                .unwrap();
            child.reap_fault(std::time::Duration::from_secs(2)).unwrap();
            localnet.validators.insert(0, child);
            assert_eq!(
                localnet.stop_owned_nodes(&expected_slots, &[]).is_ok(),
                accepted,
                "exit {code}, expected {expected_slots:?}"
            );
            assert!(localnet.validators.is_empty());
            assert!(localnet.node_launch_logs.is_empty());
        }
    }

    fn expiry_cleanup_fixture(script: &str, exited: bool) -> (tempfile::TempDir, Localnet, u32) {
        let dir = tempfile::tempdir().unwrap();
        let env = Environment {
            data_dir: dir.path().canonicalize().unwrap(),
            ..Environment::default()
        };
        let mut localnet = Localnet::new(Config::resolve(&env));
        let node_dir = localnet.cfg.validator_dir(0);
        fs::create_dir_all(&node_dir).unwrap();
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        proc::attach_log(&mut command, &node_dir).unwrap();
        // attach_log defaults stdin to null. This fixture must keep `read`
        // blocked until cleanup signals it instead of exiting on immediate EOF.
        command.stdin(std::process::Stdio::piped());
        let mut child = localnet
            .spawn_node("expiry-exit-fixture", 0, &node_dir, command)
            .unwrap();
        if exited {
            child.reap_fault(std::time::Duration::from_secs(2)).unwrap();
        } else {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !localnet
                .node_launch_log(0, child.pid())
                .unwrap()
                .contains("ready\n")
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "live fixture not ready"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(child.exit_status().unwrap().is_none());
        }
        let pid = child.pid();
        localnet.validators.insert(0, child);
        (dir, localnet, pid)
    }

    #[test]
    fn dkg_expiry_cleanup_requires_exact_owned_and_launch_pid() {
        for damage in [
            "none",
            "witness_pid",
            "zero_pid",
            "witness_slot",
            "launch_pid",
            "missing_launch",
            "missing_owner",
            "missing_proof",
        ] {
            let (_dir, mut localnet, pid) = expiry_cleanup_fixture("exit 1", true);
            let mut proofs = vec![DkgExpiryExpectedExit {
                slot: 0,
                node_pid: pid,
            }];
            match damage {
                "none" => {}
                "witness_pid" => proofs[0].node_pid += 1,
                "zero_pid" => proofs[0].node_pid = 0,
                "witness_slot" => proofs[0].slot = 1,
                "launch_pid" => localnet.node_launch_logs.get_mut(&0).unwrap().0 += 1,
                "missing_launch" => {
                    localnet.node_launch_logs.remove(&0);
                }
                "missing_owner" => {
                    localnet.validators.remove(&0);
                }
                "missing_proof" => proofs.clear(),
                _ => unreachable!(),
            }
            assert_eq!(
                localnet.stop_owned_nodes(&[], &proofs).is_ok(),
                damage == "none",
                "accepted damaged DKG expiry evidence: {damage}"
            );
            assert!(localnet.validators.is_empty());
            assert!(localnet.node_launch_logs.is_empty());
        }
    }

    #[test]
    fn dkg_expiry_cleanup_rejects_success_other_errors_signals_and_live_children() {
        for (script, exited, accepted) in [
            ("exit 1", true, true),
            ("exit 0", true, false),
            ("exit 2", true, false),
            ("kill -KILL $$", true, false),
            // Readiness is emitted after installing the trap. Cleanup itself
            // will produce exit 1, which must not count as a natural halt.
            (
                "trap 'exit 1' TERM; printf 'ready\\n'; read unused",
                false,
                false,
            ),
        ] {
            let (_dir, mut localnet, pid) = expiry_cleanup_fixture(script, exited);
            let proof = DkgExpiryExpectedExit {
                slot: 0,
                node_pid: pid,
            };
            // Even an existing slot allowance cannot bypass the exact proof.
            assert_eq!(
                localnet.stop_owned_nodes(&[0], &[proof]).is_ok(),
                accepted,
                "script {script}, exited before cleanup {exited}"
            );
            assert!(localnet.validators.is_empty());
        }
    }
}
