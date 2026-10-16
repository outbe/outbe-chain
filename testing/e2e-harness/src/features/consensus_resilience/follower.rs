//! FullNode follower resilience against upstream faults, outages and restarts.

use std::thread::sleep;
use std::time::Duration;

use cucumber::{then, when};

use super::upstream::ControlledUpstream;
use super::{FOLLOWER_RATE_LIMIT_NAME, FOLLOWER_RESILIENCE_NAME, FOLLOWER_RESILIENCE_SLOT};
use crate::world::World;

#[when("a production FullNode survives transient HTTP 429 from its unchanged upstream")]
fn follower_survives_upstream_rate_limit(world: &mut World) {
    let slot = FOLLOWER_RESILIENCE_SLOT;
    world
        .localnet
        .provision_full_node_node_host(slot)
        .expect("provision production FullNode NodeHost");
    let proxy = ControlledUpstream::start(world.validators.primary_port())
        .expect("start scenario-owned upstream proxy");
    world
        .localnet
        .launch_full_node_with_upstream_url(FOLLOWER_RATE_LIMIT_NAME, slot, 0, &proxy.url())
        .expect("launch certified FullNode through upstream proxy");
    let follower_port = world.validators.http_port(slot);
    let primary = world.validators.primary_port();
    let initial = world
        .rpc
        .finalized(primary)
        .expect("initial committee finality");
    assert!(
        world
            .rpc
            .wait_finalized_at_least(follower_port, initial, 90),
        "FullNode did not catch up before rate limit"
    );
    let pid = world
        .localnet
        .live_follower_pid(FOLLOWER_RATE_LIMIT_NAME)
        .expect("original FullNode PID");
    let requests_before = proxy.tip_requests();
    proxy.rate_limit_tips(true);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while proxy.rejected_tips() < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "FullNode did not query the rate-limited consensus status"
        );
        assert_eq!(
            world
                .localnet
                .live_follower_pid(FOLLOWER_RATE_LIMIT_NAME)
                .expect("FullNode during HTTP 429"),
            pid
        );
        sleep(Duration::from_millis(250));
    }
    let target = world
        .rpc
        .finalized(primary)
        .expect("committee finality during 429")
        + 3;
    assert!(
        world.rpc.wait_finalized_at_least(primary, target, 90),
        "committee did not advance during HTTP 429"
    );
    proxy.rate_limit_tips(false);
    let expected = world
        .rpc
        .checkpoint_at(primary, target)
        .expect("pin finalized recovery checkpoint");
    assert!(
        world.rpc.wait_finalized_at_least(follower_port, target, 90),
        "unchanged FullNode did not recover after HTTP 429"
    );
    let observed = world
        .rpc
        .checkpoint_at(follower_port, target)
        .expect("FullNode recovery checkpoint");
    assert_eq!(observed.block_hash, expected.block_hash);
    assert_eq!(observed.state_root, expected.state_root);
    assert_eq!(
        world
            .localnet
            .live_follower_pid(FOLLOWER_RATE_LIMIT_NAME)
            .expect("FullNode after HTTP 429"),
        pid
    );
    assert!(proxy.tip_requests() > requests_before);
    let evidence = serde_json::json!({
        "full_node_pid": pid, "upstream_url": proxy.url(),
        "tip_requests": proxy.tip_requests(), "http_429_responses": proxy.rejected_tips(),
        "recovery_checkpoint": {
            "height": expected.height,
            "block_hash": expected.block_hash,
            "state_root": expected.state_root,
        },
    });
    let path = world.localnet.scenario_dir().join("follower-http-429.json");
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&evidence).expect("serialize 429 evidence"),
    )
    .expect("write 429 evidence");
    world
        .localnet
        .stop_follower(FOLLOWER_RATE_LIMIT_NAME)
        .expect("stop probe FullNode");
}

