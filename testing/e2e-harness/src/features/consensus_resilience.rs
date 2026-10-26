//! Step definitions for `features/consensus_resilience.feature` (B8, B20, R7, R11, R13, S14).

use std::thread::sleep;
use std::time::Duration;

use cucumber::{given, then, when};

use crate::features::common::boot_localnet;
use crate::internal::eth;
use crate::world::World;

const FOLLOWER_RESILIENCE_SLOT: usize = 14;
const FOLLOWER_RESILIENCE_NAME: &str = "follower-resilience";
const FOLLOWER_RATE_LIMIT_NAME: &str = "follower-rate-limit";

mod follower;
mod nonce_order;
mod upstream;

fn epoch_checkpoint_attempts(world: &World) -> eyre::Result<u32> {
    let genesis_path = world.localnet.scenario_dir().join("genesis.json");
    let genesis = serde_json::from_slice(&std::fs::read(genesis_path)?)?;
    let budget = super::follower::reshare_wait_budget(&genesis)?;
    Ok(u32::try_from(
        budget
            .as_millis()
            .div_ceil(Duration::from_secs(3).as_millis()),
    )?)
}

#[when("two validators are stopped outside an epoch transition")]
fn stop_two_validators_outside_epoch(world: &mut World) {
    assert_eq!(
        world.validators.size(),
        4,
        "quorum fault requires four validators"
    );
    let ports = world.validators.committee_ports();
    let before = world
        .rpc
        .wait_finalized_checkpoint(&ports, 8, 60)
        .expect("all validators finalize before quorum fault");
    world.state.resilience_observed_height = Some(before.height);
    for index in [2, 3] {
        world
            .localnet
            .kill_validator(index)
            .expect("stop validator for quorum fault");
    }
}

#[then("the remaining two validators cannot finalize new blocks")]
fn two_validators_cannot_finalize(world: &mut World) {
    let ports = [world.validators.http_port(0), world.validators.http_port(1)];
    // Let any proposal already committed before the process stops settle first.
    sleep(Duration::from_secs(3));
    let settled = ports.map(|port| world.rpc.finalized(port).expect("survivor finality RPC"));
    sleep(Duration::from_secs(8));
    for (&port, &baseline) in ports.iter().zip(&settled) {
        assert_eq!(
            world.rpc.finalized(port),
            Some(baseline),
            "two of four validators finalized a block without quorum on port {port}"
        );
    }
    world.state.resilience_disconnected_height = Some(*settled.iter().max().unwrap());
}

#[when("both stopped validators restart from their durable data")]
fn restart_two_validators(world: &mut World) {
    for index in [2, 3] {
        world
            .localnet
            .restart_validator(index)
            .expect("restart validator after quorum fault");
    }
}

#[then("all four validators finalize a matching checkpoint beyond the outage")]
fn quorum_recovers_matching_checkpoint(world: &mut World) {
    let ports = world.validators.committee_ports();
    let outage_height = world
        .state
        .resilience_disconnected_height
        .expect("outage height");
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, outage_height + 2, 90)
        .expect("four validators recover finality after quorum returns");
    for port in ports {
        let observed = world
            .rpc
            .checkpoint_at(port, checkpoint.height)
            .expect("validator checkpoint after quorum recovery");
        assert_eq!(observed.block_hash, checkpoint.block_hash);
        assert_eq!(observed.state_root, checkpoint.state_root);
    }
}

// -----------------------------------------------------------------------
// B8: post-epoch validator restart recovery smoke
// -----------------------------------------------------------------------

#[given(expr = "a fresh localnet with a short epoch and {int}-block voting window")]
fn fresh_localnet_short_epoch(world: &mut World, window: u64) {
    boot_localnet(
        world,
        window,
        &[
            ("TESTNET_EPOCH_LENGTH_BLOCKS", "30".to_string()),
            ("TESTNET_DKG_PREPARE_WINDOW_BLOCKS", "10".to_string()),
            ("TESTNET_DEV_FELONY_THRESHOLD", "5".to_string()),
        ],
    );
}

