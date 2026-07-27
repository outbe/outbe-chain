//! OCM-24 process-topology acceptance steps.
//!
//! The scenario extends the normal localnet lifecycle and launches only the
//! production `outbe-ocomp` executable. It cannot construct jobs, results,
//! roots or chain state.

use std::{
    str::FromStr as _,
    thread::{self, sleep},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use alloy_primitives::B256;
use cucumber::{given, then, when};
use outbe_ocomp_protocol::{
    profile::poc_schema_limits,
    state::{OcompJobStatus, OcompTerminalOutcome},
    vote::ResultVoteV1,
};

use crate::features::common::{
    bootstrap_final_ocomp_localnet, bootstrap_localnet, start_bootstrapped_localnet,
};
use crate::internal::eth;
use crate::world::localnet::StartOpts;
use crate::world::ocomp::OCOMP_CAPACITY_OFFERING_AFTER_GENESIS_SECS;
use crate::world::ocomp::{
    OcompForkMismatchEvidenceV1, OcompForkRestartEvidenceV1, OcompMeasurementForkV1,
    OcompProcessFault, OcompProcessRole, OCOMP_MEASUREMENT_ACTIVATION_HEIGHT,
};
use crate::world::World;

const OCOMP_CAPACITY_TRIBUTE_COUNT: usize = 257;
const OCOMP_CAPACITY_SUBMISSION_CONCURRENCY: usize = 8;

#[given("a fresh four-validator OCOMP measurement localnet")]
fn fresh_ocomp_measurement_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, None);
}

#[given("a fresh four-validator OCOMP public measurement localnet")]
fn fresh_ocomp_public_measurement_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, Some(0));
}

#[given("a fresh four-validator OCOMP public capacity localnet")]
fn fresh_ocomp_public_capacity_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, Some(OCOMP_CAPACITY_TRIBUTE_COUNT));
}

#[given("the canonical four-validator OCOMP Final devnet")]
fn canonical_ocomp_final_devnet(world: &mut World) {
    bootstrap_final_ocomp_localnet(world, 6);
    world.state.ocomp_capacity_tribute_private_keys = world
        .ocomp
        .final_capacity_tribute_private_keys(OCOMP_CAPACITY_TRIBUTE_COUNT)
        .expect("derive funded canonical capacity owners");
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_secs();
    let mut start_opts = StartOpts {
        voting_window: Some(6),
        unix_time_offset_secs: Some(
            world
                .localnet
                .ocomp_final_clock_offset(now_secs)
                .expect("derive canonical OCOMP logical clock"),
        ),
        genesis_timestamp_pre_shifted: true,
        ocomp_protocol_bundle_hash: None,
    };
    let prepared = world
        .ocomp
        .prepare_final_fork_install()
        .expect("load canonical OCOMP Final install");
    launch_prepared_ocomp(world, &mut start_opts, &prepared, false);
    wait_for_finalized_ocomp_activation(world);
}

fn start_ocomp_measurement_localnet(
    world: &mut World,
    public_capacity_tribute_count: Option<usize>,
) {
    let shorten_public_day = public_capacity_tribute_count.is_some();
    bootstrap_localnet(world, 6, &[]);
    let mut start_opts = if shorten_public_day {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after unix epoch")
            .as_secs();
        let boundary_lead_secs = if public_capacity_tribute_count == Some(0) {
            120
        } else {
            OCOMP_CAPACITY_OFFERING_AFTER_GENESIS_SECS
        };
        let mut opts = StartOpts::near_next_utc_day_with_lead(6, now_secs, boundary_lead_secs);
        let offset = opts
            .unix_time_offset_secs
            .expect("public measurement clock offset");
        world
            .localnet
            .shift_genesis_timestamp(offset)
            .expect("shift public measurement genesis before deriving fork identity");
        opts.genesis_timestamp_pre_shifted = true;
        opts
    } else {
        StartOpts::default()
    };
    let measurement_fork = match public_capacity_tribute_count {
        Some(0) => world
            .ocomp
            .prepare_public_measurement_fork_install()
            .expect("publish the immutable public measurement fork before node launch"),
        Some(tribute_count) => {
            let (prepared, private_keys) = world
                .ocomp
                .prepare_public_capacity_fork_install(tribute_count)
                .expect("fund capacity owners and publish the immutable measurement fork");
            world.state.ocomp_capacity_tribute_private_keys = private_keys;
            prepared
        }
        None => world
            .ocomp
            .prepare_measurement_fork_install()
            .expect("publish the immutable measurement fork before node launch"),
    };
    launch_prepared_ocomp(
        world,
        &mut start_opts,
        &measurement_fork,
        !shorten_public_day,
    );
    if shorten_public_day {
        wait_for_finalized_ocomp_activation(world);
    }
}

fn launch_prepared_ocomp(
    world: &mut World,
    start_opts: &mut StartOpts,
    prepared: &OcompMeasurementForkV1,
    activate_workers: bool,
) {
    let expected_identity = prepared.launch_identity();
    start_opts.ocomp_protocol_bundle_hash =
        Some(format!("{:#x}", expected_identity.protocol_bundle_hash));
    start_bootstrapped_localnet(world, start_opts);

    let primary = world.validators.primary_port();
    let chain_id = world
        .rpc
        .chain_id(primary)
        .expect("read measurement chain id from public RPC");
    let genesis_hash = world
        .rpc
        .block_hash(primary, 0)
        .and_then(|hash| B256::from_str(&hash).ok())
        .expect("read measurement genesis hash from public RPC");
    assert_eq!(chain_id, expected_identity.chain_id);
    assert_eq!(genesis_hash, expected_identity.genesis_hash);
    let identity = expected_identity;
    world
        .ocomp
        .start_validator_roles(identity)
        .expect("start all production node-facing OCOMP roles");
    if activate_workers {
        for validator_index in 0..4_u8 {
            world
                .ocomp
                .activate_worker(validator_index, 0, identity)
                .unwrap_or_else(|error| {
                    panic!("activate validator-{validator_index} production worker: {error}")
                });
        }
    }
}

fn wait_for_finalized_ocomp_activation(world: &mut World) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let finalized = world
            .validators
            .committee_ports()
            .into_iter()
            .map(|port| world.rpc.finalized(port))
            .collect::<Vec<_>>();
        if finalized.iter().all(|height| {
            height.is_some_and(|height| height >= OCOMP_MEASUREMENT_ACTIVATION_HEIGHT)
        }) {
            return;
        }
        world
            .ocomp
            .ensure_validator_roles_alive()
            .expect("OCOMP roles stay alive until the immutable fork is active");
        assert!(
            Instant::now() < deadline,
            "OCOMP fork did not finalize on every validator before public Tribute submission: \
             expected height {OCOMP_MEASUREMENT_ACTIVATION_HEIGHT}, observed {finalized:?}"
        );
        sleep(Duration::from_millis(250));
    }
}