#[when("a production FullNode receives a forged finalized proof from its upstream")]
fn follower_rejects_forged_finalization(world: &mut World) {
    const NAME: &str = "follower-forged-finalization";
    let slot = FOLLOWER_RESILIENCE_SLOT;
    world
        .localnet
        .provision_full_node_node_host(slot)
        .expect("provision production FullNode NodeHost");
    let primary = world.validators.primary_port();
    let proxy = ControlledUpstream::start(primary).expect("start owned proof proxy");
    world
        .localnet
        .launch_full_node_with_upstream_url(NAME, slot, 0, &proxy.url())
        .expect("launch certified FullNode through proof proxy");
    let follower_port = world.validators.http_port(slot);
    let initial = world
        .rpc
        .finalized(primary)
        .expect("initial committee finality");
    assert!(
        world
            .rpc
            .wait_finalized_at_least(follower_port, initial, 90),
        "FullNode did not synchronize before forged proof"
    );
    let pid = world
        .localnet
        .live_follower_pid(NAME)
        .expect("original FullNode process");
    let forged_floor = world
        .rpc
        .finalized(follower_port)
        .expect("FullNode finality before fault")
        + 1;
    proxy.forge_finalizations_from(forged_floor);
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    while proxy.forged_finalizations() < 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "FullNode never received a forged finalized proof (finalization requests: {}, forged: {})",
            proxy.finalization_requests(),
            proxy.forged_finalizations()
        );
        assert_eq!(
            world
                .localnet
                .live_follower_pid(NAME)
                .expect("FullNode during forged proof"),
            pid
        );
        sleep(Duration::from_millis(250));
    }
    let forged_height = proxy
        .first_forged_height()
        .expect("first forged proof height");
    assert!(forged_height >= forged_floor);
    sleep(Duration::from_secs(2));
    let during = world
        .rpc
        .finalized(follower_port)
        .expect("FullNode RPC after forged proof");
    assert!(
        during < forged_height,
        "FullNode accepted forged proof at h{forged_height}: finalized h{during}"
    );
    proxy.restore_finalizations();
    let target = forged_height + 2;
    assert!(
        world.rpc.wait_finalized_at_least(primary, target, 90),
        "committee did not advance past forged proof height"
    );
    let expected = world
        .rpc
        .checkpoint_at(primary, target)
        .expect("pin committee checkpoint after forged proof");
    assert!(
        world.rpc.wait_finalized_at_least(follower_port, target, 90),
        "FullNode did not recover after authentic proofs returned"
    );
    let observed = world
        .rpc
        .checkpoint_at(follower_port, target)
        .expect("FullNode checkpoint after recovery");
    assert_eq!(observed.block_hash, expected.block_hash);
    assert_eq!(observed.state_root, expected.state_root);
    assert_eq!(
        world
            .localnet
            .live_follower_pid(NAME)
            .expect("unchanged FullNode process"),
        pid
    );
    let path = world
        .localnet
        .scenario_dir()
        .join("follower-forged-finalization.json");
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "full_node_pid": pid, "proxy_url": proxy.url(),
            "forged_proof_count": proxy.forged_finalizations(),
            "forged_height": forged_height, "finalized_during_forgery": during,
            "recovery_height": target, "recovery_hash": expected.block_hash,
            "recovery_state_root": expected.state_root,
        }))
        .expect("serialize forged proof evidence"),
    )
    .expect("write forged proof evidence");
    world
        .localnet
        .stop_follower(NAME)
        .expect("stop probe FullNode");
}

#[when("a production FullNode syncs from the committee with bounded follower resolution")]
fn sync_follower_with_bounded_resolution(world: &mut World) {
    world
        .localnet
        .provision_full_node_node_host(FOLLOWER_RESILIENCE_SLOT)
        .expect("provision production FullNode NodeHost and enclave");
    world
        .localnet
        .launch_dcap_full_node(FOLLOWER_RESILIENCE_NAME, FOLLOWER_RESILIENCE_SLOT, 0)
        .expect("launch production FullNode follower");

    let committee_port = world.validators.primary_port();
    let follower_port = world.validators.http_port(FOLLOWER_RESILIENCE_SLOT);

    let mut synced = false;
    for _ in 0..40 {
        if let (Some(head), Some(follower_head)) = (
            world.rpc.head(committee_port),
            world.rpc.head(follower_port),
        ) {
            if follower_head + 2 >= head {
                synced = true;
                break;
            }
        }
        sleep(Duration::from_secs(1));
    }
    assert!(synced, "follower did not sync to committee head");
    world.state.resilience_follower_pid = Some(
        world
            .localnet
            .live_follower_pid(FOLLOWER_RESILIENCE_NAME)
            .expect("record original FullNode process"),
    );
}

#[when("the follower upstream is stopped while committee consensus advances")]
fn stop_follower_upstream(world: &mut World) {
    let follower_port = world.validators.http_port(FOLLOWER_RESILIENCE_SLOT);
    let pre_height = world
        .rpc
        .finalized(follower_port)
        .expect("follower must report active finalized height before fault");
    world.state.resilience_disconnected_height = Some(pre_height);

    // Stop upstream validator 0
    world
        .localnet
        .kill_validator(0)
        .expect("stop configured upstream validator-0");

    // Surviving quorum (1, 2, 3) advances
    let survivor_ports = vec![
        world.validators.http_port(1),
        world.validators.http_port(2),
        world.validators.http_port(3),
    ];
    let advanced = world
        .rpc
        .wait_finalized_checkpoint(&survivor_ports, pre_height + 2, 60)
        .expect("surviving committee advances past stopped upstream");
    world.state.resilience_observed_height = Some(advanced.height);
}