#[given("the committee reaches a finalized height past the epoch transition")]
fn committee_past_epoch_transition(world: &mut World) {
    let ports = world.validators.committee_ports();
    let attempts = epoch_checkpoint_attempts(world).expect("epoch checkpoint budget from genesis");
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, 32, attempts)
        .expect("committee reaches past-epoch checkpoint");
    world.state.resilience_observed_height = Some(checkpoint.height);
}

#[when("an active validator is stopped and restarted after the epoch transition")]
fn stop_and_restart_validator_post_epoch(world: &mut World) {
    let target_slot = 3;
    world.state.resilience_target_slot = Some(target_slot);
    world
        .localnet
        .kill_validator(target_slot)
        .expect("kill validator-3 post-epoch");
    sleep(Duration::from_millis(500));
    world
        .localnet
        .restart_validator(target_slot)
        .expect("restart validator-3 from durable datadir");
}

#[then("the restarted validator catches up and resumes finalization")]
fn restarted_validator_catches_up_and_resumes(world: &mut World) {
    let target_slot = world
        .state
        .resilience_target_slot
        .expect("target slot must be recorded");
    let target_port = world.validators.http_port(target_slot);
    let floor_height = world
        .state
        .resilience_observed_height
        .expect("observed height must be recorded");

    let mut caught_up = false;
    for _ in 0..30 {
        if let Some(height) = world.rpc.finalized(target_port) {
            if height >= floor_height {
                caught_up = true;
                break;
            }
        }
        sleep(Duration::from_secs(1));
    }
    assert!(
        caught_up,
        "restarted validator did not resume finalization past floor height {floor_height}"
    );
}

#[then("the committee continues producing and finalizing blocks in lockstep")]
fn committee_continues_producing_and_finalizing_blocks(world: &mut World) {
    let ports = world.validators.committee_ports();
    let floor_height = world.state.resilience_observed_height.unwrap_or(10);
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, floor_height + 2, 60)
        .expect("committee continues producing blocks across post-epoch restart");
    world.state.resilience_observed_height = Some(checkpoint.height);
}

// -----------------------------------------------------------------------
// R7: follower sync recovery across upstream loss/stall
// -----------------------------------------------------------------------

// -----------------------------------------------------------------------
// R11: finalize vote ingress across epoch boundaries
// -----------------------------------------------------------------------

#[given("the committee advances towards an epoch transition")]
fn committee_advances_towards_epoch(world: &mut World) {
    let ports = world.validators.committee_ports();
    let attempts = epoch_checkpoint_attempts(world).expect("epoch checkpoint budget from genesis");
    // Wait until approaching epoch boundary (height 28 for a 30-block epoch)
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, 28, attempts)
        .expect("committee reaches approach to epoch boundary");
    world.state.resilience_observed_height = Some(checkpoint.height);
}

#[when("active validators process ingress finalize votes across epoch boundaries")]
fn process_ingress_finalize_votes(world: &mut World) {
    let ports = world.validators.committee_ports();
    for &port in &ports {
        assert!(
            world.rpc.finalized(port).is_some(),
            "validator at port {port} must have active finality"
        );
    }
}

#[then("the committee advances across the epoch boundary without progress stall")]
fn committee_advances_across_epoch_boundary(world: &mut World) {
    let ports = world.validators.committee_ports();
    let attempts = epoch_checkpoint_attempts(world).expect("epoch checkpoint budget from genesis");
    // In a 30-block epoch, height 32 is in epoch 1
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, 32, attempts)
        .expect("committee crosses epoch boundary to height 32");
    world.state.resilience_observed_height = Some(checkpoint.height);
}

#[then("all committee validators finalize blocks in lockstep past the transition")]
fn all_validators_finalize_past_transition(world: &mut World) {
    let ports = world.validators.committee_ports();
    let target_height = world.state.resilience_observed_height.unwrap_or(32);

    let first_hash = world
        .rpc
        .block_hash(ports[0], target_height)
        .expect("block hash port 0");
    let first_root = world
        .rpc
        .state_root(ports[0], target_height)
        .expect("state root port 0");

    for &port in &ports[1..] {
        let hash = world
            .rpc
            .block_hash(port, target_height)
            .expect("block hash");
        let root = world
            .rpc
            .state_root(port, target_height)
            .expect("state root");
        assert_eq!(
            hash, first_hash,
            "lockstep block hash mismatch across epoch transition"
        );
        assert_eq!(
            root, first_root,
            "lockstep state root mismatch across epoch transition"
        );
    }
}

