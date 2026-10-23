//! Bounded encrypted Tribute population for the native auction path.
use crate::world::World;
use cucumber::{then, when};
use std::{
    thread,
    thread::sleep,
    time::{Duration, Instant},
};
#[when(
    expr = "{int} capacity owners submit one encrypted Tribute each at no more than two per block"
)]
fn bounded_capacity_owners_submit_public_tributes(world: &mut World, count: usize) {
    capacity_owners_submit_public_tributes(world, count, 2);
}

fn capacity_owners_submit_public_tributes(world: &mut World, count: usize, batch_size: usize) {
    let started = Instant::now();
    let port = world.validators.primary_port();
    let ports = world.validators.committee_ports();
    let mut block_counts = std::collections::BTreeMap::<u64, usize>::new();
    let private_keys = world.state.tribute_owner_keys.clone();
    assert!(
        private_keys.len() >= count,
        "capacity fixture retained only {} funded owners, expected at least {count}",
        private_keys.len()
    );
    let private_keys = &private_keys[..count];
    // Distinct callers share the declared testnet circuit network. Each offer
    // still has its own caller-bound proof and network-signed Merkle root.
    // Synthetic per-owner networks have no circuit binding on this host chain.
    const CAPACITY_L2_CHAIN_ID: u64 = 57_005;
    let network_operator = world
        .validators
        .get(0)
        .evm_key()
        .expect("capacity network administrator key");
    let administrator =
        crate::features::l2_registration::operator_address(world, &network_operator);
    crate::features::l2_registration::ensure_tribute_offer_operators(world, &[network_operator]);
    assert_eq!(
        world.rpc.l2_chain_by_l1_address(administrator),
        Some(CAPACITY_L2_CHAIN_ID),
        "fresh capacity fixture registers the declared circuit network"
    );
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
                        rpc.tribute_offer_for_network_with_params(
                            private_key,
                            CAPACITY_L2_CHAIN_ID,
                            crate::world::rpc::TributeOfferParams {
                                wwd: &worldwide_day,
                                amounts: ("2", "0"),
                                currency: 840,
                            },
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
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .unwrap_or_else(|error| panic!("{error}"));
        for transaction_hash in &batch {
            assert!(
                world.rpc.wait_successful_receipt(transaction_hash, 1_200),
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
    world.state.tribute_transactions = transaction_hashes;
}

#[then(expr = "all validators observe exactly {int} public Tributes for the capacity day")]
fn all_validators_observe_public_tributes(world: &mut World, count: usize) {
    let transaction_hashes = &world.state.tribute_transactions;
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
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            let supply_matches = world.rpc.supply(port).as_deref() == Some(&expected_supply);
            let day = outbe_primitives::time::WorldwideDay::new(worldwide_day);
            let day_matches = world.state.tribute_owner_keys[..count].iter().all(|key| {
                let Some(owner) = crate::internal::eth::address_of(key) else {
                    return false;
                };
                let Ok(id) = outbe_primitives::wwd_entity_id::derive_poseidon_entity_id(owner, day)
                else {
                    return false;
                };
                world
                    .rpc
                    .tributes_by_owner(port, owner)
                    .is_some_and(|ids| ids.len() == 1 && ids[0] == id.to_u256())
            });
            if supply_matches && day_matches {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "validator {port} did not expose {count} distinct Tributes"
            );
            sleep(Duration::from_millis(250));
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