#[then("the disconnected follower remains responsive while its upstream is offline")]
fn follower_remains_responsive(world: &mut World) {
    let follower_port = world.validators.http_port(FOLLOWER_RESILIENCE_SLOT);

    // Verify follower process is still alive and has not crashed
    world
        .localnet
        .live_follower_pid(FOLLOWER_RESILIENCE_NAME)
        .expect("follower process must remain alive without crashing");

    let pre_height = world
        .state
        .resilience_disconnected_height
        .expect("pre-fault height must be recorded");

    // Follower should not advance indefinitely while its only configured upstream is dead
    sleep(Duration::from_secs(2));
    let post_height = world
        .rpc
        .finalized(follower_port)
        .expect("follower RPC must remain responsive");
    let survivor_height = world
        .state
        .resilience_observed_height
        .expect("survivor height must be recorded");
    assert!(
        post_height <= survivor_height,
        "disconnected follower advanced beyond network state: {post_height} > {survivor_height}"
    );
    let _ = pre_height;
}

#[when("the follower is restarted in place and switched to an active healthy upstream")]
fn restart_follower_and_switch_upstream(world: &mut World) {
    // Restart validator 0 first so the committee is fully restored
    world
        .localnet
        .restart_validator(0)
        .expect("restart validator-0");

    // Stop the follower and switch its upstream to validator 1
    world
        .localnet
        .stop_follower(FOLLOWER_RESILIENCE_NAME)
        .expect("stop follower");
    world
        .localnet
        .launch_dcap_full_node(FOLLOWER_RESILIENCE_NAME, FOLLOWER_RESILIENCE_SLOT, 1)
        .expect("launch follower pointing to validator-1");
}

#[when("the original upstream restarts and the FullNode remains the same process")]
fn restart_original_upstream_without_follower_restart(world: &mut World) {
    let original_pid = world
        .state
        .resilience_follower_pid
        .expect("original FullNode PID");
    assert_eq!(
        world
            .localnet
            .live_follower_pid(FOLLOWER_RESILIENCE_NAME)
            .expect("FullNode remains alive during upstream outage"),
        original_pid
    );
    world
        .localnet
        .restart_validator(0)
        .expect("restart original upstream validator");
    let ports = world.validators.committee_ports();
    let outage_height = world
        .state
        .resilience_observed_height
        .expect("survivor checkpoint");
    let recovered = world
        .rpc
        .wait_finalized_checkpoint(&ports, outage_height + 2, 90)
        .expect("original upstream rejoins canonical committee");
    world.state.resilience_observed_height = Some(recovered.height);
    assert_eq!(
        world
            .localnet
            .live_follower_pid(FOLLOWER_RESILIENCE_NAME)
            .expect("FullNode still running after upstream restart"),
        original_pid
    );
}

#[then("the unchanged FullNode catches up to a pinned matching finalized checkpoint")]
fn unchanged_follower_catches_up(world: &mut World) {
    let height = world
        .state
        .resilience_observed_height
        .expect("pinned recovery height");
    let primary = world.validators.primary_port();
    let expected = world
        .rpc
        .checkpoint_at(primary, height)
        .expect("canonical recovery checkpoint");
    let follower_port = world.validators.http_port(FOLLOWER_RESILIENCE_SLOT);
    let original_pid = world
        .state
        .resilience_follower_pid
        .expect("original FullNode PID");
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        assert_eq!(
            world
                .localnet
                .live_follower_pid(FOLLOWER_RESILIENCE_NAME)
                .expect("FullNode must remain live"),
            original_pid
        );
        if world
            .rpc
            .finalized(follower_port)
            .is_some_and(|finalized| finalized >= height)
        {
            let observed = world
                .rpc
                .checkpoint_at(follower_port, height)
                .expect("FullNode pinned recovery checkpoint");
            assert_eq!(observed.block_hash, expected.block_hash);
            assert_eq!(observed.state_root, expected.state_root);
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "unchanged FullNode did not catch up after original upstream returned"
        );
        sleep(Duration::from_millis(250));
    }
}

#[then("the follower catches up to the committee finalized checkpoint with matching hash and state root")]
fn follower_catches_up_to_committee(world: &mut World) {
    let primary = world.validators.primary_port();
    let follower_port = world.validators.http_port(FOLLOWER_RESILIENCE_SLOT);

    let mut caught_up = false;
    for _ in 0..40 {
        if let Some(target) = world.rpc.finalized(primary) {
            let expected_hash = world
                .rpc
                .block_hash(primary, target)
                .expect("expected canonical block hash");
            let expected_root = world
                .rpc
                .state_root(primary, target)
                .expect("expected canonical state root");

            let follower_height = world.rpc.finalized(follower_port);
            let follower_hash = world.rpc.block_hash(follower_port, target);
            let follower_root = world.rpc.state_root(follower_port, target);

            if follower_height.is_some_and(|h| h >= target)
                && follower_hash.as_ref() == Some(&expected_hash)
                && follower_root.as_ref() == Some(&expected_root)
            {
                caught_up = true;
                break;
            }
        }
        sleep(Duration::from_secs(2));
    }
    assert!(
        caught_up,
        "follower failed to catch up to committee checkpoint"
    );
}
