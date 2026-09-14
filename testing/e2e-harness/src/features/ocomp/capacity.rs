use crate::features::ocomp::*;

pub(in crate::features::ocomp) const OCOMP_CAPACITY_TRIBUTE_COUNT: usize = 257;

pub(in crate::features::ocomp) const OCOMP_CAPACITY_COMPLETION_TIMEOUT_SECS: u64 = 300;

const OCOMP_CAPACITY_NOD_MATERIALIZATION_TIMEOUT_SECS: u64 = 600;

// The SGX capacity lane intentionally submits 257 encrypted transactions on a
// loaded four-validator host. Keep its per-receipt bound at ten minutes while
// leaving ordinary scenario receipt waits unchanged (500 ms per attempt).
const OCOMP_CAPACITY_RECEIPT_ATTEMPTS: u32 = 1_200;

// Preserve the explicit two-per-block steps used by the smaller scenarios.
pub(in crate::features::ocomp) const OCOMP_CAPACITY_SUBMISSION_CONCURRENCY: usize = 2;

// The 256+1 capacity scenario measures bursts with one glibc arena in SGX.
// Finalize each batch before sending the next so blocks contain at most 20 offers.
const OCOMP_CAPACITY_BURST_SIZE: usize = 20;

#[when("all 257 capacity owners submit one encrypted Tribute each")]
fn capacity_owners_submit_257_public_tributes(world: &mut World) {
    capacity_owners_submit_public_tributes(
        world,
        OCOMP_CAPACITY_TRIBUTE_COUNT,
        OCOMP_CAPACITY_BURST_SIZE,
    );
}

#[when(
    expr = "{int} capacity owners submit one encrypted Tribute each at no more than two per block"
)]
fn bounded_capacity_owners_submit_public_tributes(world: &mut World, count: usize) {
    capacity_owners_submit_public_tributes(world, count, OCOMP_CAPACITY_SUBMISSION_CONCURRENCY);
}