// -----------------------------------------------------------------------
// R13: downtime recovery with empty V2 metadata
// -----------------------------------------------------------------------

#[when("an active validator experiences downtime while consensus advances")]
fn active_validator_experiences_downtime(world: &mut World) {
    let ports = world.validators.committee_ports();
    let current_checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, 2, 30)
        .expect("initial checkpoint before validator downtime");
    world.state.resilience_observed_height = Some(current_checkpoint.height);

    // Stop validator 2 during active rounds
    world
        .localnet
        .kill_validator(2)
        .expect("kill validator-2 during active rounds");
}

#[then("the surviving quorum finalizes the successor proposal past the stopped validator")]
fn surviving_quorum_finalizes_past_stopped_validator(world: &mut World) {
    let base_height = world
        .state
        .resilience_observed_height
        .expect("base height must be recorded");
    let survivor_ports = vec![
        world.validators.http_port(0),
        world.validators.http_port(1),
        world.validators.http_port(3),
    ];
    let next_checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&survivor_ports, base_height + 1, 60)
        .expect("surviving quorum finalizes successor proposal");
    world.state.resilience_observed_height = Some(next_checkpoint.height);
}

#[then("the successor block contains empty V2 missed-proposers metadata in phase 1 accounting")]
fn successor_block_contains_empty_v2_missed_proposers(world: &mut World) {
    let height = world
        .state
        .resilience_observed_height
        .expect("observed height must be recorded");
    let primary_port = world.validators.primary_port();
    let url = world.rpc.url(primary_port);

    let block_json = eth::raw_json_with_params(
        &url,
        "eth_getBlockByNumber",
        serde_json::json!([format!("0x{height:x}"), true]),
    )
    .expect("fetched successor block from primary RPC");

    let txs = block_json
        .get("transactions")
        .and_then(|v| v.as_array())
        .expect("transactions array in block");
    assert!(
        !txs.is_empty(),
        "block at height {height} must contain at least one system tx"
    );

    let first_tx = &txs[0];
    let input_hex = first_tx
        .get("input")
        .and_then(|v| v.as_str())
        .expect("first transaction input must be a hex string");
    let input_bytes = hex::decode(input_hex.trim_start_matches("0x"))
        .expect("transaction input must be valid hex");

    let decoded = outbe_primitives::system_tx::SystemTxInputV2::decode(&input_bytes)
        .expect("first transaction input must decode as SystemTxInputV2");

    let outbe_primitives::system_tx::SystemTxInputV2::CertifiedParentAccounting { metadata } =
        decoded
    else {
        panic!("first transaction input must be CertifiedParentAccounting, got different SystemTxInputV2 variant");
    };

    assert!(
        metadata.missed_proposers.is_empty(),
        "missed_proposers must be empty under V2 specs, found {:?}",
        metadata.missed_proposers
    );

    let parent_hash_str = block_json
        .get("parentHash")
        .and_then(|v| v.as_str())
        .expect("parentHash field in block");
    let expected_parent_hash: alloy_primitives::B256 =
        parent_hash_str.parse().expect("valid parentHash hex");
    assert_eq!(
        metadata.finalized_block_hash, expected_parent_hash,
        "CertifiedParentAccounting finalized_block_hash must match block parentHash"
    );
}

#[then("the committee continues producing and finalizing blocks without progress stall")]
fn committee_continues_without_stall(world: &mut World) {
    // Restart validator 2 to restore full committee
    world
        .localnet
        .restart_validator(2)
        .expect("restart validator-2");

    let ports = world.validators.committee_ports();
    let base_height = world.state.resilience_observed_height.unwrap_or(3);
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, base_height + 2, 60)
        .expect("committee continues block production after view gap recovery");
    world.state.resilience_observed_height = Some(checkpoint.height);
}

// -----------------------------------------------------------------------
// S14: txpool preservation and nonce sequence regression
// -----------------------------------------------------------------------

