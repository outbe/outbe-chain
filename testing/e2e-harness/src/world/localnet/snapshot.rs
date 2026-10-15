//! Incarnation-bound observations and replay of stopped owned node processes.

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use eyre::Result;

use super::{follower, Localnet, NodeStopObservation};
use crate::internal::proc::{self, ChildGuard};

/// Facts captured from the configured command and this owned launch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NodeLaunchObservation {
    pub(crate) index: usize,
    pub(crate) pid: u32,
    pub(crate) program: PathBuf,
    pub(crate) argv: Vec<String>,
    pub(crate) started_at_millis: u64,
    pub(crate) log_path: PathBuf,
    pub(crate) log_start: u64,
}

#[derive(Clone, Debug)]
pub(super) struct NodeLaunchRecipe {
    pub(super) observation: NodeLaunchObservation,
    pub(super) node_dir: PathBuf,
    pub(super) environment: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)>,
    pub(super) current_dir: Option<PathBuf>,
}

impl NodeLaunchRecipe {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.observation.program);
        command.args(&self.observation.argv);
        for (key, value) in &self.environment {
            match value {
                Some(value) => {
                    command.env(key, value);
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
        if let Some(path) = &self.current_dir {
            command.current_dir(path);
        }
        command
    }
}

/// One stopped owned incarnation, consumed when replaying its own launch.
/// Private recipe/owner fields prevent constructing a donor recipe for a recipient.
#[derive(Debug)]
pub(crate) struct StoppedSnapshotNode {
    pub(crate) observation: NodeStopObservation,
    pub(super) recipe: NodeLaunchRecipe,
    pub(super) follower_name: Option<String>,
}

pub(super) fn process_observed_millis() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

pub(super) fn checked_node_interrupt(
    child: &mut ChildGuard,
    launch: NodeLaunchObservation,
) -> Result<NodeStopObservation> {
    use std::os::unix::process::ExitStatusExt as _;
    eyre::ensure!(child.pid() == launch.pid, "owned node incarnation changed");
    eyre::ensure!(
        child.exit_status()?.is_none(),
        "owned node exited before requested snapshot stop"
    );
    let requested = process_observed_millis()?;
    child.interrupt();
    let status = child
        .exit_status()?
        .ok_or_else(|| eyre::eyre!("owned node remained live after SIGINT stop"))?;
    // A default signal exit or fallback SIGKILL is not a successfully closed node.
    eyre::ensure!(
        status.success(),
        "owned node PID {} failed during SIGINT stop: {status}",
        child.pid()
    );
    Ok(NodeStopObservation {
        launch,
        stop_requested_at_millis: requested,
        reaped_at_millis: process_observed_millis()?,
        code: status.code(),
        signal: status.signal(),
    })
}

impl Localnet {
    pub(super) fn captured_node_recipe(
        &self,
        index: usize,
        expected_pid: u32,
    ) -> Result<NodeLaunchRecipe> {
        let recipe = self
            .node_launch_recipes
            .get(&index)
            .ok_or_else(|| eyre::eyre!("node-{index} has no captured launch recipe"))?;
        eyre::ensure!(
            recipe.observation.pid == expected_pid,
            "node-{index} captured launch PID mismatch"
        );
        Ok(recipe.clone())
    }

    pub(crate) fn validator_launch_observation(
        &mut self,
        index: usize,
    ) -> Result<NodeLaunchObservation> {
        let child = self
            .validators
            .get_mut(&index)
            .ok_or_else(|| eyre::eyre!("validator-{index} has no owned node"))?;
        eyre::ensure!(
            child.exit_status()?.is_none(),
            "validator-{index} node exited"
        );
        let pid = child.pid();
        Ok(self.captured_node_recipe(index, pid)?.observation)
    }

    pub(crate) fn stop_validator_for_snapshot(
        &mut self,
        index: usize,
        expected_pid: u32,
    ) -> Result<StoppedSnapshotNode> {
        let observed = self.validator_launch_observation(index)?;
        eyre::ensure!(
            observed.pid == expected_pid,
            "validator-{index} stop PID mismatch"
        );
        let recipe = self.captured_node_recipe(index, expected_pid)?;
        let observation = checked_node_interrupt(
            self.validators.get_mut(&index).expect("checked owned node"),
            observed,
        )?;
        self.validators.remove(&index);
        if let Some((pid, log)) = self.node_launch_logs.get_mut(&index) {
            eyre::ensure!(*pid == expected_pid, "node stop log incarnation changed");
            log.seal()?;
        }
        Ok(StoppedSnapshotNode {
            observation,
            recipe,
            follower_name: None,
        })
    }

    /// Replay this stopped process's actual command. The replay reuses native
    /// stores, keys, enclave, arguments and explicit environment. No provisioning
    /// runs here.
    pub(crate) fn resume_snapshot_node(
        &mut self,
        stopped: StoppedSnapshotNode,
    ) -> Result<NodeLaunchObservation> {
        let index = stopped.observation.launch.index;
        let node_dir = self.cfg.validator_dir(index);
        eyre::ensure!(
            stopped.recipe.node_dir == node_dir,
            "stopped recipe belongs to another node directory"
        );
        eyre::ensure!(
            !self.validators.contains_key(&index),
            "node index still has an owned validator"
        );
        eyre::ensure!(
            !self
                .validators
                .values()
                .chain(self.followers.values())
                .any(|child| child.owns_node_data_dir(&node_dir.join("data"))),
            "node datadir still has an owned writer"
        );
        if let Some(name) = &stopped.follower_name {
            eyre::ensure!(
                !self.followers.contains_key(name),
                "follower name still has an owned node"
            );
            follower::ensure_validator_recovery_follower_args(&stopped.recipe.observation.argv)?;
        }
        let label = stopped
            .follower_name
            .clone()
            .unwrap_or_else(|| format!("validator-{index}"));
        let mut command = stopped.recipe.command();
        proc::attach_log(&mut command, &node_dir)?;
        let guard = self.spawn_node(&label, index, &node_dir, command)?;
        let pid = guard.pid();
        if let Some(name) = stopped.follower_name {
            self.followers.insert(name, guard);
        } else {
            self.validators.insert(index, guard);
        }
        // Capture actual final argv again. Projection option handling is idempotent.
        let observed = self.captured_node_recipe(index, pid)?.observation;
        eyre::ensure!(
            observed.argv == stopped.recipe.observation.argv
                && observed.program == stopped.recipe.observation.program,
            "ordinary restart changed actual launch options"
        );
        Ok(observed)
    }
}

#[cfg(test)]
mod snapshot_process_tests {
    use super::*;
    use crate::env::Environment;
    use crate::internal::config::Config;
    use std::fs;
    use std::time::{Duration, Instant};

    fn fixture() -> (tempfile::TempDir, Localnet) {
        let dir = tempfile::tempdir().unwrap();
        let env = Environment {
            data_dir: dir.path().canonicalize().unwrap(),
            ..Environment::default()
        };
        (dir, Localnet::new(Config::resolve(&env)))
    }
    fn launch(net: &mut Localnet, index: usize, follower: bool, script: &str) -> u32 {
        let node_dir = net.cfg.validator_dir(index);
        fs::create_dir_all(node_dir.join("data")).unwrap();
        let mut command = Command::new("sh");
        command
            .args(["-c", script, "owned-node-fixture"])
            .arg("--datadir")
            .arg(node_dir.join("data"))
            .args([
                "--upstream",
                "http://127.0.0.1:12345",
                "--custom=value with spaces",
                "--custom",
                "second value",
            ])
            .env("OUTBE_FIXTURE_RECIPE", "preserved value")
            .current_dir(&node_dir);
        proc::attach_log(&mut command, &node_dir).unwrap();
        let child = net
            .spawn_node("owned-node-fixture", index, &node_dir, command)
            .unwrap();
        let pid = child.pid();
        if follower {
            net.followers.insert(format!("recipient-{index}"), child);
        } else {
            net.validators.insert(index, child);
        }
        pid
    }
    fn ready(net: &mut Localnet, index: usize, pid: u32) -> String {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let text = net.node_launch_log(index, pid).unwrap();
            if text.contains("fixture-ready") {
                return text;
            }
            assert!(Instant::now() < deadline, "fixture did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    const LIVE: &str = "trap 'exit 0' INT; printf '%s\\n' \"$OUTBE_FIXTURE_RECIPE\" \"$PWD\" \"$@\" fixture-ready; while :; do sleep 1; done";

    #[test]
    fn snapshot_stop_requires_owned_live_incarnation_and_does_not_stop_peer() {
        for follower in [false, true] {
            let (_dir, mut net) = fixture();
            assert!(net.stop_validator_for_snapshot(0, 123).is_err());
            assert!(net.stop_follower_for_snapshot("missing", 0, 123).is_err());
            let pid = launch(&mut net, 0, follower, LIVE);
            let peer = launch(&mut net, 1, false, LIVE);
            ready(&mut net, 0, pid);
            ready(&mut net, 1, peer);
            let wrong = if follower {
                net.stop_follower_for_snapshot("recipient-0", 0, pid + 1)
            } else {
                net.stop_validator_for_snapshot(0, pid + 1)
            };
            assert!(wrong.is_err());
            let stopped = if follower {
                net.stop_follower_for_snapshot("recipient-0", 0, pid)
            } else {
                net.stop_validator_for_snapshot(0, pid)
            }
            .unwrap();
            assert_eq!(stopped.observation.launch.pid, pid);
            assert_eq!(stopped.observation.code, Some(0));
            assert_eq!(stopped.observation.signal, None);
            assert!(
                stopped.observation.reaped_at_millis
                    >= stopped.observation.stop_requested_at_millis
            );
            assert_eq!(net.validator_launch_observation(1).unwrap().pid, peer);
            assert!(net
                .validators
                .get_mut(&1)
                .unwrap()
                .exit_status()
                .unwrap()
                .is_none());
        }
    }

    #[test]
    fn snapshot_stop_rejects_already_exited_owner_even_with_success_status() {
        for follower in [false, true] {
            for code in [0, 7] {
                let (_dir, mut net) = fixture();
                let pid = launch(&mut net, 0, follower, &format!("exit {code}"));
                let child = if follower {
                    net.followers.get_mut("recipient-0").unwrap()
                } else {
                    net.validators.get_mut(&0).unwrap()
                };
                child.reap_fault(Duration::from_secs(3)).unwrap();
                let result = if follower {
                    net.stop_follower_for_snapshot("recipient-0", 0, pid)
                } else {
                    net.stop_validator_for_snapshot(0, pid)
                };
                assert!(
                    result.is_err(),
                    "earlier exit must not become an offline stop witness"
                );
            }
        }
    }

    #[test]
    fn snapshot_ordinary_restart_preserves_actual_final_options_environment_and_data() {
        for follower in [false, true] {
            let (_dir, mut net) = fixture();
            let pid = launch(&mut net, 0, follower, LIVE);
            let first_text = ready(&mut net, 0, pid);
            let sentinel = net.cfg.validator_dir(0).join("data/resident-sentinel");
            fs::write(&sentinel, "resident data").unwrap();
            let before = if follower {
                net.follower_launch_observation("recipient-0", 0)
            } else {
                net.validator_launch_observation(0)
            }
            .unwrap();
            assert_eq!(
                before
                    .argv
                    .iter()
                    .filter(|a| *a == "--projection.storage-config")
                    .count(),
                1
            );
            assert!(before
                .argv
                .contains(&"--custom=value with spaces".to_owned()));
            let stopped = if follower {
                net.stop_follower_for_snapshot("recipient-0", 0, pid)
            } else {
                net.stop_validator_for_snapshot(0, pid)
            }
            .unwrap();
            let restarted = net.resume_snapshot_node(stopped).unwrap();
            let second_text = ready(&mut net, 0, restarted.pid);
            assert_eq!(restarted.argv, before.argv);
            assert_eq!(restarted.program, before.program);
            assert_eq!(
                first_text, second_text,
                "same actual child args/cwd/explicit env"
            );
            assert_eq!(fs::read_to_string(&sentinel).unwrap(), "resident data");
            let _ = if follower {
                net.stop_follower_for_snapshot("recipient-0", 0, restarted.pid)
            } else {
                net.stop_validator_for_snapshot(0, restarted.pid)
            }
            .unwrap();
        }
    }
}
