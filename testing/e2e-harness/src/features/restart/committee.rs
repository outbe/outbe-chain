use crate::world::World;

use cucumber::then;
use cucumber::when;

/// Stop every committee node and enclave, then relaunch them from the same
/// datadirs. Each identity must unseal its own permanent key; there is no peer
/// redelivery path.
#[when("the entire committee and its enclaves are stopped and restarted")]
fn committee_and_enclaves_restarted(world: &mut World) {
    let ports = world.validators.committee_ports();
    let height = ports
        .iter()
        .map(|&port| {
            world
                .rpc
                .finalized_result(port)
                .expect("pre-restart finality")
        })
        .min()
        .expect("nonempty committee");
    let before = world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 1)
        .expect("common pre-restart finalized hash/root");
    let original_pids: Vec<_> = (0..world.validators.size())
        .map(|index| {
            world
                .localnet
                .live_validator_and_enclave_pids(index)
                .expect("both original committee processes must be owned and live")
        })
        .collect();
    assert!(
        world.state.committee_restart.is_empty(),
        "committee restart already armed"
    );
    world.state.marker_height = Some(before.height);
    world.state.restart_observations.push(serde_json::json!({
        "phase": "committee_before_restart",
        "checkpoint": committee_checkpoint_json(before),
        "original_pids": original_pids,
    }));
    let mut logs = Vec::new();
    world
        .localnet
        .restart_committee_and_enclaves_observed(|stopped| {
            // Old processes have been reaped; no replacement has started yet.
            for index in 0..original_pids.len() {
                let dir = stopped.scenario_dir().join(format!("validator-{index}"));
                logs.push((
                    crate::internal::launch_log::LaunchLog::arm(&dir.join("node.log"))?,
                    crate::internal::launch_log::LaunchLog::arm(&dir.join("enclave.log"))?,
                ));
            }
            Ok(())
        })
        .expect("restart committee and enclaves");
    for (index, (node_log, enclave_log)) in logs.into_iter().enumerate() {
        let (node_pid, enclave_pid) = world
            .localnet
            .live_validator_and_enclave_pids(index)
            .expect("both replacement committee processes must be owned and live");
        world.state.restart_observations.push(serde_json::json!({
            "phase": "committee_replacement",
            "validator": index,
            "node_pid": node_pid,
            "enclave_pid": enclave_pid,
            "node_log_start": node_log.start_offset(),
            "enclave_log_start": enclave_log.start_offset(),
        }));
        assert_ne!(node_pid, original_pids[index].0, "node was not replaced");
        assert_ne!(
            enclave_pid, original_pids[index].1,
            "enclave was not replaced"
        );
        world
            .state
            .committee_restart
            .push(crate::world::state::RestartIncarnation {
                node_pid,
                enclave_pid,
                node_log,
                enclave_log,
            });
    }
}

