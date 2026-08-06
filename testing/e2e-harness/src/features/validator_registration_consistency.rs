//! Registration-, reshared-set-, and P2P-consistency steps for
//! `features/validator_lifecycle_consistency.feature`.
//!
//! Every negative path is submitted as a real transaction to a public
//! precompile. The snapshots intentionally omit participation counters: those
//! counters may legitimately advance while a rejected transaction is mined and
//! are not part of the registry identity/membership transaction under test.

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::{Address, Bytes, B256};
use cucumber::{given, then, when};

use crate::world::localnet::{BootstrapProfile, StartOpts};
use crate::world::rpc::{TxOutcome, ValidatorP2pAddress, ValidatorRecord};
use crate::world::validators::RegistrationIdentity;
use crate::world::World;

const REGISTERED: u8 = 0;
const ACTIVE: u8 = 2;
const UNBONDING: u8 = 4;
const P2P_V1: u8 = 1;

/// Stable public fields of one ValidatorSet record.
///
/// Miss/proposal counters are deliberately excluded because a block produced
/// while a negative transaction is mined may update them independently.
#[derive(Clone, Debug, Eq, PartialEq)]
struct StableValidatorRecord {
    address: Address,
    consensus_pubkey: Bytes,
    stake: alloy_primitives::U256,
    status: u8,
    slash_count: u64,
    joined_at_height: u64,
    deactivated_at_height: u64,
    unbonding_end: u64,
    has_bls_share: bool,
    p2p: ValidatorP2pAddress,
}

impl StableValidatorRecord {
    fn from_public(record: ValidatorRecord, p2p: ValidatorP2pAddress) -> Self {
        Self {
            address: record.address,
            consensus_pubkey: record.consensus_pubkey,
            stake: record.stake,
            status: record.status,
            slash_count: record.slash_count,
            joined_at_height: record.joined_at_height,
            deactivated_at_height: record.deactivated_at_height,
            unbonding_end: record.unbonding_end,
            has_bls_share: record.has_bls_share,
            p2p,
        }
    }
}

/// Public dense-index and identity projection. This is the atomicity boundary
/// used by registration and P2P rejection checks.
#[derive(Clone, Debug, Eq, PartialEq)]
struct RegistryIdentityBundle {
    count: u64,
    validators: Vec<Address>,
    records: Vec<StableValidatorRecord>,
}

/// Identity plus current membership/readiness signals. Direct boundary-facade
/// calls must leave this complete bundle untouched when rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ValidatorStateBundle {
    identity: RegistryIdentityBundle,
    active: Vec<Address>,
    consensus: Vec<Address>,
    pending_set_change: bool,
}

#[derive(Clone)]
struct AtomicAttempt {
    label: String,
    outcome: TxOutcome,
    before: RegistryIdentityBundle,
    after: RegistryIdentityBundle,
}

#[derive(Default)]
struct RegistrationConsistencyState {
    identity: Option<RegistrationIdentity>,
    registry_before: Option<RegistryIdentityBundle>,
    state_before: Option<ValidatorStateBundle>,
    outcome: Option<TxOutcome>,
    mutation: Option<String>,
    attempts: Vec<AtomicAttempt>,
    p2p_key: Option<String>,
    p2p_address: Option<Address>,
    original_p2p: Option<ValidatorP2pAddress>,
    reregistered_identity: Option<RegistrationIdentity>,
}

