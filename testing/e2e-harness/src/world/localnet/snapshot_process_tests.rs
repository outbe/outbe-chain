use super::*;
use crate::env::Environment;
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
            stopped.observation.reaped_at_millis >= stopped.observation.stop_requested_at_millis
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