/// Every enclave must use its restart fast-path, every validator must advance,
/// and an enclave-backed Tribute offer must remain executable.
#[then("all validators recover sealed TEE state and resume finalization")]
fn committee_recovers_sealed_tee_state(world: &mut World) {
    let before = world.state.marker_height.expect("pre-restart height");
    let ports = world.validators.committee_ports();
    committee_assert_live(world);
    let target = world
        .rpc
        .fresh_finality_target(&ports)
        .expect("fresh target from every restarted validator")
        .max(before.checked_add(2).expect("restart height overflow"));
    let progressed = world
        .rpc
        .wait_finalized_checkpoint(&ports, target, 60)
        .expect("restarted committee must advance on one finalized hash/root");
    committee_assert_live(world);
    let before_checkpoint = world
        .state
        .restart_observations
        .iter()
        .rev()
        .find(|observation| observation["phase"] == "committee_before_restart")
        .expect("retained pre-restart checkpoint")["checkpoint"]
        .clone();
    for &port in &ports {
        assert_eq!(
            committee_checkpoint_json(
                world
                    .rpc
                    .checkpoint_at(port, before)
                    .expect("pre-restart finalized block must remain canonical")
            ),
            before_checkpoint,
            "validator RPC {port} changed the pre-restart finalized hash/root"
        );
    }
    world.state.restart_observations.push(serde_json::json!({
        "phase": "committee_restarted_finality",
        "target": target,
        "checkpoint": committee_checkpoint_json(progressed),
    }));
    for index in 0..world.validators.size() {
        let incarnation = &mut world.state.committee_restart[index];
        let enclave_log = incarnation
            .enclave_log
            .read()
            .expect("replacement enclave log identity");
        let unsealed: Vec<_> = enclave_log.lines().filter(|line| {
            *line == "outbe-tee-enclave: unsealed offer key + group signature <- /tee/sealed_root.bin (restart fast-path)"
        }).collect();
        world.state.restart_observations.push(serde_json::json!({
            "phase": "committee_unsealed",
            "validator": index,
            "enclave_pid": incarnation.enclave_pid,
            "records": unsealed,
        }));
        assert_eq!(
            unsealed.len(),
            1,
            "validator-{index} enclave did not recover its sealed offer key"
        );
    }
    let wwd = world.state.wwd.clone().expect("wwd");
    let key = world.validators.get(0).evm_key().expect("validator-0 key");
    let primary = world.validators.primary_port();
    let supply_before = committee_supply_at(world, primary, progressed);
    for &port in &ports {
        assert_eq!(
            committee_supply_at(world, port, progressed),
            supply_before,
            "pre-offer finalized supply parity"
        );
    }
    let expected_supply = supply_before
        .checked_add(alloy_primitives::U256::from(1))
        .expect("Tribute supply overflow");
    // Retain an exact prefix of the existing launch capture. The suffix must
    // come from these same processes after this checkpoint, not earlier replay.
    let offer_prefixes: Vec<_> = world
        .state
        .committee_restart
        .iter_mut()
        .map(|incarnation| {
            incarnation
                .enclave_log
                .read()
                .expect("checkpoint replacement enclave log before the new offer")
        })
        .collect();
    committee_assert_live(world);
    world.state.restart_observations.push(serde_json::json!({
        "phase": "committee_before_new_offer",
        "checkpoint": committee_checkpoint_json(progressed),
        "supply": supply_before.to_string(),
        "expected_supply": expected_supply.to_string(),
        "enclave_log_starts": world.state.committee_restart.iter().zip(&offer_prefixes)
            .map(|(incarnation, prefix)| incarnation.enclave_log.start_offset()
                .checked_add(u64::try_from(prefix.len()).expect("log length fits u64"))
                .expect("offer log offset overflow")).collect::<Vec<_>>(),
    }));
    // This helper submits a real offer before polling, even if supply is already visible.
    let transaction_hash = world
        .rpc
        .offer_until_supply_hash(&key, &wwd, primary, &expected_supply.to_string(), 5)
        .expect("new post-restart Tribute offer must be submitted and included");
    let receipt = crate::internal::eth::receipt_json(&world.rpc.url(primary), &transaction_hash)
        .expect("new Tribute transaction receipt must be observable");
    world.state.restart_observations.push(serde_json::json!({
        "phase": "committee_new_offer_receipt",
        "transaction_hash": transaction_hash,
        "receipt": receipt,
    }));
    let outcome = crate::world::rpc::TxOutcome {
        transaction_hash,
        success: receipt.get("status").and_then(serde_json::Value::as_str) == Some("0x1"),
        receipt,
    };
    assert!(outcome.success, "new Tribute offer reverted");
    assert!(
        outcome.block_number().expect("Tribute receipt height") > progressed.height,
        "Tribute receipt predates post-restart observation"
    );
    let finalized = world.rpc.finalize_outcome(&outcome, &ports, 60).expect(
        "new successful Tribute receipt must be canonical and finalized on every validator",
    );
    committee_assert_live(world);
    for &port in &ports {
        assert_eq!(
            committee_supply_at(world, port, finalized),
            expected_supply,
            "new finalized Tribute must increase supply by exactly one"
        );
    }
    world.state.restart_observations.push(serde_json::json!({
        "phase": "committee_new_offer_finalized",
        "transaction_hash": outcome.transaction_hash,
        "checkpoint": committee_checkpoint_json(finalized),
        "supply_before": supply_before.to_string(),
        "supply_after": expected_supply.to_string(),
    }));
    for (index, prefix) in offer_prefixes.iter().enumerate() {
        let incarnation = &mut world.state.committee_restart[index];
        incarnation
            .node_log
            .seal()
            .expect("seal replacement node observation");
        incarnation
            .enclave_log
            .seal()
            .expect("seal replacement enclave observation");
        let enclave_log = incarnation
            .enclave_log
            .read()
            .expect("replacement enclave log identity");
        let offer_log = enclave_log
            .strip_prefix(prefix.as_str())
            .expect("pre-offer launch log prefix changed");
        let decrypted: Vec<_> = offer_log
            .lines()
            .filter(|line| {
                line.starts_with("outbe-tee-enclave: req=process_tribute_offer_batch ")
                    && line
                        .split_ascii_whitespace()
                        .filter(|field| field.starts_with("outcome="))
                        .eq(std::iter::once("outcome=ok"))
            })
            .collect();
        let node_log = incarnation
            .node_log
            .read()
            .expect("replacement node log identity");
        let ceremonies: Vec<_> = node_log
            .lines()
            .filter(|line| line.contains("running DKG ceremony"))
            .collect();
        world.state.restart_observations.push(serde_json::json!({
            "phase": "committee_restart_served_new_offer",
            "validator": index,
            "node_pid": incarnation.node_pid,
            "enclave_pid": incarnation.enclave_pid,
            "decrypt_records": decrypted,
            "new_ceremony_records": ceremonies,
        }));
        assert!(
            !decrypted.is_empty(),
            "validator-{index} lacks a successful new-offer decrypt"
        );
        assert!(
            ceremonies.is_empty(),
            "validator-{index} restart triggered a fresh DKG ceremony"
        );
    }
    committee_assert_live(world);
}

