use crate::features::ocomp::*;

// Keep real-SGX offers inside the genesis-bound phase window; the controlled
// logical clock advances after the entire population has finalized.
const METADOSIS_CAPACITY_OFFERING_SECONDS: u64 = 3_600;

// A WWD begins at 10:00 UTC on the previous civil date (UTC+14 midnight), while
// the block-1 bootstrap derives its first key from the raw UTC civil date.
// Starting 15 hours into the WWD places block 1 at 01:00 UTC on that same key:
// both date conventions select the fixture WWD and it remains inside FORMING.
const METADOSIS_INITIAL_WWD_ELAPSED_SECS: u64 = 15 * 3_600;

#[given("a fresh four-validator OCOMP measurement localnet")]
fn fresh_ocomp_measurement_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, None, None, false);
}

#[given("a fresh four-validator OCOMP public measurement localnet")]
fn fresh_ocomp_public_measurement_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, Some(0), None, false);
}

#[given("a fresh four-validator OCOMP short-window public measurement localnet")]
fn fresh_ocomp_short_window_public_measurement_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, Some(0), Some(6), false);
}

#[given("a fresh four-validator OCOMP short-window public recovery localnet")]
fn fresh_ocomp_short_window_public_recovery_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, Some(0), Some(6), true);
}

#[given("a fresh four-validator OCOMP public capacity localnet")]
fn fresh_ocomp_public_capacity_localnet(world: &mut World) {
    start_ocomp_measurement_localnet(world, Some(OCOMP_CAPACITY_TRIBUTE_COUNT), None, false);
}

#[given("a fresh four-validator OCOMP dynamic-membership localnet with two scheduled jobs")]
fn fresh_ocomp_dynamic_membership_localnet(world: &mut World) {
    bootstrap_localnet(
        world,
        6,
        &[
            (
                "TESTNET_EPOCH_LENGTH_BLOCKS",
                OCOMP_TEST_EPOCH_LENGTH_BLOCKS.to_string(),
            ),
            (
                "TESTNET_DKG_PREPARE_WINDOW_BLOCKS",
                OCOMP_DYNAMIC_DKG_PREPARE_WINDOW_BLOCKS.to_string(),
            ),
            (
                "TESTNET_OCOMP_VOTE_WINDOW_BLOCKS",
                OCOMP_DYNAMIC_VOTE_WINDOW_BLOCKS.to_string(),
            ),
            ("TESTNET_DEV_FELONY_THRESHOLD", "10".to_owned()),
        ],
    );
    let now_secs = unix_time_secs();
    let mut start_opts = StartOpts::near_next_utc_day_with_lead(6, now_secs, 180);
    let offset = start_opts
        .unix_time_offset_secs
        .expect("dynamic membership clock offset");
    world
        .localnet
        .shift_genesis_timestamp(offset)
        .expect("shift dynamic OCOMP genesis before deriving fork identity");
    start_opts.genesis_timestamp_pre_shifted = true;

    let prepared = world
        .ocomp
        .prepare_dynamic_membership_fork_install()
        .expect("prepare two public jobs around the real membership boundary");
    world.state.ocomp_dynamic_worldwide_days = vec![
        prepared.first_worldwide_day.value(),
        prepared.second_worldwide_day.value(),
    ];
    world.state.ocomp_dynamic_processing_times = vec![
        prepared.first_processing_time,
        prepared.second_processing_time,
    ];
    world
        .localnet
        .bind_tee_genesis()
        .expect("bind canonical TEE genesis after dynamic OCOMP manifest");
    launch_prepared_ocomp(world, &mut start_opts, &prepared.fork, true);
    wait_for_finalized_ocomp_activation(world);
}

#[given("a fresh four-validator Metadosis capacity localnet at FORMING")]
fn fresh_metadosis_capacity_localnet_at_forming(world: &mut World) {
    fresh_metadosis_capacity_localnet_with_window(world, None);
}

#[given(
    expr = "a fresh four-validator Metadosis capacity localnet at FORMING with a {int}-block OCOMP vote window"
)]
fn fresh_metadosis_replay_localnet_at_forming(world: &mut World, window: u64) {
    fresh_metadosis_capacity_localnet_with_window(world, Some(window));
}