// B20: observe canonical headers through RPC after restart and follower replay.
// Negative block acceptance is exercised through the production EVM integration test.
#[then("finalized headers derive PREVRANDAO from their exact parent proofs")]
fn finalized_headers_bind_prev_randao(world: &mut World) {
    let mut ports = world.validators.committee_ports();
    if world.state.resilience_disconnected_height.is_some() {
        ports.push(world.validators.http_port(FOLLOWER_RESILIENCE_SLOT));
    }
    let height = ports
        .iter()
        .map(|port| {
            world
                .rpc
                .finalized(*port)
                .expect("node reports finalized height")
        })
        .min()
        .expect("at least one node");
    assert!(
        height >= 2,
        "PREVRANDAO check requires a certified-parent block"
    );
    // Include genesis, the short-epoch boundary, and the recovered suffix.
    let heights = [1, 2, 30, 31, 32, height.saturating_sub(1), height]
        .into_iter()
        .filter(|h| *h > 0 && *h <= height)
        .collect::<std::collections::BTreeSet<_>>();
    for port in ports {
        for height in &heights {
            let block = eth::raw_json_with_params(
                &world.rpc.url(port),
                "eth_getBlockByNumber",
                serde_json::json!([format!("0x{height:x}"), true]),
            )
            .expect("finalized block is available");
            let actual: alloy_primitives::B256 = block["mixHash"]
                .as_str()
                .expect("mixHash is present")
                .parse()
                .expect("mixHash is valid");
            let expected = if *height == 1 {
                alloy_primitives::B256::ZERO
            } else {
                parent_proof_randao(&block)
            };
            assert_eq!(
                actual, expected,
                "PREVRANDAO binding at node {port}, height {height}"
            );
        }
    }
}

fn parent_proof_randao(block: &serde_json::Value) -> alloy_primitives::B256 {
    use commonware_codec::{Encode as _, Read as _};
    use commonware_cryptography::{bls12381::primitives::variant::MinSig, Hasher as _, Sha256};
    use outbe_consensus::{digest::Digest, hybrid::HybridScheme};
    use outbe_primitives::{
        consensus_metadata::ParentParticipationProof, system_tx::SystemTxInputV2,
    };
    let input = block["transactions"][0]["input"]
        .as_str()
        .expect("Phase 1 input");
    let bytes = hex::decode(input.trim_start_matches("0x")).expect("Phase 1 input hex");
    let SystemTxInputV2::CertifiedParentAccounting { metadata } =
        SystemTxInputV2::decode(&bytes).expect("Phase 1 metadata")
    else {
        panic!("first transaction must account the parent");
    };
    let parent: alloy_primitives::B256 = block["parentHash"]
        .as_str()
        .expect("parentHash")
        .parse()
        .expect("valid parentHash");
    assert_eq!(metadata.finalized_block_hash, parent);
    let bound = metadata.ordered_committee.len();
    assert!(bound <= outbe_consensus::bls::MAX_VALIDATORS as usize);
    let mut reader = metadata.proof.as_ref();
    let (proposal, certificate) = match metadata.proof_kind {
        ParentParticipationProof::Finalization => {
            let proof = commonware_consensus::simplex::types::Finalization::<
                HybridScheme<MinSig>,
                Digest,
            >::read_cfg(&mut reader, &bound)
            .expect("decode parent finalization");
            (proof.proposal, proof.certificate)
        }
        ParentParticipationProof::CertifiedNotarization => {
            let proof = commonware_consensus::simplex::types::Notarization::<
                HybridScheme<MinSig>,
                Digest,
            >::read_cfg(&mut reader, &bound)
            .expect("decode parent notarization");
            (proof.proposal, proof.certificate)
        }
    };
    assert!(
        reader.is_empty(),
        "canonical parent proof has no trailing bytes"
    );
    assert_eq!(proposal.payload.0, parent);
    assert_eq!(proposal.round.epoch().get(), metadata.finalized_epoch);
    assert_eq!(proposal.round.view().get(), metadata.finalized_view);
    assert_eq!(proposal.parent.get(), metadata.parent_view);
    assert_eq!(
        certificate.vrf_proof.material_version,
        metadata.vrf_material_version
    );
    // Independent oracle for the existing protocol formula, not the production helper.
    alloy_primitives::B256::from_slice(
        Sha256::hash(&[certificate.vrf_proof.threshold_signature.encode().as_ref()]).as_ref(),
    )
}
