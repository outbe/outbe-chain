//! Live controller precompile against a test-owned local ISM.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{eip191_hash_message, keccak256, Address, Bytes, B256, U256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::sol;
use cucumber::{given, then, when};
use outbe_primitives::addresses::HYPERLANE_CONTROLLER_ADDRESS;

use crate::internal::eth;
use crate::world::forge::{self, DEPLOYER_KEY};
use crate::world::localnet::{BootstrapProfile, StartOpts};
use crate::world::World;

sol!("../../contracts/precompiles/src/IHyperlaneController.sol");

sol! {
    interface IFixtureIsm {
        function owner() external view returns (address);
        function transferOwnership(address next) external;
        function validatorsAndThreshold(bytes calldata message) external view returns (address[] memory, uint8);
    }
}

const SIGNER_KEY: &str = "0x000000000000000000000000000000000000000000000000000000000000000a";
const WINDOW: u64 = 150;

#[derive(Debug)]
pub(crate) struct HyperlaneFixture {
    ism: Address,
    hook: Address,
    domain: u32,
    validators: Vec<Address>,
    keys: Vec<String>,
    rotated: Address,
    next_window: u64,
    silent_validator_pid: u32,
}

fn url(world: &World) -> String {
    world.rpc.url(world.validators.primary_port())
}

fn send<C: alloy_sol_types::SolCall>(url: &str, to: Address, key: &str, call: &C) {
    eth::send_call(url, to, key, call, None::<U256>).expect("finalize Hyperlane fixture call");
}

fn ism_state_at(url: &str, ism: Address, height: u64) -> (Vec<Address>, u8) {
    let result = eth::read_call_at_result(
        url,
        ism,
        &IFixtureIsm::validatorsAndThresholdCall {
            message: Bytes::new(),
        },
        height,
    )
    .expect("read local ISM state");
    (result._0, result._1)
}

fn assert_ism(
    world: &World,
    ism: Address,
    expected: &[Address],
    threshold: u8,
    live_validators: usize,
) {
    let primary = world.validators.primary_port();
    let height = world.rpc.head(primary).expect("local ISM observation head");
    let ports = (0..live_validators)
        .map(|index| world.validators.http_port(index))
        .collect::<Vec<_>>();
    for &port in &ports {
        assert!(
            world.rpc.wait_finalized_at_least(port, height, 120),
            "validator RPC {port} did not finalize ISM observation height {height}"
        );
    }
    let checkpoint = world
        .rpc
        .checkpoint_at(primary, height)
        .expect("ISM checkpoint");
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    for port in ports {
        let peer = world
            .rpc
            .checkpoint_at(port, height)
            .expect("peer ISM checkpoint");
        assert_eq!(peer.block_hash, checkpoint.block_hash);
        assert_eq!(peer.state_root, checkpoint.state_root);
        let (mut actual, actual_threshold) = ism_state_at(&world.rpc.url(port), ism, height);
        actual.sort_unstable();
        assert_eq!(actual, expected, "local ISM signer set on RPC {port}");
        assert_eq!(
            actual_threshold, threshold,
            "local ISM quorum on RPC {port}"
        );
    }
}