fn fresh_metadosis_capacity_localnet_with_window(world: &mut World, window: Option<u64>) {
    let mut tuning = vec![
        (
            "TESTNET_EPOCH_LENGTH_BLOCKS",
            OCOMP_TEST_EPOCH_LENGTH_BLOCKS.to_string(),
        ),
        (
            "TESTNET_METADOSIS_FORMING_SECONDS",
            METADOSIS_FRESH_FORMING_SECONDS.to_string(),
        ),
        (
            "TESTNET_METADOSIS_OFFERING_SECONDS",
            METADOSIS_CAPACITY_OFFERING_SECONDS.to_string(),
        ),
    ];
    // This scenario completes two jobs, retires V1 and restarts the entire
    // cohort before repeating its vote. Keep both positive replays inside
    // the immutable genesis window; do not weaken the production deadline.
    if let Some(window) = window {
        tuning.push(("TESTNET_OCOMP_VOTE_WINDOW_BLOCKS", window.to_string()));
    }
    bootstrap_localnet(world, 6, &tuning);
    let wwd = world
        .state
        .wwd
        .as_deref()
        .expect("fresh Metadosis WorldwideDay")
        .parse::<WorldwideDay>()
        .expect("valid fresh Metadosis WorldwideDay");
    let now_secs = unix_time_secs();
    let initial_timestamp = wwd
        .start_timestamp()
        .checked_add(METADOSIS_INITIAL_WWD_ELAPSED_SECS)
        .expect("fresh Metadosis initial logical time");
    assert_eq!(
        WorldwideDay::from_timestamp(initial_timestamp),
        wwd,
        "UTC+14 timestamp mapping must select the fixture WWD"
    );
    assert_eq!(
        WorldwideDay::from_timestamp(initial_timestamp.saturating_sub(14 * 3_600)),
        wwd,
        "block-1 UTC date mapping must select the fixture WWD"
    );
    let initial_offset = logical_time_offset(initial_timestamp, now_secs);
    world
        .localnet
        .shift_genesis_timestamp(initial_offset)
        .expect("shift fresh Metadosis genesis before deriving fork identity");
    world.state.metadosis_fresh_initial_timestamp = Some(initial_timestamp);
    world.state.metadosis_fresh_initial_unix_time_offset_secs = Some(initial_offset);

    let (prepared, private_keys) = world
        .ocomp
        .prepare_fresh_metadosis_capacity_fork_install(OCOMP_CAPACITY_TRIBUTE_COUNT)
        .expect("prepare runtime-created fresh Metadosis capacity fork");
    world.state.ocomp_capacity_tribute_private_keys = private_keys;
    let mut start_opts = StartOpts {
        voting_window: Some(6),
        unix_time_offset_secs: Some(initial_offset),
        genesis_timestamp_pre_shifted: true,
        is_txpool_eviction_profile: false,
    };
    launch_prepared_ocomp(world, &mut start_opts, &prepared, true);
    wait_for_finalized_ocomp_activation(world);
}

fn start_ocomp_measurement_localnet(
    world: &mut World,
    public_capacity_tribute_count: Option<usize>,
    vote_window_blocks: Option<u64>,
    seed_recovery_day: bool,
) {
    assert!(
        !seed_recovery_day || public_capacity_tribute_count == Some(0),
        "the second recovery WWD belongs only to the one-Tribute public fixture"
    );
    let shorten_public_day = public_capacity_tribute_count.is_some();
    let mut tuning = vec![(
        "TESTNET_EPOCH_LENGTH_BLOCKS",
        OCOMP_TEST_EPOCH_LENGTH_BLOCKS.to_string(),
    )];
    if let Some(window) = vote_window_blocks {
        tuning.push(("TESTNET_OCOMP_VOTE_WINDOW_BLOCKS", window.to_string()));
    }
    bootstrap_localnet(world, 6, &tuning);
    let mut start_opts = if shorten_public_day {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after unix epoch")
            .as_secs();
        let boundary_lead_secs = if public_capacity_tribute_count == Some(0) {
            OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS
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
        Some(0) if seed_recovery_day => world
            .ocomp
            .prepare_public_recovery_fork_install()
            .expect("publish the immutable two-day public recovery fork before node launch"),
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
    if let Some(worldwide_day) = measurement_fork.public_worldwide_day {
        world.state.wwd = Some(worldwide_day.to_string());
    }
    world
        .localnet
        .bind_tee_genesis()
        .expect("bind canonical TEE genesis after installing the mandatory OCOMP manifest");
    launch_prepared_ocomp(world, &mut start_opts, &measurement_fork, true);
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
    assert!(
        world.state.ocomp_activation_height.is_none(),
        "scenario already selected an immutable OCOMP activation height"
    );
    world.state.ocomp_activation_height = Some(prepared.install.activation_height);
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
        .install_ocomp_delegate_bindings()
        .expect("install distinct role-scoped OCOMP transaction signers");
    world
        .ocomp
        .start_validator_roles(identity)
        .expect("start all production node-facing OCOMP roles");
    if activate_workers {
        for validator_index in 0..world.validators.size() {
            let validator_index = u8::try_from(validator_index)
                .expect("configured validator index fits the OCOMP harness wire format");
            world
                .ocomp
                .activate_worker(validator_index, 0, identity)
                .unwrap_or_else(|error| {
                    panic!("activate validator-{validator_index} production worker: {error}")
                });
        }
    }
}

#[then("every OCOMP transaction signer is distinct and scoped only to the OCOMP role")]
fn every_ocomp_transaction_signer_is_role_scoped(world: &mut World) {
    world
        .ocomp
        .verify_ocomp_delegate_bindings()
        .expect("verify distinct role-scoped OCOMP transaction signers");
}

#[then("all four OCOMP domains run their node-facing production roles")]
fn four_domains_run_node_facing_roles(world: &mut World) {
    let counts = world
        .ocomp
        .ensure_baseline_runtime_ready(1)
        .expect("Node-owned OCOMP endpoints, exporters and workers are ready");
    assert_eq!(counts.supervisors, 4);
    assert_eq!(counts.snapshot_exporters, 4);
    let records = world.ocomp.process_records();
    for validator_index in 0..4_u8 {
        let role = OcompProcessRole::SnapshotExporter;
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

#[then("all four OCOMP domains use the production basedir contract")]
fn four_domains_use_production_basedir(world: &mut World) {
    world
        .ocomp
        .verify_release_basedir_contract()
        .expect("all release OCOMP roles use the scenario basedir contract");
}

#[then("each OCOMP domain owns one authenticated production worker")]
fn four_domains_own_authenticated_workers(world: &mut World) {
    let records = world.ocomp.process_records();
    assert_eq!(
        records.len(),
        8,
        "expected one external RPC exporter and one worker for each Node-owned Supervisor"
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