#[when("all 257 capacity owners submit one encrypted Tribute each")]
fn capacity_owners_submit_257_public_tributes(world: &mut World) {
    let private_keys = world.state.ocomp_capacity_tribute_private_keys.clone();
    assert_eq!(
        private_keys.len(),
        OCOMP_CAPACITY_TRIBUTE_COUNT,
        "capacity fixture did not retain exactly 257 funded owners"
    );
    let worldwide_day = world
        .state
        .wwd
        .clone()
        .expect("capacity WorldwideDay is set");
    let mut transaction_hashes = Vec::with_capacity(private_keys.len());

    for keys in private_keys.chunks(OCOMP_CAPACITY_SUBMISSION_CONCURRENCY) {
        let batch = thread::scope(|scope| {
            keys.iter()
                .map(|private_key| {
                    let rpc = world.rpc.clone();
                    let worldwide_day = worldwide_day.clone();
                    scope.spawn(move || {
                        rpc.tribute_offer(private_key, &worldwide_day)
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
                world.rpc.wait_successful_receipt(transaction_hash, 240),
                "capacity Tribute transaction did not succeed: {transaction_hash}"
            );
        }
        transaction_hashes.extend(batch);
    }

    assert_eq!(
        transaction_hashes.len(),
        OCOMP_CAPACITY_TRIBUTE_COUNT,
        "not every capacity owner submitted a public Tribute"
    );
    world.state.ocomp_capacity_tribute_tx_hashes = transaction_hashes;
}

#[then("all validators observe exactly 257 public Tributes for the capacity day")]
fn all_validators_observe_257_public_tributes(world: &mut World) {
    let transaction_hashes = &world.state.ocomp_capacity_tribute_tx_hashes;
    assert_eq!(
        transaction_hashes.len(),
        OCOMP_CAPACITY_TRIBUTE_COUNT,
        "capacity Tribute transaction set is incomplete"
    );
    for transaction_hash in transaction_hashes {
        assert!(
            world.rpc.wait_successful_receipt(transaction_hash, 240),
            "capacity Tribute transaction did not succeed: {transaction_hash}"
        );
    }

    let expected_supply = OCOMP_CAPACITY_TRIBUTE_COUNT.to_string();
    let primary = world.validators.primary_port();
    let supply_deadline = Instant::now() + Duration::from_secs(60);
    while world.rpc.supply(primary).as_deref() != Some(expected_supply.as_str()) {
        assert!(
            Instant::now() < supply_deadline,
            "capacity Tributes did not produce total supply {expected_supply}"
        );
        sleep(Duration::from_millis(250));
    }

    let worldwide_day = world
        .state
        .wwd
        .as_deref()
        .expect("capacity WorldwideDay")
        .parse::<u32>()
        .expect("numeric capacity WorldwideDay");
    for port in world.validators.committee_ports() {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(ids) = world.rpc.tributes_by_day(port, worldwide_day) {
                let distinct = ids.iter().collect::<std::collections::BTreeSet<_>>();
                if ids.len() == OCOMP_CAPACITY_TRIBUTE_COUNT
                    && distinct.len() == OCOMP_CAPACITY_TRIBUTE_COUNT
                {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "validator on port {port} did not expose 257 distinct public Tributes"
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
            .mongodb
            .wait_for_tribute_projection(transaction_hash, 60)
            .unwrap_or_else(|error| {
                panic!(
                    "capacity boundary Tribute {transaction_hash} was not projected by every validator: {error}"
                )
            });
    }
}

#[then("Metadosis creates one finalized JobIntent from that public Tribute")]
fn metadosis_creates_finalized_job_intent(world: &mut World) {
    let expected_wwd = world
        .state
        .wwd
        .as_deref()
        .expect("measurement WorldwideDay")
        .parse::<u32>()
        .expect("numeric measurement WorldwideDay");
    let deadline = Instant::now() + Duration::from_secs(240);
    let request = loop {
        let observed = world
            .validators
            .committee_ports()
            .into_iter()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_request_on(port, OCOMP_MEASUREMENT_ACTIVATION_HEIGHT)
            })
            .collect::<Vec<_>>();
        if observed.iter().all(Option::is_some) {
            let first = observed[0].clone().expect("all requests are present");
            assert!(
                observed
                    .iter()
                    .all(|request| request.as_ref() == Some(&first)),
                "validators expose different finalized OCOMP JobIntent requests"
            );
            break first;
        }
        world
            .ocomp
            .ensure_validator_roles_alive()
            .expect("OCOMP roles stay alive while Metadosis reaches the request transition");
        assert!(
            Instant::now() < deadline,
            "Metadosis did not create a finalized public JobIntent in bounded measurement time"
        );
        sleep(Duration::from_millis(500));
    };
    assert_eq!(request.worldwide_day, expected_wwd);
    assert_ne!(request.intent_id, B256::ZERO);
    assert_ne!(request.activation_preconditions_hash, B256::ZERO);
    assert_eq!(
        request.open_height,
        request
            .finality_recorded_height
            .checked_add(4)
            .expect("public finality height admits the fixed depth"),
        "the public voting window must open exactly four blocks after recorded finality"
    );
    assert!(
        request.deadline_height > request.open_height,
        "JobIntent deadline is not exclusive and after its open height"
    );
    world.state.ocomp_job_request = Some(request);
}

#[when("the validator supervisors submit results directly for that finalized JobIntent")]
fn validator_supervisors_submit_results_directly(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("finalized public JobIntent");
    let generation_exists = world
        .validators
        .committee_ports()
        .into_iter()
        .map(|port| {
            world.rpc.nod_certified_generation_exists_on(
                port,
                request.worldwide_day,
                request.request_height,
            )
        })
        .collect::<Vec<_>>();
    assert!(
        generation_exists
            .iter()
            .all(|exists| *exists == Some(false)),
        "Nod generation already exists or cannot be read at the finalized request block: \
         {generation_exists:?}"
    );
    let primary = world.validators.primary_port();
    world.state.ocomp_validator_balances_before = (0..4)
        .map(|validator_index| {
            let key = world
                .validators
                .get(validator_index)
                .evm_key()
                .expect("read validator EVM key");
            let address = eth::address_of(&key).expect("derive validator EVM address");
            let balance = world
                .rpc
                .balance_on(primary, &format!("{address:#x}"))
                .expect("read validator EVM balance before result votes");
            (address, balance)
        })
        .collect();
    world
        .ocomp
        .ensure_validator_roles_alive()
        .expect("validator supervisors remain alive for direct ResultVote submission");
}

#[when("validator 2 prepares one valid vote without broadcasting it")]
fn validator_two_prepares_held_vote(world: &mut World) {
    const VALIDATOR_INDEX: usize = 2;

    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized public JobIntent");
    let primary = world.validators.primary_port();
    let timeout = Instant::now() + Duration::from_secs(180);
    let vote_bytes = loop {
        let finalized_height = world.rpc.finalized(primary).unwrap_or_default();
        let public_votes = world
            .rpc
            .finalized_ocomp_result_vote_transactions_on(
                primary,
                request.request_height,
                finalized_height,
            )
            .unwrap_or_default();
        if let Some(transaction) = public_votes.iter().find(|transaction| transaction.success) {
            break world
                .rpc
                .ocomp_result_vote_bytes_on(primary, transaction.transaction_hash)
                .expect("decode one finalized public ResultVoteV1");
        }
        assert!(
            Instant::now() < timeout,
            "no valid public result vote became available for held-vote preparation"
        );
        sleep(Duration::from_millis(250));
    };
    let vote = ResultVoteV1::decode_canonical(&vote_bytes, &poc_schema_limits())
        .expect("canonical public ResultVoteV1");
    let canonical_result = vote
        .result
        .encode_canonical(&poc_schema_limits())
        .expect("canonical result from the public vote");

    let validator = world.validators.get(VALIDATOR_INDEX);
    let key = validator
        .evm_key()
        .expect("read validator-2 EVM key for address derivation");
    let address = eth::address_of(&key).expect("derive validator-2 EVM address");
    let nonce = world
        .rpc
        .canonical_nonce_on(primary, address)
        .expect("read validator-2 canonical nonce");
    let max_fee_per_gas = world
        .rpc
        .gas_price_on(primary)
        .expect("read public gas price")
        .max(outbe_zerofee::MIN_ZERO_FEE_OCOMP_MAX_FEE_PER_GAS);
    assert!(
        world.rpc.head(primary).unwrap_or_default() < request.deadline_height,
        "held vote was not prepared before the exclusive deadline"
    );
    let prepared = world
        .ocomp
        .prepare_held_vote_transaction(
            VALIDATOR_INDEX as u8,
            canonical_result,
            nonce,
            max_fee_per_gas,
            outbe_zerofee::MAX_ZERO_FEE_OCOMP_GAS_LIMIT,
        )
        .expect("production node prepares validator-2 vote without exposing its key");
    world.state.ocomp_held_late_vote_hash = Some(prepared.transaction_hash);
    world.state.ocomp_held_late_vote_raw = Some(prepared.raw_transaction.0);
}

#[when("the held validator vote is broadcast at the exclusive deadline")]
fn held_vote_is_broadcast_at_deadline(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized public JobIntent");
    let primary = world.validators.primary_port();
    let target_parent = request
        .deadline_height
        .checked_sub(1)
        .expect("deadline follows genesis");
    let timeout = Instant::now() + Duration::from_secs(180);
    while world.rpc.head(primary).unwrap_or_default() < target_parent {
        assert!(
            Instant::now() < timeout,
            "chain did not approach the exclusive vote deadline"
        );
        sleep(Duration::from_millis(50));
    }

    let raw = world
        .state
        .ocomp_held_late_vote_raw
        .take()
        .expect("node-signed held vote transaction");
    let expected_hash = world
        .state
        .ocomp_held_late_vote_hash
        .expect("held vote transaction hash");
    let submitted_hash = world
        .rpc
        .send_raw_transaction_on(primary, &raw)
        .expect("broadcast held vote through public RPC");
    assert_eq!(
        submitted_hash
            .parse::<B256>()
            .expect("public transaction hash"),
        expected_hash,
        "public RPC changed the node-signed held transaction identity"
    );
    let receipt_timeout = Instant::now() + Duration::from_secs(60);
    let receipt = loop {
        if let Some(receipt) = world
            .rpc
            .transaction_receipt(&submitted_hash, primary)
            .filter(|receipt| receipt.get("blockNumber").is_some())
        {
            break receipt;
        }
        assert!(
            Instant::now() < receipt_timeout,
            "held vote did not receive a mined public receipt"
        );
        sleep(Duration::from_millis(100));
    };
    assert_eq!(
        receipt.get("status").and_then(serde_json::Value::as_str),
        Some("0x0"),
        "a vote included at or after the exclusive deadline must revert"
    );
    let inclusion_height = world
        .rpc
        .receipt_block_number(&submitted_hash, primary)
        .expect("held vote inclusion height");
    assert!(
        inclusion_height >= request.deadline_height,
        "held vote was included before the intended exclusive boundary: \
         inclusion={inclusion_height}, deadline={}",
        request.deadline_height
    );
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, inclusion_height, 60),
        "held deadline-vote receipt did not finalize"
    );
    world.state.ocomp_late_vote_reverted = Some(true);
    world.state.ocomp_late_vote_inclusion_height = Some(inclusion_height);
}

#[then("three matching validator domains atomically apply Lysis and create the Nod")]
fn quorum_applies_lysis_and_creates_nod(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized public JobIntent");
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let ports = world.validators.committee_ports();
        let activations = ports
            .iter()
            .copied()
            .map(|port| {
                world.rpc.finalized_ocomp_activation_on(
                    port,
                    request.request_height,
                    request.intent_id,
                )
            })
            .collect::<Vec<_>>();
        if activations.iter().all(Option::is_some) {
            let activation = activations[0].clone().expect("all activations are present");
            assert!(
                activations
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&activation)),
                "validators expose different finalized Lysis activation"
            );
            assert_eq!(activation.intent_id, request.intent_id);
            assert_eq!(activation.worldwide_day, request.worldwide_day);
            assert_ne!(activation.job_id, B256::ZERO);
            assert_ne!(activation.result_digest, B256::ZERO);
            assert_ne!(activation.activation_call_id, B256::ZERO);
            assert_ne!(activation.terminal_receipt_hash, B256::ZERO);

            let generations = ports
                .iter()
                .copied()
                .map(|port| {
                    world
                        .rpc
                        .finalized_ocomp_certified_generation_on(port, &activation)
                })
                .collect::<Vec<_>>();
            assert!(
                generations.iter().all(Option::is_some),
                "one or more validators cannot verify both generation projections at the exact \
                 finalized activation block: {generations:?}"
            );
            let generation = generations[0]
                .clone()
                .expect("all certified generations are present");
            assert!(
                generations
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&generation)),
                "validators expose different certified Nod generations"
            );
            assert_eq!(generation.worldwide_day, request.worldwide_day);
            assert_eq!(generation.job_id, activation.job_id);
            assert_eq!(generation.block_number, activation.block_number);
            assert_eq!(generation.block_hash, activation.block_hash);
            assert_ne!(generation.program_semantics_hash, B256::ZERO);
            assert_ne!(generation.nod_root, B256::ZERO);
            assert_ne!(generation.bucket_root, B256::ZERO);
            assert_ne!(generation.output_manifest_root, B256::ZERO);
            assert_eq!(generation.tribute_count, generation.nod_count);
            assert!(generation.tribute_count > 0);
            assert!(generation.bucket_count <= generation.nod_count);

            let accountability_deadline = Instant::now() + Duration::from_secs(120);
            let accountability = loop {
                let observed = ports
                    .iter()
                    .copied()
                    .map(|port| {
                        world
                            .rpc
                            .finalized_ocomp_vote_accountability_on(port, activation.job_id)
                    })
                    .collect::<Vec<_>>();
                if observed.iter().all(|value| {
                    value.as_ref().is_some_and(|accountability| {
                        accountability.slot_validator_indexes.len() == 4
                    })
                }) {
                    let first = observed[0]
                        .clone()
                        .expect("all accountability records are present");
                    assert!(
                        observed.iter().all(|value| value.as_ref() == Some(&first)),
                        "validators expose different finalized vote accountability"
                    );
                    break first;
                }
                assert!(
                    Instant::now() < accountability_deadline,
                    "the fourth timely validator vote did not reach finalized accountability: \
                     {observed:?}"
                );
                sleep(Duration::from_millis(250));
            };
            assert_eq!(accountability.job_id, activation.job_id);
            assert_eq!(accountability.slot_validator_indexes, vec![0, 1, 2, 3]);
            assert_eq!(
                accountability.quorum_result_digest,
                Some(activation.result_digest)
            );
            assert_eq!(
                accountability
                    .quorum_signer_bitmap
                    .expect("completed job quorum")
                    .count_ones(),
                3
            );

            let finalized_height = ports
                .iter()
                .copied()
                .map(|port| {
                    world
                        .rpc
                        .finalized(port)
                        .expect("validator finalized height")
                })
                .min()
                .expect("four validator ports");
            let public_votes = ports
                .iter()
                .copied()
                .map(|port| {
                    world.rpc.finalized_ocomp_result_vote_transactions_on(
                        port,
                        request.request_height,
                        finalized_height,
                    )
                })
                .collect::<Vec<_>>();
            assert!(
                public_votes.iter().all(Option::is_some),
                "one or more validators cannot enumerate finalized public result votes"
            );
            let first_votes = public_votes[0]
                .clone()
                .expect("all public vote collections are present");
            assert!(
                public_votes
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&first_votes)),
                "proposer/import/replay validators expose different public result-vote transactions"
            );
            assert_eq!(
                first_votes
                    .iter()
                    .filter(|transaction| transaction.success)
                    .count(),
                4,
                "exactly four independent public validator result votes must succeed"
            );
            let mut signers = first_votes
                .iter()
                .map(|transaction| transaction.signer)
                .collect::<Vec<_>>();
            signers.sort_unstable();
            signers.dedup();
            assert_eq!(
                signers.len(),
                4,
                "public result votes must come from four validator EVM signers"
            );
            assert!(
                first_votes
                    .iter()
                    .any(|transaction| transaction.transaction_hash == activation.transaction_hash),
                "q-forming activation transaction is absent from the public result-vote set"
            );

            let primary = world.validators.primary_port();
            let mut balances_after = Vec::with_capacity(4);
            for (address, before) in &world.state.ocomp_validator_balances_before {
                let after = world
                    .rpc
                    .balance_on(primary, &format!("{address:#x}"))
                    .expect("read validator balance after result votes");
                assert_eq!(
                    after, *before,
                    "validator {address:#x} paid for a ZeroFee result vote"
                );
                balances_after.push((*address, after));
            }

            if generation.tribute_count as usize == OCOMP_CAPACITY_TRIBUTE_COUNT {
                let q_forming = first_votes
                    .iter()
                    .find(|transaction| transaction.transaction_hash == activation.transaction_hash)
                    .expect("q-forming public transaction");
                let vote_bytes = world
                    .rpc
                    .ocomp_result_vote_bytes_on(primary, q_forming.transaction_hash)
                    .expect("canonical q-forming ResultVoteV1 bytes");
                let internal_work =
                    outbe_ocomp_protocol::capacity::result_vote_internal_work(vote_bytes.len())
                        .expect("q-forming vote fits generated internal-work cap");
                let finalized_block_hash = world
                    .rpc
                    .block_hash(primary, finalized_height)
                    .and_then(|value| value.parse::<B256>().ok())
                    .expect("finalized capacity capture block hash");
                let q_forming_timestamp = world
                    .rpc
                    .block_timestamp(primary, q_forming.block_number)
                    .expect("q-forming capacity block timestamp");
                let finalized_timestamp = world
                    .rpc
                    .block_timestamp(primary, finalized_height)
                    .expect("finalized capacity capture timestamp");
                let finality_latency_micros = finalized_timestamp
                    .checked_sub(q_forming_timestamp)
                    .and_then(|seconds| seconds.checked_mul(1_000_000))
                    .expect("capacity finality timestamp delta");
                let block_processing_micros_by_validator = ports
                    .iter()
                    .enumerate()
                    .map(|(validator_index, _)| {
                        world
                            .localnet
                            .validator_block_processing_micros(
                                validator_index,
                                q_forming.block_number,
                                q_forming.block_hash,
                            )
                            .unwrap_or_else(|error| {
                                panic!(
                                    "observe q-forming capacity block on validator \
                                     {validator_index}: {error:#}"
                                )
                            })
                    })
                    .collect::<Vec<_>>();
                let block_processing_micros = block_processing_micros_by_validator
                    .iter()
                    .copied()
                    .max()
                    .expect("four validator block-processing timings");
                world.state.ocomp_capacity_observation =
                    Some(crate::world::state::OcompPublicCapacityObservationV1 {
                        job_id: activation.job_id,
                        result_digest: activation.result_digest,
                        q_forming_transaction_hash: q_forming.transaction_hash,
                        q_forming_block_number: q_forming.block_number,
                        q_forming_block_hash: q_forming.block_hash,
                        finalized_block_number: finalized_height,
                        finalized_block_hash,
                        tribute_count: u64::from(generation.tribute_count),
                        nod_count: u64::from(generation.nod_count),
                        worker_shard_count:
                            outbe_ocomp_protocol::capacity::worker_shard_count(
                                u64::from(generation.tribute_count),
                                u32::try_from(
                                    outbe_ocomp_protocol::generated_shape::
                                        OCOMP_POC_CANDIDATE_LIMITS_V1
                                            .max_tributes_per_work_shard,
                                )
                                .expect("generated shard cap fits u32"),
                            )
                            .expect("generated shard cap is non-zero"),
                        transaction_bytes: u64::try_from(q_forming.raw_transaction_len)
                            .expect("q-forming transaction length fits u64"),
                        block_bytes: u64::try_from(q_forming.block_rlp_len)
                            .expect("q-forming block length fits u64"),
                        gas: q_forming.gas_used,
                        internal_work,
                        block_processing_micros_by_validator,
                        block_processing_micros,
                        finality_latency_micros,
                    });
            }

            world.state.ocomp_activation = Some(activation);
            world.state.ocomp_certified_generation = Some(generation);
            world.state.ocomp_result_vote_transactions = first_votes;
            world.state.ocomp_vote_accountability = Some(accountability);
            world.state.ocomp_validator_balances_after = balances_after;
            world.state.ocomp_atomic_quorum_apply_verified = true;
            return;
        }
        world
            .ocomp
            .ensure_validator_roles_alive()
            .expect("OCOMP roles stay alive through public activation");
        assert!(
            Instant::now() < deadline,
            "q=3 public Lysis activation did not finalize before the bounded E2E deadline; \
             activations={activations:?}"
        );
        sleep(Duration::from_millis(500));
    }
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
}