fn committee_assert_live(world: &mut World) {
    assert_eq!(
        world.state.committee_restart.len(),
        world.validators.size(),
        "every committee replacement must be observed"
    );
    for index in 0..world.validators.size() {
        let observed = world
            .localnet
            .live_validator_and_enclave_pids(index)
            .expect("committee replacement processes must remain owned and live");
        let expected = &world.state.committee_restart[index];
        assert_eq!(
            observed,
            (expected.node_pid, expected.enclave_pid),
            "validator-{index} changed process incarnation during restart proof"
        );
    }
}

pub(super) fn committee_checkpoint_json(
    checkpoint: crate::world::rpc::FinalizedCheckpoint,
) -> serde_json::Value {
    serde_json::json!({
        "height": checkpoint.height,
        "block_hash": format!("{:#x}", checkpoint.block_hash),
        "state_root": format!("{:#x}", checkpoint.state_root),
    })
}

fn committee_supply_at(
    world: &World,
    port: u16,
    checkpoint: crate::world::rpc::FinalizedCheckpoint,
) -> alloy_primitives::U256 {
    assert!(
        world
            .rpc
            .finalized_result(port)
            .expect("supply observation finality")
            >= checkpoint.height
    );
    assert_eq!(
        world
            .rpc
            .checkpoint_at(port, checkpoint.height)
            .expect("supply checkpoint"),
        checkpoint
    );
    let supply = crate::internal::eth::read_call_at_result(
        &world.rpc.url(port),
        crate::internal::addresses::TRIBUTE_ADDR,
        &crate::internal::eth::ITribute::totalSupplyCall {},
        checkpoint.height,
    )
    .expect("read Tribute supply at exact finalized checkpoint");
    assert_eq!(
        world
            .rpc
            .checkpoint_at(port, checkpoint.height)
            .expect("recheck supply checkpoint"),
        checkpoint
    );
    supply
}