#[given("a live committee with a controller-owned Hyperlane ISM fixture")]
fn live_controller_fixture(world: &mut World) {
    world
        .localnet
        .bootstrap_with_profile(world.validators.size(), &BootstrapProfile::default())
        .expect("bootstrap Hyperlane controller committee");
    crate::features::common::start_bootstrapped_localnet(world, &StartOpts::default());
    let url = url(world);
    let deployer = eth::address_of(DEPLOYER_KEY).expect("fixture deployer address");
    let funder = world
        .validators
        .get(0)
        .evm_key()
        .expect("validator funding key");
    eth::send_value(&url, deployer, &funder, eth::coen(100))
        .expect("fund throwaway Hyperlane fixture deployer");
    let repo = crate::env::environment().repo.join("testing/contracts");
    let ism_output = forge::run(
        &repo,
        &["create", "src/MockHyperlaneController.sol:MockHyperlaneIsm"],
        &[],
        &url,
    )
    .expect("deploy fixture ISM");
    let ism = forge::address_from(&ism_output, "Deployed to:").expect("fixture ISM address");
    let router_output = forge::run_with_ctor(
        &repo,
        &[
            "create",
            "src/MockHyperlaneController.sol:MockHyperlaneIcaRouter",
        ],
        &[&format!("{HYPERLANE_CONTROLLER_ADDRESS:#x}")],
        &[],
        &url,
    )
    .expect("deploy fixture ICA router");
    let router = forge::address_from(&router_output, "Deployed to:").expect("fixture router");
    send(
        &url,
        ism,
        DEPLOYER_KEY,
        &IFixtureIsm::transferOwnershipCall {
            next: HYPERLANE_CONTROLLER_ADDRESS,
        },
    );
    let chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .expect("local Hyperlane domain");
    let domain = u32::try_from(chain_id).expect("Hyperlane domain fits uint32");
    let hook = eth::address_of(DEPLOYER_KEY).expect("fixture hook identity");
    send(
        &url,
        HYPERLANE_CONTROLLER_ADDRESS,
        DEPLOYER_KEY,
        &IHyperlaneController::initializeCall {
            icaRouter: router,
            validatorAnnounce: hook,
            domains: vec![domain],
            isms: vec![ism],
            hooks: vec![hook],
        },
    );
    send(
        &url,
        HYPERLANE_CONTROLLER_ADDRESS,
        DEPLOYER_KEY,
        &IHyperlaneController::syncCall {},
    );
    let keys = (0..world.validators.size())
        .map(|i| {
            world
                .validators
                .get(i)
                .evm_key()
                .expect("validator EOA key")
        })
        .collect::<Vec<_>>();
    let validators = keys
        .iter()
        .map(|key| eth::address_of(key).expect("validator address"))
        .collect::<Vec<_>>();
    assert_eq!(
        validators.len(),
        4,
        "liveness fixture needs four validators"
    );
    let silent_validator_pid = world
        .localnet
        .live_validator_and_enclave_pids(3)
        .expect("capture silent validator before liveness jail")
        .0;
    world.state.hyperlane_controller = Some(HyperlaneFixture {
        ism,
        hook,
        domain,
        validators,
        keys,
        rotated: Address::ZERO,
        next_window: 0,
        silent_validator_pid,
    });
}

#[then("the controller mirrors all active validator signers into the ISM")]
fn initial_ism(world: &mut World) {
    let fixture = world.state.hyperlane_controller.as_ref().expect("fixture");
    assert_ism(world, fixture.ism, &fixture.validators, 3, 4);
    assert_eq!(
        eth::read_call(&url(world), fixture.ism, &IFixtureIsm::ownerCall {}).expect("ISM owner"),
        HYPERLANE_CONTROLLER_ADDRESS
    );
}

fn digest(domain: u32, hook: Address, root: B256, index: u32, message_id: B256) -> B256 {
    let mut domain_input = Vec::with_capacity(45);
    domain_input.extend_from_slice(&domain.to_be_bytes());
    domain_input.extend_from_slice(hook.into_word().as_slice());
    domain_input.extend_from_slice(b"HYPERLANE");
    let domain_hash = keccak256(domain_input);
    let mut checkpoint = Vec::with_capacity(100);
    checkpoint.extend_from_slice(domain_hash.as_slice());
    checkpoint.extend_from_slice(root.as_slice());
    checkpoint.extend_from_slice(&index.to_be_bytes());
    checkpoint.extend_from_slice(message_id.as_slice());
    eip191_hash_message(keccak256(checkpoint))
}

fn signed_checkpoint(
    fixture: &HyperlaneFixture,
    signer_key: &str,
    index: u32,
) -> IHyperlaneController::submitCheckpointCall {
    let root = keccak256(format!("outbe-e2e-hyperlane-root-{index}"));
    let message_id = keccak256(format!("outbe-e2e-hyperlane-message-{index}"));
    let signer: PrivateKeySigner = signer_key.parse().expect("Hyperlane signer key");
    let signature = signer
        .sign_hash_sync(&digest(
            fixture.domain,
            fixture.hook,
            root,
            index,
            message_id,
        ))
        .expect("sign Hyperlane checkpoint");
    IHyperlaneController::submitCheckpointCall {
        domain: fixture.domain,
        root,
        index,
        messageId: message_id,
        signature: Bytes::copy_from_slice(&signature.as_bytes()),
    }
}