#[then("validator 0 reconstructs that certified generation from canonical history")]
fn validator_zero_reconstructs_certified_generation(world: &mut World) {
    let capacity = world
        .state
        .ocomp_capacity_observation
        .clone()
        .expect("capacity public-path observation");
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("capacity finalized JobIntent");
    let activation = world
        .state
        .ocomp_activation
        .clone()
        .expect("capacity finalized activation");
    let generation = world
        .state
        .ocomp_certified_generation
        .clone()
        .expect("capacity certified generation");
    let recovery = world
        .localnet
        .reconstruct_validator_ce_from_canonical_history(0)
        .unwrap_or_else(|error| {
            panic!("reconstruct validator-0 CE from canonical history: {error:#}")
        });
    assert!(
        recovery.first_missing_block_number <= capacity.q_forming_block_number
            && recovery.target_block_number >= capacity.finalized_block_number,
        "historical CE replay span does not cover the q-forming/finalized capacity blocks: \
         recovery={recovery:?}, capacity={capacity:?}"
    );
    assert_eq!(
        recovery.replayed_block_count,
        recovery.target_block_number - recovery.first_missing_block_number + 1
    );

    let primary = world.validators.primary_port();
    let canonical_target_hash = world
        .rpc
        .block_hash(primary, recovery.target_block_number)
        .and_then(|value| value.parse::<B256>().ok())
        .expect("restarted validator exposes replay target block");
    assert_eq!(
        canonical_target_hash, recovery.target_block_hash,
        "startup replay target is not the restarted validator's canonical block"
    );

    let deadline = Instant::now() + Duration::from_secs(120);
    let (recovered_activation, recovered_generation) = loop {
        let recovered_activation = world.rpc.finalized_ocomp_activation_on(
            primary,
            request.request_height,
            request.intent_id,
        );
        let recovered_generation = recovered_activation.as_ref().and_then(|observed| {
            world
                .rpc
                .finalized_ocomp_certified_generation_on(primary, observed)
        });
        if let (Some(recovered_activation), Some(recovered_generation)) =
            (recovered_activation, recovered_generation)
        {
            break (recovered_activation, recovered_generation);
        }
        assert!(
            Instant::now() < deadline,
            "restarted validator did not expose the recovered certified generation"
        );
        sleep(Duration::from_millis(250));
    };
    assert_eq!(
        recovered_activation, activation,
        "historical CE replay changed the finalized activation"
    );
    assert_eq!(
        recovered_generation, generation,
        "historical CE replay changed the certified generation"
    );
    world.state.ocomp_historical_replay_observation =
        Some(crate::world::state::OcompHistoricalReplayObservationV1 {
            recovery,
            recovered_result_digest: recovered_activation.result_digest,
            recovered_generation,
        });
}