fn scenario_state() -> MutexGuard<'static, RegistrationConsistencyState> {
    static STATE: OnceLock<Mutex<RegistrationConsistencyState>> = OnceLock::new();
    STATE
        .get_or_init(|| Mutex::new(RegistrationConsistencyState::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn reset_scenario_state() {
    *scenario_state() = RegistrationConsistencyState::default();
}

fn address_string(address: Address) -> String {
    format!("{address:#x}")
}

fn boot_profiled_localnet(world: &mut World, profile: &BootstrapProfile) {
    let committee_size = world.validators.size();
    world.state.voting_window = 6;
    world
        .localnet
        .bootstrap_with_profile(committee_size, profile)
        .expect("bootstrap checked localnet profile");
    world
        .localnet
        .start(&StartOpts::with_voting_window(6))
        .expect("start profiled localnet");
    wait_until_chain_ready(world);
}

fn wait_until_chain_ready(world: &World) {
    if world.localnet.tee_enabled() {
        assert!(
            world
                .rpc
                .wait_bootstrapped(world.localnet.tee_bootstrap_wait_attempts()),
            "TEE chain did not bootstrap"
        );
    } else {
        let port = world.validators.primary_port();
        assert!(
            world.rpc.wait_block(port, 1, 18).is_some(),
            "tee-less chain RPC did not become ready"
        );
    }
}

fn registry_identity_bundle(world: &World, port: u16) -> RegistryIdentityBundle {
    let count = world
        .rpc
        .validator_count(port)
        .expect("read ValidatorSet count");
    let validators = world
        .rpc
        .validators(port)
        .expect("read dense ValidatorSet addresses");
    assert_eq!(
        count as usize,
        validators.len(),
        "public validator count disagrees with dense address list"
    );

    let records = validators
        .iter()
        .enumerate()
        .map(|(offset, address)| {
            let address_text = address_string(*address);
            let by_address = world
                .rpc
                .validator_record(port, &address_text)
                .expect("read validator by address");
            let by_index = world
                .rpc
                .validator_record_by_index(port, offset as u64 + 1)
                .expect("read validator by dense index");
            assert_eq!(
                by_index,
                by_address,
                "dense index {} does not resolve to {}",
                offset + 1,
                address_text
            );
            assert_eq!(
                by_address.address,
                *address,
                "record address disagrees with dense list at index {}",
                offset + 1
            );
            let p2p = world
                .rpc
                .validator_p2p_address(port, &address_text)
                .expect("read validator P2P pair");
            StableValidatorRecord::from_public(by_address, p2p)
        })
        .collect();

    RegistryIdentityBundle {
        count,
        validators,
        records,
    }
}

fn validator_state_bundle(world: &World, port: u16) -> ValidatorStateBundle {
    ValidatorStateBundle {
        identity: registry_identity_bundle(world, port),
        active: world
            .rpc
            .active_validators(port)
            .expect("read status-ACTIVE validators"),
        consensus: world
            .rpc
            .active_consensus_set(port)
            .expect("read current consensus participants"),
        pending_set_change: world
            .rpc
            .has_pending_set_change(port)
            .expect("read pending-set-change signal"),
    }
}

fn fund_identity(world: &World, identity: &RegistrationIdentity) {
    world
        .rpc
        .fund_key(&world.validators.get(0), identity.evm_key(), 100)
        .expect("fund registration identity");
}

fn register_self(world: &World, identity: &RegistrationIdentity) -> TxOutcome {
    world
        .rpc
        .register_validator(
            identity.evm_key(),
            identity.address(),
            identity.bls_public_key(),
            identity.registration_signature(),
        )
        .expect("submit self-registration transaction")
}

fn valid_symmetric_p2p(port: u16) -> Vec<u8> {
    // consensus_p2p v1: symmetric(0), IPv4(4), 127.0.0.1, big-endian port.
    let [hi, lo] = port.to_be_bytes();
    vec![0, 4, 127, 0, 0, 1, hi, lo]
}

// ---- D-01A: proof replay across valid chains -------------------------------

#[given(expr = "a funded validator registration fixture on a valid localnet with chain id {int}")]
fn funded_registration_fixture(world: &mut World, chain_id: u64) {
    reset_scenario_state();
    let profile = BootstrapProfile::default()
        .with_chain_id(chain_id)
        .expect("positive chain id");
    boot_profiled_localnet(world, &profile);
    let port = world.validators.primary_port();
    assert_eq!(
        world.rpc.chain_id(port),
        Some(chain_id),
        "localnet must expose the requested valid chain id"
    );

    let identity = world
        .localnet
        .prepare_registration_identity(world.validators.joiner_index())
        .expect("prepare reusable registration identity");
    fund_identity(world, &identity);
    scenario_state().identity = Some(identity);
}

#[given("the fixture is accepted without changing its identity material")]
fn fixture_is_accepted(world: &mut World) {
    let identity = scenario_state()
        .identity
        .clone()
        .expect("registration identity");
    let public_key_before = identity.bls_public_key().clone();
    let proof_before = identity.registration_signature().clone();
    let evm_key_before = identity.evm_key().to_owned();
    let p2p_secret_before = identity.reth_p2p_secret().to_owned();

    let before = registry_identity_bundle(world, world.validators.primary_port());
    let outcome = register_self(world, &identity);
    assert!(
        outcome.success,
        "first-chain registration fixture must be accepted: {}",
        outcome.transaction_hash
    );
    let port = world.validators.primary_port();
    let record = world
        .rpc
        .validator_record(port, &address_string(identity.address()))
        .expect("registered validator record");
    assert_eq!(record.address, identity.address());
    assert_eq!(record.consensus_pubkey, public_key_before);
    assert_eq!(record.status, REGISTERED);
    assert!(!record.has_bls_share);
    assert_eq!(
        world.rpc.validator_count(port),
        Some(before.count + 1),
        "accepted fixture consumes exactly one dense registry slot"
    );
    assert_eq!(identity.registration_signature(), &proof_before);
    assert_eq!(identity.evm_key(), evm_key_before);
    assert_eq!(identity.reth_p2p_secret(), p2p_secret_before);
}

#[when(expr = "the localnet is re-bootstrapped with valid chain id {int}")]
fn rebootstrap_with_valid_chain_id(world: &mut World, chain_id: u64) {
    let profile = BootstrapProfile::default()
        .with_chain_id(chain_id)
        .expect("positive chain id");
    world
        .localnet
        .rebootstrap_with_profile(world.validators.size(), &profile)
        .expect("re-bootstrap with another checked profile");
    world
        .mongodb
        .reset_projection_state()
        .expect("reset first-chain projection state");
    world
        .localnet
        .start(&StartOpts::with_voting_window(6))
        .expect("start re-bootstrapped localnet");
    wait_until_chain_ready(world);
    assert_eq!(
        world.rpc.chain_id(world.validators.primary_port()),
        Some(chain_id),
        "re-bootstrapped network must expose the second chain id"
    );
    let identity = scenario_state()
        .identity
        .clone()
        .expect("reusable registration identity survived rebuild");
    fund_identity(world, &identity);
}

#[when("the same validator address, BLS public key, and proof are submitted")]
fn submit_same_registration_material(world: &mut World) {
    let identity = scenario_state()
        .identity
        .clone()
        .expect("replay registration identity");
    let before = registry_identity_bundle(world, world.validators.primary_port());
    let outcome = register_self(world, &identity);
    let mut state = scenario_state();
    state.registry_before = Some(before);
    state.outcome = Some(outcome);
}

#[then("the cross-chain proof replay is rejected without changing the registry identity bundle")]
fn cross_chain_replay_is_rejected(world: &mut World) {
    let (identity, before, outcome) = {
        let state = scenario_state();
        (
            state.identity.clone().expect("replay identity"),
            state.registry_before.clone().expect("pre-replay bundle"),
            state.outcome.clone().expect("replay outcome"),
        )
    };
    let port = world.validators.primary_port();
    let after = registry_identity_bundle(world, port);
    let absent = world
        .rpc
        .is_validator(port, &address_string(identity.address()))
        == Some(false);
    let mut violations = Vec::new();
    if outcome.success {
        violations.push(format!(
            "cross-chain replay transaction {} was accepted",
            outcome.transaction_hash
        ));
    }
    if after != before {
        violations.push("registry identity/index bundle changed".to_owned());
    }
    if !absent {
        violations.push("replayed address acquired a validator record".to_owned());
    }
    assert!(
        violations.is_empty(),
        "D-01A target invariant failed: {}",
        violations.join("; ")
    );
}

// ---- D-01B / S-02 / S-03: configured owner paths ---------------------------

#[given(expr = "a fresh localnet whose configured ValidatorSet owner is {string}")]
fn localnet_with_configured_owner(world: &mut World, owner_name: String) {
    reset_scenario_state();
    let owner_index = world
        .validators
        .by_name(&owner_name)
        .expect("configured owner name")
        .index;
    let profile = BootstrapProfile::default().with_validator_set_owner(owner_index);
    boot_profiled_localnet(world, &profile);
}

#[given("a funded unregistered validator identity")]
fn funded_unregistered_identity(world: &mut World) {
    let identity = world
        .localnet
        .prepare_registration_identity(world.validators.joiner_index())
        .expect("prepare unregistered identity");
    fund_identity(world, &identity);
    let before = registry_identity_bundle(world, world.validators.primary_port());
    let mut state = scenario_state();
    state.identity = Some(identity);
    state.registry_before = Some(before);
}

#[when("the owner submits its registration with an empty possession proof")]
fn owner_submits_empty_proof_registration(world: &mut World) {
    let identity = scenario_state()
        .identity
        .clone()
        .expect("owner-registration identity");
    let owner_key = world
        .validators
        .get(0)
        .evm_key()
        .expect("configured owner key");
    let outcome = world
        .rpc
        .register_validator(
            &owner_key,
            identity.address(),
            identity.bls_public_key(),
            &[],
        )
        .expect("submit owner registration without PoP");
    scenario_state().outcome = Some(outcome);
}

#[then("owner registration is rejected without changing the registry identity bundle")]
fn owner_registration_without_pop_is_rejected(world: &mut World) {
    let (identity, before, outcome) = {
        let state = scenario_state();
        (
            state.identity.clone().expect("owner-registration identity"),
            state
                .registry_before
                .clone()
                .expect("owner-registration bundle"),
            state.outcome.clone().expect("owner-registration outcome"),
        )
    };
    let port = world.validators.primary_port();
    let after = registry_identity_bundle(world, port);
    let absent = world
        .rpc
        .is_validator(port, &address_string(identity.address()))
        == Some(false);
    let mut violations = Vec::new();
    if outcome.success {
        violations.push(format!(
            "owner no-PoP transaction {} was accepted",
            outcome.transaction_hash
        ));
    }
    if after != before {
        violations.push("registry identity/index bundle changed".to_owned());
    }
    if !absent {
        violations.push("no-PoP identity acquired a validator record".to_owned());
    }
    assert!(
        violations.is_empty(),
        "D-01B target invariant failed: {}",
        violations.join("; ")
    );
}

#[given("the public validator state bundle is snapshotted")]
fn snapshot_public_validator_state_bundle(world: &mut World) {
    // S-02 needs a real REGISTERED, unconfirmed member. Preparing it here also
    // gives S-03 an untouched candidate whose status must survive malformed
    // facade calls.
    let identity = world
        .localnet
        .prepare_registration_identity(world.validators.joiner_index())
        .expect("prepare registered unconfirmed member");
    fund_identity(world, &identity);
    let registration = register_self(world, &identity);
    assert!(
        registration.success,
        "registered-unconfirmed setup transaction failed: {}",
        registration.transaction_hash
    );
    let port = world.validators.primary_port();
    assert_eq!(
        world
            .rpc
            .validator_record(port, &address_string(identity.address()))
            .map(|record| record.status),
        Some(REGISTERED),
        "direct-activation fixture must remain REGISTERED and unconfirmed"
    );
    let bundle = validator_state_bundle(world, port);
    let mut state = scenario_state();
    state.identity = Some(identity);
    state.state_before = Some(bundle);
}

#[when("the owner directly activates a registered unconfirmed member with an arbitrary group hash")]
fn owner_directly_activates_unconfirmed_member(world: &mut World) {
    let identity = scenario_state()
        .identity
        .clone()
        .expect("registered unconfirmed member");
    let port = world.validators.primary_port();
    let mut requested = world
        .rpc
        .active_validators(port)
        .expect("current active validators");
    requested.push(identity.address());
    let owner_key = world.validators.get(0).evm_key().expect("owner key");
    let outcome = world
        .rpc
        .activate_reshared_set(&owner_key, &requested, B256::from([0xa5; 32]))
        .expect("submit direct activation");
    scenario_state().outcome = Some(outcome);
}

#[then("direct activation is rejected with the validator state bundle unchanged")]
fn direct_activation_is_rejected(world: &mut World) {
    let (before, outcome) = {
        let state = scenario_state();
        (
            state
                .state_before
                .clone()
                .expect("pre-activation state bundle"),
            state.outcome.clone().expect("direct-activation outcome"),
        )
    };
    let after = validator_state_bundle(world, world.validators.primary_port());
    let mut violations = Vec::new();
    if outcome.success {
        violations.push(format!(
            "raw activation transaction {} was accepted",
            outcome.transaction_hash
        ));
    }
    if after != before {
        violations.push("public status/share/membership/index bundle changed".to_owned());
    }
    assert!(
        violations.is_empty(),
        "S-02 target invariant failed: {}",
        violations.join("; ")
    );
}

#[when(expr = "the owner submits a reshared set with {string}")]
fn owner_submits_malformed_reshared_set(world: &mut World, mutation: String) {
    let port = world.validators.primary_port();
    let mut requested = world
        .rpc
        .active_validators(port)
        .expect("current active validators");
    assert!(
        requested.len() >= 2,
        "malformed-set fixture needs at least two active validators"
    );
    let hash = match mutation.as_str() {
        "a duplicate active member" => {
            requested.push(requested[0]);
            B256::from([0xd1; 32])
        }
        "non-canonical member order" => {
            requested.swap(0, 1);
            B256::from([0xd2; 32])
        }
        "a mismatched active hash" => B256::from([0xd3; 32]),
        other => panic!("unknown reshared-set mutation: {other}"),
    };
    let owner_key = world.validators.get(0).evm_key().expect("owner key");
    let outcome = world
        .rpc
        .activate_reshared_set(&owner_key, &requested, hash)
        .expect("submit malformed reshared set");
    let mut state = scenario_state();
    state.mutation = Some(mutation);
    state.outcome = Some(outcome);
}

#[then("malformed reshared-set activation is rejected with the validator state bundle unchanged")]
fn malformed_reshared_set_is_rejected(world: &mut World) {
    let (mutation, before, outcome) = {
        let state = scenario_state();
        (
            state.mutation.clone().expect("mutation label"),
            state
                .state_before
                .clone()
                .expect("pre-activation state bundle"),
            state.outcome.clone().expect("malformed activation outcome"),
        )
    };
    let after = validator_state_bundle(world, world.validators.primary_port());
    let mut violations = Vec::new();
    if outcome.success {
        violations.push(format!(
            "malformed activation transaction {} was accepted",
            outcome.transaction_hash
        ));
    }
    if after != before {
        violations.push("public status/share/membership/index bundle changed".to_owned());
    }
    assert!(
        violations.is_empty(),
        "S-03 target invariant failed for {mutation}: {}",
        violations.join("; ")
    );
}

// ---- S-09: versioned P2P pair atomicity and lifecycle cleanup ---------------

#[given("a registered validator has a valid P2P version and payload")]
fn registered_validator_has_valid_p2p_pair(world: &mut World) {
    reset_scenario_state();
    assert!(
        world.localnet.tee_enabled(),
        "S-09 cleanup uses the real join/DKG/exit path and requires @tee"
    );
    let profile = BootstrapProfile::default()
        .with_validator_set_owner(0)
        .with_reregistration_cooldown_blocks(0)
        .with_dkg_timing(60, 15, 20)
        .expect("valid short DKG timing")
        .with_staking_timing(Some(8), Some(8))
        .expect("valid short unbonding timing");
    boot_profiled_localnet(world, &profile);

    let index = world.validators.joiner_index();
    world
        .localnet
        .provision_joiner(index)
        .expect("provision registered joiner with P2P identity");
    let key = world.validators.joiner().evm_key().expect("joiner key");
    let address: Address = world
        .rpc
        .address_of(&key)
        .expect("derive joiner address")
        .parse()
        .expect("parse joiner address");
    let pair = world
        .rpc
        .validator_p2p_address(world.validators.primary_port(), &address_string(address))
        .expect("read provisioned P2P pair");
    assert_eq!(pair.version, P2P_V1);
    assert!(!pair.encoded.is_empty(), "valid P2P payload is empty");

    let mut state = scenario_state();
    state.p2p_key = Some(key);
    state.p2p_address = Some(address);
    state.original_p2p = Some(pair);
}

#[when("invalid-version, malformed-payload, and unauthorized P2P updates are submitted")]
fn invalid_p2p_updates_are_submitted(world: &mut World) {
    let (key, address, original) = {
        let state = scenario_state();
        (
            state.p2p_key.clone().expect("P2P validator key"),
            state.p2p_address.expect("P2P validator address"),
            state.original_p2p.clone().expect("original P2P pair"),
        )
    };
    let port = world.validators.primary_port();
    let mut attempts = Vec::new();

    let before = registry_identity_bundle(world, port);
    let invalid_version = world
        .rpc
        .set_validator_p2p_address(&key, address, 2, &original.encoded)
        .expect("submit unsupported P2P version");
    let after = registry_identity_bundle(world, port);
    attempts.push(AtomicAttempt {
        label: "unsupported P2P version".to_owned(),
        outcome: invalid_version,
        before,
        after,
    });

    let before = registry_identity_bundle(world, port);
    let malformed = world
        .rpc
        .set_validator_p2p_address(&key, address, P2P_V1, &[0xff])
        .expect("submit malformed P2P payload");
    let after = registry_identity_bundle(world, port);
    attempts.push(AtomicAttempt {
        label: "malformed P2P payload".to_owned(),
        outcome: malformed,
        before,
        after,
    });

    let unrelated_key = world.validators.get(1).evm_key().expect("unrelated key");
    let before = registry_identity_bundle(world, port);
    let unauthorized = world
        .rpc
        .set_validator_p2p_address(
            &unrelated_key,
            address,
            P2P_V1,
            &valid_symmetric_p2p(30_405),
        )
        .expect("submit unauthorized P2P update");
    let after = registry_identity_bundle(world, port);
    attempts.push(AtomicAttempt {
        label: "unauthorized P2P update".to_owned(),
        outcome: unauthorized,
        before,
        after,
    });

    scenario_state().attempts = attempts;
}

#[then("every update is rejected and the original P2P pair remains intact on every node")]
fn invalid_p2p_updates_leave_pair_intact(world: &mut World) {
    let (address, original, attempts) = {
        let state = scenario_state();
        (
            state.p2p_address.expect("P2P validator address"),
            state.original_p2p.clone().expect("original P2P pair"),
            state.attempts.clone(),
        )
    };
    let mut violations = Vec::new();
    for attempt in &attempts {
        if attempt.outcome.success {
            violations.push(format!(
                "{} transaction {} was accepted",
                attempt.label, attempt.outcome.transaction_hash
            ));
        }
        if attempt.after != attempt.before {
            violations.push(format!(
                "{} changed the registry identity/P2P bundle",
                attempt.label
            ));
        }
    }
    for port in world.validators.committee_ports() {
        let observed = world
            .rpc
            .validator_p2p_address(port, &address_string(address));
        if observed.as_ref() != Some(&original) {
            violations.push(format!(
                "node on RPC port {port} reports {observed:?}, expected {original:?}"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "S-09 rejected-update invariant failed: {}",
        violations.join("; ")
    );
}

#[when("the validator completes cleanup and re-registration")]
fn validator_completes_cleanup_and_reregistration(world: &mut World) {
    let (key, address) = {
        let state = scenario_state();
        (
            state.p2p_key.clone().expect("P2P validator key"),
            state.p2p_address.expect("P2P validator address"),
        )
    };
    let primary = world.validators.primary_port();
    let index = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(index);

    world
        .localnet
        .launch_joiner(index, &[])
        .expect("launch registered joiner");
    let caught_up = world.rpc.wait_block(joiner_port, 20, 40).unwrap_or(0);
    assert!(
        caught_up >= 20,
        "registered joiner did not catch up before activation"
    );
    world.rpc.stake(&key, 1_000).expect("stake joiner");
    assert_eq!(
        world
            .rpc
            .validator_status(primary, &address_string(address)),
        Some(1),
        "staked joiner must be PENDING before confirmation"
    );
    world
        .rpc
        .confirm_ready(&key)
        .expect("confirm joiner readiness");
    assert!(
        world
            .rpc
            .wait_participant(primary, &address_string(address), 70),
        "joiner did not become a consensus participant"
    );
    assert_eq!(
        world
            .rpc
            .validator_status(primary, &address_string(address)),
        Some(u64::from(ACTIVE)),
        "joiner did not become ACTIVE"
    );

    // Generate the replacement key while the old identity is still live. Only
    // the EOA is inherited; the BLS key and PoP are freshly generated.
    let eoa_source = RegistrationIdentity::new(
        address,
        key.clone(),
        String::new(),
        Bytes::new(),
        Bytes::new(),
        String::new(),
    );
    let replacement = world
        .localnet
        .rotate_registration_bls(index + 16, &eoa_source)
        .expect("prepare same-EOA replacement BLS identity");

    world
        .rpc
        .deactivate(&key)
        .expect("deactivate active joiner");
    let mut exclusion_height = None;
    for _ in 0..45 {
        if world
            .rpc
            .validator_status(primary, &address_string(address))
            == Some(u64::from(UNBONDING))
        {
            exclusion_height = world.rpc.head(primary);
            break;
        }
        sleep(Duration::from_secs(10));
    }
    assert_eq!(
        world
            .rpc
            .validator_status(primary, &address_string(address)),
        Some(u64::from(UNBONDING)),
        "exclusion boundary did not move validator to UNBONDING"
    );
    assert!(
        world.rpc.wait_finalized_at_least(
            primary,
            exclusion_height.unwrap_or_default().saturating_add(1),
            30,
        ),
        "post-boundary stake-drain block did not finalize"
    );
    for _ in 0..30 {
        if world
            .rpc
            .stake_on(primary, &address_string(address))
            .is_some_and(|stake| stake.is_zero())
        {
            break;
        }
        sleep(Duration::from_secs(1));
    }
    assert!(
        world
            .rpc
            .stake_on(primary, &address_string(address))
            .is_some_and(|stake| stake.is_zero()),
        "UNBONDING validator retained bonded stake"
    );

    let unbonding_end = world
        .rpc
        .validator_record(primary, &address_string(address))
        .expect("UNBONDING record")
        .unbonding_end;
    assert!(unbonding_end > 0, "unbonding claim has no maturity");
    for _ in 0..30 {
        if world
            .rpc
            .latest_block_timestamp(primary)
            .is_some_and(|timestamp| timestamp >= unbonding_end)
        {
            break;
        }
        sleep(Duration::from_secs(2));
    }
    assert!(
        world
            .rpc
            .latest_block_timestamp(primary)
            .is_some_and(|timestamp| timestamp >= unbonding_end),
        "unbonding claim did not mature"
    );

    let [claim, registration] = world
        .rpc
        .claim_unbonded_then_register(
            &key,
            address,
            replacement.bls_public_key(),
            replacement.registration_signature(),
        )
        .expect("submit claim followed by same-EOA re-registration");
    assert!(
        claim.success,
        "final claim transaction failed: {}",
        claim.transaction_hash
    );
    assert!(
        registration.success,
        "same-EOA re-registration failed: {}",
        registration.transaction_hash
    );
    if let Some(block) = registration.block_number() {
        assert!(
            world.rpc.wait_finalized_at_least(primary, block, 30),
            "re-registration block did not finalize"
        );
    }
    world
        .localnet
        .stop_joiner(index)
        .expect("stop re-registered joiner with obsolete local BLS key");
    scenario_state().reregistered_identity = Some(replacement);
}

#[then("both P2P fields are cleared together on every node")]
fn p2p_pair_is_cleared_after_reregistration(world: &mut World) {
    let (address, replacement) = {
        let state = scenario_state();
        (
            state.p2p_address.expect("re-registered validator address"),
            state
                .reregistered_identity
                .clone()
                .expect("replacement identity"),
        )
    };
    let mut violations = Vec::new();
    for port in world.validators.committee_ports() {
        let address_text = address_string(address);
        let pair = world.rpc.validator_p2p_address(port, &address_text);
        if !pair
            .as_ref()
            .is_some_and(|pair| pair.version == 0 && pair.encoded.is_empty())
        {
            violations.push(format!(
                "node on RPC port {port} reports non-cleared P2P pair {pair:?}"
            ));
        }
        match world.rpc.validator_record(port, &address_text) {
            Some(record)
                if record.status == REGISTERED
                    && record.consensus_pubkey == *replacement.bls_public_key()
                    && !record.has_bls_share => {}
            other => violations.push(format!(
                "node on RPC port {port} reports unexpected re-registration record {other:?}"
            )),
        }
    }
    assert!(
        violations.is_empty(),
        "S-09 cleanup invariant failed: {}",
        violations.join("; ")
    );
}

// ---- S-14: rejected registration atomicity ---------------------------------

#[given("a valid localnet with capacity for exactly one additional validator")]
fn localnet_with_one_registration_slot(world: &mut World) {
    reset_scenario_state();
    let capacity =
        u32::try_from(world.validators.size() + 1).expect("test committee capacity fits u32");
    let profile = BootstrapProfile::default()
        .with_max_validators(capacity)
        .expect("valid one-slot registry capacity");
    boot_profiled_localnet(world, &profile);
    assert_eq!(
        world.rpc.validator_count(world.validators.primary_port()),
        Some(u64::from(capacity - 1)),
        "genesis committee must leave exactly one registry slot"
    );
}

#[when("invalid-PoP, duplicate-key, and over-capacity registrations are attempted")]
fn rejected_registration_variants_are_attempted(world: &mut World) {
    let port = world.validators.primary_port();
    let next = world.validators.joiner_index();
    let first = world
        .localnet
        .prepare_registration_identity(next)
        .expect("prepare first candidate");
    let duplicate_eoa = world
        .localnet
        .prepare_registration_identity(next + 1)
        .expect("prepare duplicate-key candidate EOA");
    let duplicate = world
        .localnet
        .rebind_registration_bls(next + 2, &duplicate_eoa, &first)
        .expect("bind first candidate's BLS key to another EOA");
    let over_capacity = world
        .localnet
        .prepare_registration_identity(next + 3)
        .expect("prepare over-capacity candidate");
    for identity in [&first, &duplicate, &over_capacity] {
        fund_identity(world, identity);
    }

    let mut attempts = Vec::new();
    let before = registry_identity_bundle(world, port);
    let invalid_pop = world
        .rpc
        .register_validator(
            first.evm_key(),
            first.address(),
            first.bls_public_key(),
            &[0; 96],
        )
        .expect("submit invalid-PoP registration");
    let after = registry_identity_bundle(world, port);
    attempts.push(AtomicAttempt {
        label: "invalid PoP".to_owned(),
        outcome: invalid_pop.clone(),
        before,
        after,
    });

    // Fill the one valid slot. If invalid PoP was unexpectedly accepted, it
    // already filled that slot; retaining that state lets the final Then report
    // the target failure instead of aborting during setup.
    if !invalid_pop.success {
        let accepted = register_self(world, &first);
        assert!(
            accepted.success,
            "valid capacity-filling registration failed: {}",
            accepted.transaction_hash
        );
    }

    let before = registry_identity_bundle(world, port);
    let duplicate_key = register_self(world, &duplicate);
    let after = registry_identity_bundle(world, port);
    attempts.push(AtomicAttempt {
        label: "duplicate BLS key".to_owned(),
        outcome: duplicate_key,
        before,
        after,
    });

    let before = registry_identity_bundle(world, port);
    let at_capacity = register_self(world, &over_capacity);
    let after = registry_identity_bundle(world, port);
    attempts.push(AtomicAttempt {
        label: "over-capacity registration".to_owned(),
        outcome: at_capacity,
        before,
        after,
    });

    let mut state = scenario_state();
    state.identity = Some(first);
    state.reregistered_identity = Some(duplicate);
    state.p2p_address = Some(over_capacity.address());
    state.attempts = attempts;
}

#[then("every rejected registration leaves count, indexes, key ownership, and candidate records unchanged")]
fn rejected_registrations_are_atomic(world: &mut World) {
    let (first, duplicate, over_capacity, attempts) = {
        let state = scenario_state();
        (
            state.identity.clone().expect("capacity-filling identity"),
            state
                .reregistered_identity
                .clone()
                .expect("duplicate-key identity"),
            state.p2p_address.expect("over-capacity address"),
            state.attempts.clone(),
        )
    };
    let port = world.validators.primary_port();
    let mut violations = Vec::new();
    for attempt in &attempts {
        if attempt.outcome.success {
            violations.push(format!(
                "{} transaction {} was accepted",
                attempt.label, attempt.outcome.transaction_hash
            ));
        }
        if attempt.after != attempt.before {
            violations.push(format!(
                "{} changed count, dense indexes, key records, or candidate records",
                attempt.label
            ));
        }
    }

    let first_record = world
        .rpc
        .validator_record(port, &address_string(first.address()));
    if !first_record.as_ref().is_some_and(|record| {
        record.address == first.address()
            && record.consensus_pubkey == *first.bls_public_key()
            && record.status == REGISTERED
    }) {
        violations.push("the valid key owner record was not preserved".to_owned());
    }
    if world
        .rpc
        .is_validator(port, &address_string(duplicate.address()))
        != Some(false)
    {
        violations.push("duplicate-key candidate acquired a validator record".to_owned());
    }
    if world.rpc.is_validator(port, &address_string(over_capacity)) != Some(false) {
        violations.push("over-capacity candidate acquired a validator record".to_owned());
    }
    assert!(
        violations.is_empty(),
        "S-14 target invariant failed: {}",
        violations.join("; ")
    );
}