#[when("a validator rotates its bridge signing key and submits verified checkpoints")]
fn rotated_checkpoints(world: &mut World) {
    let url = url(world);
    let fixture = world.state.hyperlane_controller.as_mut().expect("fixture");
    let signer: PrivateKeySigner = SIGNER_KEY.parse().expect("rotated signer");
    fixture.rotated = signer.address();
    send(
        &url,
        HYPERLANE_CONTROLLER_ADDRESS,
        &fixture.keys[0],
        &IHyperlaneController::setHyperlaneSignerCall {
            signer: fixture.rotated,
        },
    );
    send(
        &url,
        HYPERLANE_CONTROLLER_ADDRESS,
        DEPLOYER_KEY,
        &IHyperlaneController::syncCall {},
    );
    let invalid = signed_checkpoint(fixture, &fixture.keys[0], 1);
    assert!(
        eth::send_call(
            &url,
            HYPERLANE_CONTROLLER_ADDRESS,
            &fixture.keys[0],
            &invalid,
            None::<U256>
        )
        .is_err(),
        "validator EOA signature bypassed registered bridge signer"
    );
    for index in 0..fixture.keys.len() {
        let signing_key = if index == 0 {
            SIGNER_KEY
        } else {
            &fixture.keys[index]
        };
        send(
            &url,
            HYPERLANE_CONTROLLER_ADDRESS,
            &fixture.keys[index],
            &signed_checkpoint(fixture, signing_key, 1),
        );
    }
    let stale = signed_checkpoint(fixture, SIGNER_KEY, 1);
    assert!(
        eth::send_call(
            &url,
            HYPERLANE_CONTROLLER_ADDRESS,
            &fixture.keys[0],
            &stale,
            None::<U256>
        )
        .is_err(),
        "replayed checkpoint index was accepted"
    );
}

#[then("the ISM reflects the rotated signer and rejects invalid checkpoint signatures")]
fn rotated_ism(world: &mut World) {
    let fixture = world.state.hyperlane_controller.as_ref().expect("fixture");
    let mut expected = fixture.validators.clone();
    expected[0] = fixture.rotated;
    assert_ism(world, fixture.ism, &expected, 3, 4);
    for validator in &fixture.validators {
        assert_eq!(
            eth::read_call(
                &url(world),
                HYPERLANE_CONTROLLER_ADDRESS,
                &IHyperlaneController::submittedIndexCall {
                    validator: *validator,
                    domain: fixture.domain,
                }
            )
            .expect("verified checkpoint index"),
            1
        );
    }
}

#[when("three validators advance their checkpoint while one bridge agent stops")]
fn bridge_agent_outage(world: &mut World) {
    let url = url(world);
    let fixture = world.state.hyperlane_controller.as_mut().expect("fixture");
    for index in 0..3 {
        let signing_key = if index == 0 {
            SIGNER_KEY
        } else {
            &fixture.keys[index]
        };
        send(
            &url,
            HYPERLANE_CONTROLLER_ADDRESS,
            &fixture.keys[index],
            &signed_checkpoint(fixture, signing_key, 2),
        );
    }
    let head = world
        .rpc
        .head(world.validators.primary_port())
        .expect("head");
    fixture.next_window = head.div_ceil(WINDOW) * WINDOW;
    if fixture.next_window.saturating_sub(head) < 35 {
        fixture.next_window += WINDOW;
    }
    assert!(fixture.next_window > head + 30);
}