#[when("the completed full-result vote is retried and then mutated through public RPC")]
fn completed_vote_is_retried_and_mutated(world: &mut World) {
    let activation = world
        .state
        .ocomp_activation
        .clone()
        .expect("completed public Lysis activation");
    let primary = world.validators.primary_port();
    let vote_bytes = world
        .rpc
        .ocomp_result_vote_bytes_on(primary, activation.transaction_hash)
        .expect("decode the q-forming public result vote");
    let vote = ResultVoteV1::decode_canonical(&vote_bytes, &poc_schema_limits())
        .expect("canonical q-forming ResultVoteV1");
    assert_eq!(vote.job_id, activation.job_id);

    let retry_hash = world
        .rpc
        .submit_ocomp_result_vote_bytes(primary, &world.validators.get(0), vote_bytes)
        .expect("submit exact completed-vote retry through public RPC");
    let retry_receipt = world
        .rpc
        .transaction_receipt(&retry_hash, primary)
        .expect("exact retry receipt");
    assert_eq!(
        retry_receipt
            .get("status")
            .and_then(serde_json::Value::as_str),
        Some("0x1"),
        "exact completed-vote retry must be idempotently accepted"
    );
    world.state.ocomp_exact_completed_retry_succeeded = Some(true);

    let mut mutated = vote;
    mutated.job_id = B256::repeat_byte(0xa5);
    let mutated_bytes = mutated
        .encode_canonical(&poc_schema_limits())
        .expect("structurally canonical changed-binding vote");
    let mutation_hash = world
        .rpc
        .submit_ocomp_result_vote_bytes(primary, &world.validators.get(0), mutated_bytes)
        .expect("submit changed-binding result vote through public RPC");
    let mutation_receipt = world
        .rpc
        .transaction_receipt(&mutation_hash, primary)
        .expect("changed-binding receipt");
    assert_eq!(
        mutation_receipt
            .get("status")
            .and_then(serde_json::Value::as_str),
        Some("0x0"),
        "changed-binding completed vote must revert in the OCOMP module"
    );
    world.state.ocomp_changed_completed_binding_reverted = Some(true);

    let retry_block = world
        .rpc
        .receipt_block_number(&retry_hash, primary)
        .expect("exact retry block");
    let mutation_block = world
        .rpc
        .receipt_block_number(&mutation_hash, primary)
        .expect("changed-binding block");
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, retry_block.max(mutation_block), 60),
        "public retry/mutation receipts did not finalize"
    );
}