fn capacity_owners_submit_public_tributes(world: &mut World, count: usize, batch_size: usize) {
    let started = Instant::now();
    let port = world.validators.primary_port();
    let ports = world.validators.committee_ports();
    let mut block_counts = std::collections::BTreeMap::<u64, usize>::new();
    let private_keys = world.state.ocomp_capacity_tribute_private_keys.clone();
    assert!(
        private_keys.len() >= count,
        "capacity fixture retained only {} funded owners, expected at least {count}",
        private_keys.len()
    );
    let private_keys = &private_keys[..count];
    // Every capacity owner offers as its own EOA, so each one needs its own
    // zk-disabled L2Registry registration before the burst starts.
    crate::features::l2_registration::ensure_tribute_offer_operators(world, private_keys);
    let worldwide_day = world
        .state
        .wwd
        .clone()
        .expect("capacity WorldwideDay is set");
    let mut transaction_hashes = Vec::with_capacity(private_keys.len());

    for keys in private_keys.chunks(batch_size) {
        let batch_started = Instant::now();
        let batch = thread::scope(|scope| {
            keys.iter()
                .map(|private_key| {
                    let rpc = world.rpc.clone();
                    let worldwide_day = worldwide_day.clone();
                    scope.spawn(move || {
                        rpc.tribute_offer_with_params(
                            private_key,
                            &worldwide_day,
                            OCOMP_PUBLIC_TRIBUTE_AMOUNT_BASE,
                            OCOMP_PUBLIC_TRIBUTE_AMOUNT_MICRO,
                            840,
                            false,
                        )
                        .ok_or_else(|| {
                            format!(
                                "capacity owner {} did not return a public Tribute tx hash",
                                rpc.address_of(private_key)
                                    .unwrap_or_else(|| "unknown".to_owned())
                            )
                        })
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| "capacity Tribute submission thread panicked".to_owned())?
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .unwrap_or_else(|error| panic!("{error}"));
        for transaction_hash in &batch {
            assert!(
                world
                    .rpc
                    .wait_successful_receipt(transaction_hash, OCOMP_CAPACITY_RECEIPT_ATTEMPTS),
                "capacity Tribute transaction did not succeed: {transaction_hash}"
            );
        }
        let last_height = batch
            .iter()
            .map(|tx| {
                world
                    .rpc
                    .receipt_block_number(tx, port)
                    .expect("mined receipt height")
            })
            .max()
            .expect("nonempty capacity batch");
        world
            .rpc
            .wait_finalized_checkpoint(&ports, last_height, 100)
            .expect("capacity batch finalizes on every validator with identical hash/root");
        for tx in &batch {
            let receipt = world
                .rpc
                .transaction_receipt(tx, port)
                .expect("finalized receipt");
            let height = u64::from_str_radix(
                receipt["blockNumber"]
                    .as_str()
                    .expect("receipt block number")
                    .trim_start_matches("0x"),
                16,
            )
            .expect("hex receipt block number");
            assert!(
                height <= last_height,
                "capacity receipt moved after finalization: {tx}"
            );
            assert_eq!(
                receipt["status"].as_str(),
                Some("0x1"),
                "capacity offer reverted: {tx}"
            );
            let canonical_hash = world
                .rpc
                .block_hash(port, height)
                .expect("canonical block hash");
            assert_eq!(
                receipt["blockHash"].as_str(),
                Some(canonical_hash.as_str()),
                "capacity receipt belongs to an orphaned block: {tx}"
            );
            let occupancy = block_counts.entry(height).or_default();
            *occupancy += 1;
            assert!(
                *occupancy <= batch_size,
                "capacity block {height} exceeds {batch_size} offers"
            );
        }
        transaction_hashes.extend(batch);
        eprintln!(
            "E2E_CAPACITY_POPULATION finalized={}/{} batch_size={} batch_ms={} elapsed_ms={} blocks={block_counts:?}",
            transaction_hashes.len(), count, keys.len(), batch_started.elapsed().as_millis(), started.elapsed().as_millis(),
        );
    }

    assert_eq!(
        transaction_hashes.len(),
        count,
        "not every capacity owner submitted a public Tribute"
    );
    world.state.ocomp_capacity_tribute_tx_hashes = transaction_hashes;
}

#[then(expr = "all validators observe exactly {int} public Tributes for the capacity day")]
fn all_validators_observe_public_tributes(world: &mut World, count: usize) {
    let transaction_hashes = &world.state.ocomp_capacity_tribute_tx_hashes;
    assert_eq!(transaction_hashes.len(), count);
    let expected_supply = count.to_string();
    let worldwide_day = world
        .state
        .wwd
        .as_deref()
        .expect("capacity WorldwideDay")
        .parse::<u32>()
        .expect("numeric capacity WorldwideDay");
    for port in world.validators.committee_ports() {
        let deadline = Instant::now() + Duration::from_secs(OCOMP_CAPACITY_COMPLETION_TIMEOUT_SECS);
        loop {
            let supply_matches = world.rpc.supply(port).as_deref() == Some(&expected_supply);
            let day_matches = world
                .rpc
                .tributes_by_day(port, worldwide_day)
                .is_some_and(|ids| {
                    ids.len() == count
                        && ids.iter().collect::<std::collections::BTreeSet<_>>().len() == count
                });
            match bounded_completion_decision(
                supply_matches && day_matches,
                Instant::now(),
                deadline,
            ) {
                BoundedCompletionDecision::Complete => break,
                BoundedCompletionDecision::Continue => sleep(Duration::from_millis(250)),
                BoundedCompletionDecision::TimedOut => {
                    panic!("validator {port} did not expose {count} distinct Tributes")
                }
            }
        }
    }
    for transaction_hash in [
        transaction_hashes
            .first()
            .expect("first capacity transaction"),
        transaction_hashes
            .last()
            .expect("last capacity transaction"),
    ] {
        world
            .projection
            .wait_for_tribute_projection(transaction_hash, 60)
            .unwrap_or_else(|error| {
                panic!(
                    "capacity boundary Tribute {transaction_hash} was not projected by every validator: {error}"
                )
            });
    }
}

#[then("three matching validator domains atomically certify the Lysis generation")]
fn validators_certify_lysis_generation(world: &mut World) {
    quorum_applies_lysis_and_creates_nod(world);
}

#[then("mineGratis is rejected while that certified generation is incomplete")]
fn mine_is_rejected_before_materialization_completion(world: &mut World) {
    let private_key = world
        .state
        .ocomp_capacity_tribute_private_keys
        .first()
        .expect("first capacity owner key")
        .clone();
    let generation = world
        .state
        .ocomp_certified_generation
        .clone()
        .expect("certified generation before mining gate");
    world
        .rpc
        .assert_certified_nod_mining_blocked(
            world.validators.primary_port(),
            &private_key,
            &generation,
        )
        .expect("pre-completion certified NOD mining rejection");
}

#[then("the certified generation is materialized through at least two bounded transactions")]
fn certified_generation_crosses_multiple_materialization_batches(world: &mut World) {
    let generation = world
        .state
        .ocomp_certified_generation
        .clone()
        .expect("certified generation before materialization");
    let observation = world
        .rpc
        .wait_for_completed_nod_materialization(
            world.validators.primary_port(),
            &generation,
            OCOMP_CAPACITY_NOD_MATERIALIZATION_TIMEOUT_SECS,
        )
        .expect("completed multi-batch NOD materialization");
    assert!(observation.successful_batch_transactions >= 2);
    world.state.ocomp_nod_materialization = Some(observation);
}

#[then("every capacity owner enumerates one ordinary NOD with matching nodData")]
fn every_capacity_owner_has_one_materialized_nod(world: &mut World) {
    assert_materialized_capacity_owners(world, usize::MAX);
}

#[then("five deterministic capacity owners enumerate ordinary NODs with matching nodData")]
fn five_capacity_owners_have_materialized_nods(world: &mut World) {
    assert_materialized_capacity_owners(world, 5);
}

fn assert_materialized_capacity_owners(world: &mut World, limit: usize) {
    let count = world
        .state
        .ocomp_capacity_tribute_tx_hashes
        .len()
        .min(limit);
    let completion_block_number = world
        .state
        .ocomp_nod_materialization
        .as_ref()
        .expect("materialization completion before owner reads")
        .completion_block_number;
    for private_key in &world.state.ocomp_capacity_tribute_private_keys[..count] {
        let owner = world
            .rpc
            .address_of(private_key)
            .expect("capacity owner address")
            .parse()
            .expect("capacity owner address format");
        world
            .rpc
            .assert_one_materialized_nod_for_owner(
                world.validators.primary_port(),
                owner,
                completion_block_number,
            )
            .expect("ordinary owner NOD and nodData");
    }
}

#[then("mineGratis succeeds after the certified generation is completely materialized")]
fn mine_succeeds_after_materialization_completion(world: &mut World) {
    let private_key = world
        .state
        .ocomp_capacity_tribute_private_keys
        .first()
        .expect("first capacity owner key")
        .clone();
    let port = world.validators.primary_port();
    let owner = world
        .rpc
        .address_of(&private_key)
        .expect("capacity owner address")
        .parse::<alloy_primitives::Address>()
        .expect("canonical capacity owner address");
    let nod_id = world
        .rpc
        .nod_id_of_owner_by_index_on(port, owner, 0)
        .expect("capacity owner NOD read")
        .expect("capacity owner NOD is available");
    let body = world
        .rpc
        .nod_data_on(port, &nod_id)
        .expect("capacity owner NOD body");
    // Mining always burns a note, so the capacity Nod needs one deposited under
    // an asset the router registers for its reference currency — the fixture
    // genesis registers liquidity sources but no vault.
    assert_eq!(
        body.referenceCurrency, 840,
        "the settlement fixture only registers an asset for USD"
    );
    let fixture = crate::features::settlement::deploy_settlement_fixture(world);
    let proof = crate::features::paynote::deposit_and_prove(
        world,
        port,
        &private_key,
        owner,
        fixture.asset,
        body.costAmountMinor,
    );
    world
        .rpc
        .mine_first_materialized_capacity_nod(port, &private_key, &proof)
        .expect("post-completion mineGratis");
}

#[then("the completed materialization cursor and ordinary NOD set remain unchanged")]
fn completed_materialization_survives_restart(world: &mut World) {
    let before = world
        .state
        .ocomp_nod_materialization
        .clone()
        .expect("materialization observation before restart");
    let after = world
        .rpc
        .completed_nod_materialization(
            world.validators.primary_port(),
            world
                .state
                .ocomp_certified_generation
                .as_ref()
                .expect("certified generation after restart"),
        )
        .expect("materialization observation after restart");
    assert_eq!(after, before);
    assert_materialized_capacity_owners(world, 5);
}

#[then("the certified generation contains exactly 257 Tribute and Nod records")]
fn certified_generation_contains_257_records(world: &mut World) {
    let generation = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .expect("capacity certified generation");
    assert_eq!(
        generation.tribute_count,
        u32::try_from(OCOMP_CAPACITY_TRIBUTE_COUNT).expect("capacity count fits u32")
    );
    assert_eq!(
        generation.nod_count,
        u32::try_from(OCOMP_CAPACITY_TRIBUTE_COUNT).expect("capacity count fits u32")
    );
    assert_eq!(
        outbe_ocomp_protocol::capacity::worker_shard_count(
            u64::from(generation.tribute_count),
            u32::try_from(
                outbe_ocomp_protocol::generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1
                    .max_tributes_per_work_shard
            )
            .expect("generated shard cap fits u32"),
        )
        .expect("non-zero generated shard cap"),
        2,
        "the public S+1 population must be covered by two worker shards"
    );
    let fresh_worldwide_day = world
        .state
        .metadosis_fresh_lifecycle_observation
        .as_ref()
        .map(|_| fresh_metadosis_wwd(world));
    if let Some(worldwide_day) = fresh_worldwide_day {
        let finalized_height = world
            .state
            .ocomp_capacity_observation
            .as_ref()
            .expect("fresh capacity public-path observation")
            .finalized_block_number;
        let completed = world
            .validators
            .committee_ports()
            .into_iter()
            .map(|port| {
                world
                    .rpc
                    .metadosis_wwd_state_at(port, worldwide_day, finalized_height)
            })
            .collect::<Vec<_>>();
        assert!(
            completed
                .iter()
                .all(|state| state.as_ref().is_some_and(|state| state.status == 6)),
            "the runtime-created fresh WWD is not COMPLETED on every validator"
        );
        if let Some(lifecycle) = world.state.metadosis_fresh_lifecycle_observation.as_mut() {
            lifecycle.completed_validator_count = 4;
        }
    }
}