#[then("three liveness windows jail only the silent validator and resync the ISM")]
fn jailed_and_resynced(world: &mut World) {
    let (ism, validators, rotated, first_window, silent_validator_pid) = {
        let fixture = world.state.hyperlane_controller.as_ref().expect("fixture");
        (
            fixture.ism,
            fixture.validators.clone(),
            fixture.rotated,
            fixture.next_window,
            fixture.silent_validator_pid,
        )
    };
    let primary = world.validators.primary_port();
    let genesis: serde_json::Value = serde_json::from_slice(
        &std::fs::read(world.localnet.scenario_dir().join("genesis.json"))
            .expect("read executed Hyperlane genesis"),
    )
    .expect("parse executed Hyperlane genesis");
    let targets = [
        first_window,
        first_window
            .checked_add(WINDOW)
            .expect("second Hyperlane liveness height overflow"),
        first_window
            .checked_add(2 * WINDOW)
            .expect("third Hyperlane liveness height overflow"),
    ];
    let started = world
        .rpc
        .finalized(primary)
        .expect("starting finalized head");
    let remaining = targets[2].saturating_sub(started);
    let budget = if remaining == 0 {
        Duration::ZERO
    } else {
        super::common::consensus_block_wait_budget(&genesis, remaining)
            .expect("Hyperlane liveness wait budget")
    };
    let deadline = Instant::now()
        .checked_add(budget)
        .expect("Hyperlane liveness deadline overflow");
    for (miss, target) in (1..=3u32).zip(targets) {
        loop {
            let finalized = world.rpc.finalized(primary).expect("finalized head");
            if finalized >= target {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Hyperlane liveness window {miss} did not reach finalized target {target} within {budget:?}: last finalized {finalized}"
            );
            if finalized.is_multiple_of(30) {
                eprintln!("Hyperlane liveness: finalized={finalized} target={target}");
            }
            sleep(Duration::from_secs(2));
        }
        let observed = eth::read_call(
            &url(world),
            HYPERLANE_CONTROLLER_ADDRESS,
            &IHyperlaneController::missCountCall {
                validator: validators[3],
            },
        )
        .expect("silent bridge signer miss count");
        assert_eq!(
            observed,
            if miss == 3 { 0 } else { miss },
            "silent bridge signer window {miss}"
        );
        for validator in &validators[..3] {
            assert_eq!(
                eth::read_call(
                    &url(world),
                    HYPERLANE_CONTROLLER_ADDRESS,
                    &IHyperlaneController::missCountCall {
                        validator: *validator,
                    },
                )
                .expect("active bridge signer miss count"),
                0
            );
        }
    }
    let mut expected = validators[..3].to_vec();
    expected[0] = rotated;
    let jailed = world
        .rpc
        .validator_record(primary, &format!("{:#x}", validators[3]))
        .expect("read silent validator after liveness jail");
    assert_eq!(jailed.status, 6, "only silent signer must be JAILED");
    for validator in &validators[..3] {
        let record = world
            .rpc
            .validator_record(primary, &format!("{validator:#x}"))
            .expect("read healthy validator after liveness jail");
        assert_eq!(record.status, 2, "active signer must remain ACTIVE");
    }
    assert_ism(world, ism, &expected, 2, 3);
    let stalled = world.rpc.finalized(primary).expect("finalized after jail");
    let progress_target = stalled
        .checked_add(5)
        .expect("post-jail finality height overflow");
    let finality_budget = super::common::consensus_block_wait_budget(&genesis, 5)
        .expect("post-jail finality wait budget");
    let finality_retries = u32::try_from(finality_budget.as_millis().div_ceil(2_000))
        .expect("post-jail finality retry count overflow");
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, progress_target, finality_retries),
        "committee did not finalize five blocks after liveness jail within {finality_budget:?}"
    );
    let exit_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (pid, status) = world
            .localnet
            .owned_validator_process(3)
            .expect("observe silent validator's owned process after jail");
        assert_eq!(pid, silent_validator_pid, "silent validator was replaced");
        if let Some(status) = status {
            assert!(
                status.success(),
                "jailed validator did not stop cleanly: {status}"
            );
            break;
        }
        assert!(
            Instant::now() < exit_deadline,
            "jailed validator did not stop"
        );
        sleep(Duration::from_millis(100));
    }
    let log = world
        .localnet
        .node_launch_log(3, silent_validator_pid)
        .expect("read exact jailed validator process log");
    let guard = "ERROR outbe_chain::launch::shutdown: finalized TEE lease guard requested node shutdown reason=validator is jailed; complete ordinary unjail and then run tee join";
    assert_eq!(
        log.lines()
            .filter(|line| line.trim_end().ends_with(guard))
            .count(),
        1,
        "silent validator must stop for exactly the canonical jail reason"
    );
    world.state.expected_tee_lease_guard_shutdown_validator = Some(3);
}