#[then("the completed job and Nod generation are unchanged by both transactions")]
fn completed_job_and_generation_are_unchanged(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("finalized public JobIntent");
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("completed public activation");
    let expected_accountability = world
        .state
        .ocomp_vote_accountability
        .as_ref()
        .expect("four-slot accountability before retry");
    let expected_generation = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .expect("certified generation before retry");

    for port in world.validators.committee_ports() {
        let record = world
            .rpc
            .finalized_ocomp_job_record_on(port, request.intent_id)
            .expect("completed job record after public retry/mutation");
        let completed = record
            .terminal
            .as_ref()
            .and_then(|terminal| terminal.completed_binding.as_ref())
            .expect("completed binding remains present");
        assert_eq!(completed.job_id, activation.job_id);
        assert_eq!(completed.result_digest, activation.result_digest);
        assert_eq!(
            completed.terminal_receipt_hash,
            activation.terminal_receipt_hash
        );

        let accountability = world
            .rpc
            .finalized_ocomp_vote_accountability_on(port, activation.job_id)
            .expect("accountability after public retry/mutation");
        assert_eq!(&accountability, expected_accountability);

        let generation = world
            .rpc
            .finalized_ocomp_certified_generation_on(port, activation)
            .expect("certified generation after public retry/mutation");
        assert_eq!(&generation, expected_generation);
    }
    world.state.ocomp_completed_state_unchanged = Some(true);
}

