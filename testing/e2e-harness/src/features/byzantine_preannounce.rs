//! A byzantine leader forging committee pre-announces in the window after an
//! epoch boundary commits. Honest validators must reject every one, so
//! finalized history and FullNodes only ever carry the genuine committee.
//!
//! Validators inherit `OUTBE_TEST_BYZANTINE_PREANNOUNCE` from the harness
//! process; the node hook exists only in `test-protocol-overrides` builds.

use std::collections::BTreeMap;
use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::Bytes;
use cucumber::{given, then};
use eyre::{ensure, eyre, Result};
use outbe_evm::tee_attestation_activation::DcapSeededChainSpecBindingV1;
use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact;

use crate::internal::certified_handoff::{read_certified, AuthenticatedHistory};
use crate::world::World;

const BYZANTINE_ENV: &str = "OUTBE_TEST_BYZANTINE_PREANNOUNCE";
const FORGED_LOG: &str = "byzantine test hook: proposing a forged committee pre-announce";
const REJECTED_LOG: &str = "proposed block carries invalid header consensus artifact";
const HANDOFFS: u64 = 2;

#[given("every validator is armed to forge committee pre-announces")]
fn armed(_world: &mut World) {
    assert!(
        std::env::var_os(BYZANTINE_ENV).is_some(),
        "run this scenario with {BYZANTINE_ENV}=1 so every validator inherits it"
    );
}

#[then("finalized history authenticates from genesis through two committee handoffs")]
fn authenticate_history(world: &mut World) {
    authenticate(world).expect("finalized history carries only the genuine committees");
}

#[then("forged pre-announces were proposed and every one was rejected")]
fn forged_and_rejected(world: &mut World) {
    let (mut forged, mut rejected) = (0usize, 0usize);
    for index in 0..world.validators.size() {
        let pid = world
            .localnet
            .validator_pid(index)
            .expect("validator process");
        let log = world
            .localnet
            .node_launch_log(index, pid)
            .expect("validator launch log");
        forged += log.matches(FORGED_LOG).count();
        rejected += log
            .lines()
            .filter(|line| line.contains(REJECTED_LOG) && line.contains("pre-announce"))
            .count();
    }
    assert!(
        forged > 0,
        "no forged pre-announce was proposed; the window was never exercised"
    );
    assert!(
        rejected > 0,
        "honest validators never rejected a forged pre-announce"
    );
}

fn authenticate(world: &mut World) -> Result<()> {
    let primary = world.validators.primary_port();
    let binding = DcapSeededChainSpecBindingV1::from_genesis_path(
        &world.localnet.scenario_dir().join("genesis.json"),
    )
    .map_err(|error| eyre!(error))?;
    let mut history = AuthenticatedHistory::new(&binding)?;
    let mut preannounced = BTreeMap::<u64, Bytes>::new();
    let mut boundaries = BTreeMap::<u64, Bytes>::new();
    let deadline = Instant::now() + Duration::from_secs(900);
    while !(1..=HANDOFFS).all(|epoch| boundaries.contains_key(&epoch)) {
        ensure!(
            Instant::now() < deadline,
            "no {HANDOFFS} committee handoffs within the window"
        );
        let through = world.rpc.finalized_result(primary)?;
        while history.height() < through {
            let height = history.height() + 1;
            let proof = read_certified(&world.rpc, primary, height, history.member_count())?;
            // `advance` registers committees first-write-wins: a finalized forged
            // pre-announce makes the genuine boundary conflict here.
            match history.advance(&proof, world.rpc.checkpoint_at(primary, height)?)? {
                Some(ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, outcome }) => {
                    let first = preannounced.entry(epoch).or_insert_with(|| outcome.clone());
                    ensure!(
                        *first == outcome,
                        "two different finalized pre-announces for epoch {epoch}"
                    );
                }
                Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) => {
                    boundaries.insert(boundary.epoch, boundary.outcome);
                }
                Some(ConsensusHeaderArtifact::DealerLog(_)) | None => {}
            }
        }
        sleep(Duration::from_secs(2));
    }
    for (epoch, outcome) in &preannounced {
        if let Some(boundary) = boundaries.get(epoch) {
            ensure!(
                outcome == boundary,
                "finalized pre-announce for epoch {epoch} differs from its boundary"
            );
        }
    }
    Ok(())
}