#[when("validators 2 and 3 OCOMP supervisors are stopped before the job")]
fn stop_two_supervisors_before_job(world: &mut World) {
    let primary = world.validators.primary_port();
    world.state.ocomp_finality_before_fault = world.rpc.finalized(primary);
    for validator_index in [2, 3] {
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopSupervisor { validator_index })
            .unwrap_or_else(|error| {
                panic!("stop validator-{validator_index} Supervisor before the job: {error}")
            });
    }
}

#[when("validators 1, 2 and 3 OCOMP supervisors are stopped before the job")]
fn stop_three_supervisors_before_job(world: &mut World) {
    for validator_index in [1, 2, 3] {
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopSupervisor { validator_index })
            .unwrap_or_else(|error| {
                panic!("stop validator-{validator_index} Supervisor before the job: {error}")
            });
    }
}

#[when("one valid vote is finalized and a changed-binding vote is submitted")]
fn one_valid_then_changed_binding_vote(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized single-voter JobIntent");
    let ports = world.validators.committee_ports();
    let primary = world.validators.primary_port();
    let timeout = Instant::now() + Duration::from_secs(180);
    let (job_id, accountability) = loop {
        let records = ports
            .iter()
            .copied()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_record_on(port, request.intent_id)
            })
            .collect::<Vec<_>>();
        if records.iter().all(|record| {
            record
                .as_ref()
                .is_some_and(|record| record.status == OcompJobStatus::VotingOpen)
        }) {
            let record = records[0].clone().expect("all records are present");
            assert!(
                records
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&record)),
                "validators expose different single-voter job state"
            );
            let job_id = record
                .finalized
                .as_ref()
                .expect("finalized single-voter job")
                .job_id;
            let accountability = world
                .rpc
                .finalized_ocomp_vote_accountability_on(primary, job_id);
            if accountability
                .as_ref()
                .is_some_and(|value| value.slot_validator_indexes == [0])
            {
                eprintln!(
                    "OCOMP_PUBLIC_MUTATION stage=single_vote_observed finalized_height={:?}",
                    world.rpc.finalized(primary)
                );
                break (job_id, accountability.expect("checked above"));
            }
        }
        assert!(
            Instant::now() < timeout,
            "validator-0 did not finalize the sole public result vote before mutation"
        );
        sleep(Duration::from_millis(250));
    };
    assert_eq!(accountability.quorum_result_digest, None);

    let finalized_height = world.rpc.finalized(primary).expect("finalized height");
    eprintln!(
        "OCOMP_PUBLIC_MUTATION stage=scan_public_votes from={} to={finalized_height}",
        request.request_height
    );
    let public_votes = world
        .rpc
        .finalized_ocomp_result_vote_transactions_on(
            primary,
            request.request_height,
            finalized_height,
        )
        .expect("enumerate the sole finalized public vote");
    eprintln!(
        "OCOMP_PUBLIC_MUTATION stage=scan_public_votes_done observed={}",
        public_votes.len()
    );
    let successful = public_votes
        .iter()
        .filter(|transaction| transaction.success)
        .collect::<Vec<_>>();
    assert_eq!(successful.len(), 1, "expected one successful public vote");
    let vote_bytes = world
        .rpc
        .ocomp_result_vote_bytes_on(primary, successful[0].transaction_hash)
        .expect("decode sole public ResultVoteV1");
    let mut mutated = ResultVoteV1::decode_canonical(&vote_bytes, &poc_schema_limits())
        .expect("canonical sole ResultVoteV1");
    assert_eq!(mutated.job_id, job_id);
    mutated.job_id = B256::repeat_byte(0x5a);
    eprintln!("OCOMP_PUBLIC_MUTATION stage=submit_changed_binding");
    let mutation_hash = world
        .rpc
        .submit_ocomp_result_vote_bytes(
            primary,
            &world.validators.get(1),
            mutated
                .encode_canonical(&poc_schema_limits())
                .expect("canonical changed-binding payload"),
        )
        .expect("submit changed-binding public vote");
    eprintln!("OCOMP_PUBLIC_MUTATION stage=changed_binding_receipt tx={mutation_hash}");
    let mutation_receipt = world
        .rpc
        .transaction_receipt(&mutation_hash, primary)
        .expect("changed-binding vote receipt");
    assert_eq!(
        mutation_receipt
            .get("status")
            .and_then(serde_json::Value::as_str),
        Some("0x0"),
        "changed-binding vote must revert"
    );
    world.state.ocomp_non_quorum_changed_binding_reverted = Some(true);
    let mutation_height = world
        .rpc
        .receipt_block_number(&mutation_hash, primary)
        .expect("changed-binding inclusion height");
    eprintln!(
        "OCOMP_PUBLIC_MUTATION stage=wait_changed_binding_finality inclusion_height={mutation_height}"
    );
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, mutation_height, 60),
        "changed-binding vote did not finalize"
    );
    eprintln!(
        "OCOMP_PUBLIC_MUTATION stage=changed_binding_finalized finalized_height={:?}",
        world.rpc.finalized(primary)
    );

    for port in ports {
        let after = world
            .rpc
            .finalized_ocomp_job_record_on(port, request.intent_id)
            .expect("job after changed-binding vote");
        assert_eq!(after.status, OcompJobStatus::VotingOpen);
        assert!(after.terminal.is_none());
        assert_eq!(
            world
                .rpc
                .finalized_ocomp_vote_accountability_on(port, job_id)
                .expect("accountability after changed-binding vote"),
            accountability
        );
        assert_eq!(
            world.rpc.nod_certified_generation_exists_on(
                port,
                request.worldwide_day,
                request.request_height,
            ),
            Some(false)
        );
    }
    world.state.ocomp_non_quorum_state_unchanged = Some(true);
}

#[when("the three stopped supervisors restart and form the remaining quorum")]
fn restart_three_supervisors_for_quorum(world: &mut World) {
    for validator_index in [1, 2, 3] {
        world
            .ocomp
            .restart_supervisor(validator_index)
            .unwrap_or_else(|error| {
                panic!("restart validator-{validator_index} Supervisor for quorum: {error}")
            });
    }
}

#[then("the no-quorum job expires at its exclusive deadline without creating Nod")]
fn no_quorum_job_expires_without_nod(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized no-quorum JobIntent");
    let primary = world.validators.primary_port();
    let timeout = Instant::now() + Duration::from_secs(240);
    let records = loop {
        let finalized = world.rpc.finalized(primary).unwrap_or_default();
        let records = world
            .validators
            .committee_ports()
            .into_iter()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_record_on(port, request.intent_id)
            })
            .collect::<Vec<_>>();
        if finalized >= request.deadline_height
            && records.iter().all(|record| {
                record
                    .as_ref()
                    .is_some_and(|record| record.status == OcompJobStatus::Expired)
            })
        {
            break records;
        }
        world
            .ocomp
            .ensure_validator_roles_alive()
            .expect("remaining OCOMP roles stay alive during no-quorum expiry");
        assert!(
            Instant::now() < timeout,
            "no-quorum JobIntent did not expire at deadline {}: finalized={finalized}, \
             records={records:?}",
            request.deadline_height
        );
        sleep(Duration::from_millis(250));
    };
    let record = records[0].clone().expect("all expired records are present");
    assert!(
        records
            .iter()
            .all(|observed| observed.as_ref() == Some(&record)),
        "validators expose different expired JobIntent state"
    );
    let terminal = record.terminal.expect("expired terminal record");
    assert_eq!(terminal.outcome, OcompTerminalOutcome::Expired);
    assert_eq!(terminal.terminal_height, request.deadline_height);
    assert!(terminal.completed_binding.is_none());
    let finalized = record.finalized.expect("finalized expired job");
    assert!(finalized.quorum.is_none());

    let accountability = world
        .rpc
        .finalized_ocomp_vote_accountability_on(primary, finalized.job_id)
        .expect("closed no-quorum accountability");
    assert_eq!(accountability.slot_validator_indexes, vec![0, 1]);
    assert_eq!(accountability.quorum_result_digest, None);
    assert_eq!(accountability.closed_height, Some(request.deadline_height));
    assert_eq!(accountability.timely_bitmap, Some(0b0011));
    assert_eq!(accountability.missing_bitmap, Some(0b1100));
    assert_eq!(accountability.equivocation_bitmap, Some(0));

    for port in world.validators.committee_ports() {
        assert_eq!(
            world.rpc.nod_certified_generation_exists_on(
                port,
                request.worldwide_day,
                request.request_height,
            ),
            Some(false),
            "expired no-quorum job created a Nod generation on port {port}"
        );
        assert!(
            world
                .rpc
                .finalized_ocomp_activation_on(port, request.request_height, request.intent_id)
                .is_none(),
            "expired no-quorum job emitted a Lysis apply event on port {port}"
        );
    }
    assert!(
        world.rpc.finalized(primary).unwrap_or_default()
            > world.state.ocomp_finality_before_fault.unwrap_or_default(),
        "consensus finality did not advance while two Supervisors were stopped"
    );
    world.state.ocomp_vote_accountability = Some(accountability);
    world.state.ocomp_expired_without_nod = Some(true);
}

#[then("all four OCOMP domains run their node-facing production roles")]
fn four_domains_run_node_facing_roles(world: &mut World) {
    let records = world.ocomp.process_records();
    for validator_index in 0..4_u8 {
        for role in [
            OcompProcessRole::Supervisor,
            OcompProcessRole::SnapshotExporter,
        ] {
            let matches = records
                .iter()
                .filter(|record| {
                    record.validator_index == Some(validator_index)
                        && record.role == role
                        && record.worker_ordinal.is_none()
                        && record.stopped_at_millis.is_none()
                })
                .count();
            assert_eq!(
                matches, 1,
                "validator-{validator_index} must own one live {role:?}"
            );
        }
    }
}

#[then("each OCOMP domain owns one authenticated production worker")]
fn four_domains_own_authenticated_workers(world: &mut World) {
    let records = world.ocomp.process_records();
    assert_eq!(
        records.len(),
        12,
        "expected two node-facing roles and one worker in each domain"
    );
    for validator_index in 0..4_u8 {
        let workers = records
            .iter()
            .filter(|record| {
                record.validator_index == Some(validator_index)
                    && record.role == OcompProcessRole::Worker
                    && record.worker_ordinal == Some(0)
                    && record.stopped_at_millis.is_none()
            })
            .count();
        assert_eq!(
            workers, 1,
            "validator-{validator_index} must own one live authenticated worker"
        );
    }
}

#[when("validator 0 OCOMP supervisor is stopped through the typed fault control")]
fn stop_validator_zero_supervisor(world: &mut World) {
    let primary = world.validators.primary_port();
    world.state.ocomp_finality_before_fault = world.rpc.finalized(primary);
    world
        .ocomp
        .apply_process_fault(OcompProcessFault::StopSupervisor { validator_index: 0 })
        .expect("stop only validator-0 supervisor");
}

#[then("consensus finality advances while only that supervisor remains stopped")]
fn finality_advances_after_supervisor_stop(world: &mut World) {
    let before = world
        .state
        .ocomp_finality_before_fault
        .expect("height captured before OCOMP fault");
    let primary = world.validators.primary_port();
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, before.saturating_add(2), 60),
        "consensus finality did not advance after stopping an OCOMP supervisor"
    );
    let after = world.rpc.finalized(primary).expect("finalized height");
    assert!(after >= before.saturating_add(2));

    let records = world.ocomp.process_records();
    let stopped = records
        .iter()
        .filter(|record| record.stopped_at_millis.is_some())
        .collect::<Vec<_>>();
    assert_eq!(
        stopped.len(),
        1,
        "fault must stop exactly one owned process"
    );
    assert_eq!(stopped[0].validator_index, Some(0));
    assert_eq!(stopped[0].role, OcompProcessRole::Supervisor);
}

#[then("validator 0 OCOMP supervisor restarts through the typed topology")]
fn validator_zero_supervisor_restarts(world: &mut World) {
    world
        .ocomp
        .restart_supervisor(0)
        .expect("restart only validator-0 OCOMP supervisor");
    let records = world.ocomp.process_records();
    let validator_zero_supervisors = records
        .iter()
        .filter(|record| {
            record.validator_index == Some(0)
                && record.role == OcompProcessRole::Supervisor
                && record.worker_ordinal.is_none()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        validator_zero_supervisors.len(),
        2,
        "restart must retain the stopped lifecycle record and add one process"
    );
    assert_eq!(
        validator_zero_supervisors
            .iter()
            .filter(|record| record.stopped_at_millis.is_none())
            .count(),
        1,
        "exactly one validator-0 supervisor must be live after restart"
    );
}

#[when("validator 0 restarts before, across, and after the OCOMP fork height")]
fn validator_zero_restarts_across_fork(world: &mut World) {
    const VALIDATOR_INDEX: usize = 0;
    const ACTIVE_PROTOCOL_VERSION: u64 = 1;

    let activation = OCOMP_MEASUREMENT_ACTIVATION_HEIGHT;
    let primary = world.validators.http_port(VALIDATOR_INDEX);
    let witness = world.validators.http_port(1);
    let pre_fork_restart_from_height = world.rpc.head(primary).expect("validator-0 head");
    assert!(
        pre_fork_restart_from_height + 4 < activation,
        "restart scenario started too close to H: head={pre_fork_restart_from_height}, H={activation}"
    );

    world
        .localnet
        .restart_validator_and_enclave(VALIDATOR_INDEX)
        .expect("restart validator-0 before H");
    let pre_fork_rejoined_height = world
        .rpc
        .wait_block(primary, pre_fork_restart_from_height.saturating_add(1), 20)
        .expect("validator-0 rejoins before H");
    assert!(
        pre_fork_rejoined_height < activation,
        "pre-fork restart rejoined after H: head={pre_fork_rejoined_height}, H={activation}"
    );

    let deadline = Instant::now() + Duration::from_secs(90);
    let down_across_fork_from_height = loop {
        let head = world.rpc.head(witness).expect("witness head before H");
        if head >= activation.saturating_sub(2) {
            assert!(
                head < activation,
                "missed the pre-H shutdown window: head={head}, H={activation}"
            );
            break head;
        }
        assert!(
            Instant::now() < deadline,
            "witness did not approach OCOMP activation height {activation}"
        );
        sleep(Duration::from_millis(100));
    };

    world
        .localnet
        .kill_validator(VALIDATOR_INDEX)
        .expect("keep validator-0 down across H");
    assert!(
        world
            .rpc
            .wait_finalized_at_least(witness, activation.saturating_add(1), 60),
        "three live validators did not finalize through H while validator-0 was down"
    );
    let finalized_while_down_height = world.rpc.finalized(witness).expect("witness finality");

    world
        .localnet
        .restart()
        .expect("restart validator-0 after the network finalized H");
    let replayed_through_height = world
        .rpc
        .wait_block(primary, finalized_while_down_height, 60)
        .expect("validator-0 replays through the finalized activation block");
    assert_eq!(
        world.rpc.active_version_on(primary),
        Some(ACTIVE_PROTOCOL_VERSION),
        "restarted validator did not replay the protocol-v1 activation"
    );
    assert_eq!(
        world.rpc.active_version_on(witness),
        Some(ACTIVE_PROTOCOL_VERSION),
        "live committee did not activate protocol v1 at H"
    );

    let post_fork_restart_from_height = world.rpc.head(primary).expect("post-H validator-0 head");
    assert!(post_fork_restart_from_height >= activation);
    world
        .localnet
        .restart_validator_and_enclave(VALIDATOR_INDEX)
        .expect("restart validator-0 after H");
    let post_fork_rejoined_height = world
        .rpc
        .wait_block(primary, post_fork_restart_from_height.saturating_add(1), 30)
        .expect("validator-0 rejoins after H");
    assert_eq!(
        world.rpc.active_version_on(primary),
        Some(ACTIVE_PROTOCOL_VERSION),
        "post-H restart lost the activated protocol version"
    );
    world
        .ocomp
        .ensure_validator_roles_alive()
        .expect("node-facing OCOMP roles survive validator restarts");
    world
        .ocomp
        .record_fork_restart_evidence(OcompForkRestartEvidenceV1 {
            validator_index: VALIDATOR_INDEX as u8,
            activation_height: activation,
            pre_fork_restart_from_height,
            pre_fork_rejoined_height,
            down_across_fork_from_height,
            finalized_while_down_height,
            replayed_through_height,
            post_fork_restart_from_height,
            post_fork_rejoined_height,
            active_protocol_version: ACTIVE_PROTOCOL_VERSION,
        })
        .expect("retain validated fork restart evidence");
}

#[then("the OCOMP evidence records successful H-1, H, and H+1 recovery")]
fn fork_restart_evidence_is_closed(world: &mut World) {
    let snapshot = world
        .ocomp
        .evidence_snapshot()
        .expect("build OCOMP restart evidence snapshot");
    snapshot
        .validate()
        .expect("validate OCOMP restart evidence");
    assert!(
        snapshot.fork_restart.is_some(),
        "restart scenario must retain exact fork-height observations"
    );
}

#[when("validator 0 restarts with a different valid immutable OCOMP fork install")]
fn validator_zero_restarts_with_mismatched_fork_install(world: &mut World) {
    const VALIDATOR_INDEX: usize = 0;
    const ACTIVE_PROTOCOL_VERSION: u64 = 1;

    let primary = world.validators.http_port(VALIDATOR_INDEX);
    let witness = world.validators.http_port(1);
    let canonical_head_before_restart = world.rpc.head(primary).expect("validator-0 head");
    assert!(
        canonical_head_before_restart < OCOMP_MEASUREMENT_ACTIVATION_HEIGHT,
        "fork mismatch must be installed before H"
    );

    let mismatched = world
        .ocomp
        .prepare_mismatched_fork_manifest(
            VALIDATOR_INDEX as u8,
            OCOMP_MEASUREMENT_ACTIVATION_HEIGHT.saturating_add(1),
        )
        .expect("create a valid same-genesis manifest with a distinct install");
    world
        .localnet
        .restart_validator_with_chain_manifest(VALIDATOR_INDEX, mismatched.path.clone())
        .expect("restart validator-0 with its mismatched immutable fork install");

    assert!(
        world.rpc.wait_finalized_at_least(
            witness,
            OCOMP_MEASUREMENT_ACTIVATION_HEIGHT.saturating_add(1),
            60,
        ),
        "the three canonical validators did not finalize through H"
    );
    let canonical_finalized_after_fork = world
        .rpc
        .finalized(witness)
        .expect("canonical finality after H");
    let mismatched_head_after_fork = world.rpc.head(primary).expect("mismatched validator head");
    assert!(
        mismatched_head_after_fork < OCOMP_MEASUREMENT_ACTIVATION_HEIGHT,
        "mismatched validator imported the canonical activation block"
    );
    let canonical_active_protocol_version = world
        .rpc
        .active_version_on(witness)
        .expect("canonical active protocol version");
    let mismatched_active_protocol_version = world
        .rpc
        .active_version_on(primary)
        .expect("mismatched validator active protocol version");
    assert_eq!(
        canonical_active_protocol_version, ACTIVE_PROTOCOL_VERSION,
        "canonical committee did not activate protocol v1"
    );
    assert_eq!(
        mismatched_active_protocol_version, 0,
        "mismatched validator crossed the canonical Update boundary"
    );

    world
        .ocomp
        .record_fork_mismatch_evidence(OcompForkMismatchEvidenceV1 {
            validator_index: VALIDATOR_INDEX as u8,
            canonical_install_hash: format!("{:#x}", mismatched.canonical_install_hash),
            mismatched_install_hash: format!("{:#x}", mismatched.mismatched_install_hash),
            canonical_activation_height: mismatched.canonical_activation_height,
            mismatched_activation_height: mismatched.mismatched_activation_height,
            canonical_head_before_restart,
            mismatched_head_after_fork,
            canonical_finalized_after_fork,
            canonical_active_protocol_version,
            mismatched_active_protocol_version,
        })
        .expect("retain validated fork mismatch evidence");
}

#[then("the canonical committee finalizes through H while the mismatched validator stays before H")]
fn fork_mismatch_is_fail_closed(world: &mut World) {
    let snapshot = world
        .ocomp
        .evidence_snapshot()
        .expect("build OCOMP fork mismatch evidence snapshot");
    snapshot
        .validate()
        .expect("validate OCOMP fork mismatch evidence");
    assert!(
        snapshot.fork_mismatch.is_some(),
        "fork mismatch scenario must retain exact install and height observations"
    );
}
