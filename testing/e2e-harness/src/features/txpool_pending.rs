//! Pending eviction on an ACTIVE proposer, not an isolated FullNode. Canonical
//! notifications drive production maintenance; no pool/state writes are injected.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, U256};
use alloy_signer_local::PrivateKeySigner;
use cucumber::{then, when};
use eyre::{ensure, Result};

use crate::internal::eth;
use crate::world::rpc::TxOutcome;
use crate::world::state::PendingValidatorPoolFixture;
use crate::world::World;

const OWNER: usize = 0;
const STALENESS_SECS: u64 = 20;
// This funded test transaction's fee budget, not a production cap. Every
// observed canonical tip must remain below it; fee eviction cannot pass.
const FIXTURE_MAX_FEE: u128 = 10_000_000_000;

fn require_eligible_account(
    sample: &eth::PoolAccountAtTip,
    fixture: &PendingValidatorPoolFixture,
) -> Result<()> {
    ensure!(
        sample.number > 1,
        "pending fixture requires a post-bootstrap block"
    );
    ensure!(
        sample.gas_limit == fixture.gas_limit,
        "canonical block gas limit changed during pending fixture"
    );
    ensure!(
        u128::from(sample.base_fee) <= fixture.max_fee,
        "pending fixture became fee-ineligible"
    );
    ensure!(
        sample.nonce == fixture.nonce,
        "pending fixture consumed its canonical nonce"
    );
    let required = U256::from(fixture.gas_limit) * U256::from(fixture.max_fee) + U256::from(1);
    ensure!(
        sample.balance >= required,
        "pending fixture lacks its full upfront balance"
    );
    Ok(())
}

fn require_evicted_account(
    sample: &eth::PoolAccountAtTip,
    fixture: &PendingValidatorPoolFixture,
) -> Result<()> {
    require_eligible_account(sample, fixture)?;
    ensure!(
        sample
            .timestamp
            .saturating_sub(fixture.admitted_tip_timestamp)
            >= STALENESS_SECS,
        "candidate disappeared before a canonical staleness interval"
    );
    Ok(())
}

fn require_active_owner(world: &mut World) {
    world
        .localnet
        .ensure_committee_alive()
        .expect("owned committee is alive");
    let ports = world.validators.committee_ports();
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, 2, 60)
        .expect("exact finalized committee checkpoint");
    let key = world
        .validators
        .get(OWNER)
        .evm_key()
        .expect("owner identity");
    let address = eth::address_of(&key).expect("owner address");
    for port in ports {
        let record = world
            .rpc
            .validator_record_at(port, &address.to_string(), checkpoint.height)
            .expect("owner ValidatorSet record at the shared finalized checkpoint");
        assert_eq!(record.address, address, "owner registry identity");
        assert_eq!(record.status, 2, "pending-pool owner must be ACTIVE");
        assert!(record.has_bls_share, "owner must hold its committee share");
        assert!(world
            .rpc
            .is_participant(port, &address.to_string())
            .expect("owner consensus participation read"));
    }
}

#[when("a funded independent sender submits a block-sized transaction to an ACTIVE validator")]
fn submit_active_pending(world: &mut World) {
    require_active_owner(world);
    let signer = PrivateKeySigner::random();
    let sender = signer.address();
    let key_file = world
        .localnet
        .scenario_dir()
        .join("pending-pool-sender.hex");
    let mut key_output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key_file)
        .expect("create private scenario sender key");
    writeln!(
        key_output,
        "{}",
        alloy_primitives::hex::encode(signer.to_bytes())
    )
    .expect("write private scenario sender key");
    let key = fs::read_to_string(&key_file).expect("read scenario sender key");
    let url = world.rpc.url(world.validators.http_port(OWNER));
    let initial = eth::pool_account_at_tip(&url, sender).expect("canonical candidate context");
    assert_eq!(
        initial.gas_limit,
        outbe_primitives::system_tx::STEADY_BLOCK_GAS_LIMIT
    );
    let funder = world
        .validators
        .get(OWNER)
        .evm_key()
        .expect("independent funder key");
    let funding = U256::from(initial.gas_limit) * U256::from(FIXTURE_MAX_FEE) + eth::coen(1);
    let funded = TxOutcome::from(
        eth::send_value_outcome(&url, sender, &funder, funding)
            .expect("fund independent pending sender"),
    );
    world
        .rpc
        .finalize_outcome(&funded, &world.validators.committee_ports(), 60)
        .expect("funding succeeded at a shared finalized checkpoint");
    let sample =
        eth::pool_account_at_tip(&url, sender).expect("funded canonical candidate context");
    let mut fixture = PendingValidatorPoolFixture {
        key_file,
        sender,
        hash: String::new(),
        nonce: sample.nonce,
        gas_limit: initial.gas_limit,
        max_fee: FIXTURE_MAX_FEE,
        admitted_tip_timestamp: sample.timestamp,
        owner_pid: world
            .localnet
            .validator_pid(OWNER)
            .expect("owned pending validator PID"),
    };
    require_eligible_account(&sample, &fixture).expect("funded executable candidate");
    fixture.hash = eth::send_value_with_gas_at_nonce(
        &url,
        Address::repeat_byte(0x7b),
        key.trim(),
        U256::from(1),
        fixture.nonce,
        fixture.gas_limit,
        fixture.max_fee,
    )
    .expect("admit full-block-gas transaction through the real ACTIVE validator RPC");
    assert_eq!(
        world
            .rpc
            .txpool_location(world.validators.http_port(OWNER), &fixture.hash)
            .expect("strict pending admission observation"),
        Some("pending")
    );
    world.state.pending_validator_pool = Some(fixture);
}

#[then("the transaction is pending while an independent transfer finalizes")]
fn pending_does_not_stop_ordinary_traffic(world: &mut World) {
    let fixture = world
        .state
        .pending_validator_pool
        .as_ref()
        .expect("pending fixture");
    let key = world
        .validators
        .get(OWNER)
        .evm_key()
        .expect("progress sender");
    assert_ne!(eth::address_of(&key).unwrap(), fixture.sender);
    let outcome = TxOutcome::from(
        eth::send_value_outcome(
            &world.rpc.url(world.validators.http_port(OWNER)),
            Address::repeat_byte(0x7c),
            &key,
            U256::from(1),
        )
        .expect("ordinary transfer while pending"),
    );
    world
        .rpc
        .finalize_outcome(&outcome, &world.validators.committee_ports(), 60)
        .expect("ordinary transfer succeeded and finalized on every validator");
    assert_eq!(
        world
            .rpc
            .txpool_location(world.validators.http_port(OWNER), &fixture.hash)
            .expect("candidate pending after independent finalized traffic"),
        Some("pending")
    );
}

#[then("canonical snapshots evict the exact pending transaction on its ACTIVE owner")]
fn wait_active_pending_eviction(world: &mut World) {
    let fixture = world
        .state
        .pending_validator_pool
        .as_ref()
        .expect("pending fixture");
    let port = world.validators.http_port(OWNER);
    let ports = world.validators.committee_ports();
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        assert!(
            Instant::now() < deadline,
            "pending eviction never reached all-peer absence with exact owner evidence"
        );
        world
            .localnet
            .ensure_committee_alive()
            .expect("committee alive while waiting for pending eviction");
        let sample = eth::pool_account_at_tip(&world.rpc.url(port), fixture.sender)
            .expect("canonical-tip timestamp and hash-bound account observation");
        require_eligible_account(&sample, fixture)
            .expect("candidate remains funded, nonce-valid and fee-eligible");
        match world
            .rpc
            .txpool_location(port, &fixture.hash)
            .expect("strict pending observation")
        {
            Some("pending") => {}
            None => {
                // Capture owner absence before waiting for peers or log
                // delivery: their later progress cannot bless early eviction.
                // Read after the pool RPC because maintenance may have advanced
                // to another canonical tip since the preceding account read.
                let sample = eth::pool_account_at_tip(&world.rpc.url(port), fixture.sender)
                    .expect("canonical account immediately after owner absence");
                require_evicted_account(&sample, fixture).expect(
                    "owner eviction preserves eligibility and canonical staleness interval",
                );
                // Peers receive the candidate and maintenance notifications at
                // different instants. Wait for their own eviction before the
                // restart fault; never use replacement mining to hide a copy.
                let mut all_absent = true;
                for peer in &ports {
                    match world
                        .rpc
                        .txpool_location(*peer, &fixture.hash)
                        .expect("strict peer observation before pending-owner restart")
                    {
                        None => {}
                        Some("pending") => all_absent = false,
                        other => panic!(
                            "pending candidate changed eligibility on peer {peer}: {other:?}"
                        ),
                    }
                }
                let log =
                    fs::read_to_string(world.localnet.scenario_dir().join("validator-0/node.log"))
                        .expect("read exact pending owner's current scenario log");
                if !all_absent
                    || !log
                        .lines()
                        .any(|line| exact_pending_eviction(line, fixture))
                {
                    sleep(Duration::from_millis(250));
                    continue;
                }
                break;
            }
            other => panic!("candidate left pending without eviction: {other:?}"),
        }
        sleep(Duration::from_millis(250));
    }
    let log = fs::read_to_string(world.localnet.scenario_dir().join("validator-0/node.log"))
        .expect("read exact pending owner's current scenario log");
    assert!(
        log.lines()
            .any(|line| exact_pending_eviction(line, fixture)),
        "owner emitted no exact hash/sender/nonce stale_pending eviction"
    );
    require_active_owner(world);
    require_absent_and_nonce_unconsumed(world);
}

fn exact_pending_eviction(line: &str, fixture: &PendingValidatorPoolFixture) -> bool {
    line.contains("outbe::txpool")
        && line.contains("evicting stale pending transaction")
        && line
            .split_whitespace()
            .any(|field| field == format!("tx_hash={}", fixture.hash))
        && line
            .split_whitespace()
            .any(|field| field == format!("sender={}", fixture.sender))
        && line
            .split_whitespace()
            .any(|field| field == format!("nonce={}", fixture.nonce))
        && line
            .split_whitespace()
            .any(|field| field == "reason=\"stale_pending\"")
        && line
            .split_whitespace()
            .any(|field| field == "staleness_interval_secs=20")
}

fn require_absent_and_nonce_unconsumed(world: &mut World) {
    let fixture = world
        .state
        .pending_validator_pool
        .as_ref()
        .expect("pending fixture");
    let ports = world.validators.committee_ports();
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, 2, 60)
        .expect("all committee RPCs ready at exact checkpoint before nonce replacement");
    for port in ports {
        assert_eq!(
            world
                .rpc
                .txpool_location(port, &fixture.hash)
                .expect("observe absence before replacement"),
            None
        );
        let nonce: alloy_primitives::U64 = serde_json::from_value(
            eth::raw_json_result(
                &world.rpc.url(port),
                "eth_getTransactionCount",
                serde_json::json!([fixture.sender, format!("0x{:x}", checkpoint.height)]),
            )
            .expect("canonical nonce at shared finalized checkpoint"),
        )
        .expect("decode canonical nonce");
        assert_eq!(
            nonce.to::<u64>(),
            fixture.nonce,
            "evicted transaction consumed canonical nonce"
        );
        assert!(
            eth::raw_json_result(
                &world.rpc.url(port),
                "eth_getTransactionReceipt",
                serde_json::json!([fixture.hash])
            )
            .expect("observe missing receipt")
            .is_null(),
            "evicted pending transaction was mined"
        );
    }
}

#[when("the ACTIVE pending-pool owner restarts before any nonce replacement")]
fn restart_active_pending_owner(world: &mut World) {
    require_absent_and_nonce_unconsumed(world);
    let before = world
        .state
        .pending_validator_pool
        .as_ref()
        .unwrap()
        .owner_pid;
    assert_eq!(
        world.localnet.validator_pid(OWNER).unwrap(),
        before,
        "pending owner changed before fault"
    );
    world
        .localnet
        .restart_validator_preserving_enclave(OWNER)
        .expect("restart exact pending owner with preserved argv/datadir/enclave");
    assert_ne!(
        world.localnet.validator_pid(OWNER).unwrap(),
        before,
        "owner process did not restart"
    );
}

#[then("the pending transaction remains absent and its nonce is unconsumed on every validator")]
fn pending_stays_absent_after_owner_restart(world: &mut World) {
    require_active_owner(world);
    require_absent_and_nonce_unconsumed(world);
}

#[then("an explicit same-nonce replacement finalizes and the committee advances two fresh blocks")]
fn replace_active_pending_nonce(world: &mut World) {
    require_absent_and_nonce_unconsumed(world);
    let fixture = world
        .state
        .pending_validator_pool
        .as_ref()
        .expect("pending fixture");
    let url = world.rpc.url(world.validators.http_port(OWNER));
    let key = fs::read_to_string(&fixture.key_file).expect("read preserved test sender key");
    let hash = eth::send_value_at_nonce(
        &url,
        Address::repeat_byte(0x7d),
        key.trim(),
        U256::from(1),
        fixture.nonce,
    )
    .expect("submit explicit same-nonce replacement");
    let deadline = Instant::now() + Duration::from_secs(120);
    let receipt = loop {
        let receipt =
            eth::raw_json_result(&url, "eth_getTransactionReceipt", serde_json::json!([hash]))
                .expect("read replacement receipt");
        if !receipt.is_null() {
            break receipt;
        }
        assert!(Instant::now() < deadline, "replacement never mined");
        sleep(Duration::from_millis(250));
    };
    let outcome = TxOutcome {
        transaction_hash: hash,
        success: true,
        receipt,
    };
    let ports = world.validators.committee_ports();
    world
        .rpc
        .finalize_outcome(&outcome, &ports, 60)
        .expect("exact successful finalized replacement receipt");
    let target = world
        .rpc
        .fresh_finality_target(&ports)
        .expect("post-replacement anchor from every peer");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, target, 60)
        .expect("two fresh exact finalized blocks after repair");
    world
        .localnet
        .ensure_committee_alive()
        .expect("repaired committee remains alive");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PendingValidatorPoolFixture {
        PendingValidatorPoolFixture {
            key_file: "private-scenario-key.hex".into(),
            sender: Address::repeat_byte(0xab),
            hash: format!("{}", alloy_primitives::B256::repeat_byte(0xcd)),
            nonce: 3,
            gas_limit: outbe_primitives::system_tx::STEADY_BLOCK_GAS_LIMIT,
            max_fee: FIXTURE_MAX_FEE,
            admitted_tip_timestamp: 100,
            owner_pid: 1,
        }
    }

    #[test]
    fn pending_fixture_rejects_alternative_account_invalidation_causes() {
        let fixture = fixture();
        let sample = eth::PoolAccountAtTip {
            number: 2,
            timestamp: 100,
            gas_limit: fixture.gas_limit,
            base_fee: fixture.max_fee.try_into().unwrap(),
            balance: U256::from(fixture.gas_limit) * U256::from(fixture.max_fee) + U256::from(1),
            nonce: fixture.nonce,
        };
        require_eligible_account(&sample, &fixture).unwrap();
        for invalid in [
            eth::PoolAccountAtTip {
                number: 1,
                ..sample.clone()
            },
            eth::PoolAccountAtTip {
                gas_limit: sample.gas_limit - 1,
                ..sample.clone()
            },
            eth::PoolAccountAtTip {
                base_fee: sample.base_fee + 1,
                ..sample.clone()
            },
            eth::PoolAccountAtTip {
                balance: sample.balance - U256::from(1),
                ..sample.clone()
            },
            eth::PoolAccountAtTip {
                nonce: sample.nonce + 1,
                ..sample.clone()
            },
        ] {
            assert!(
                require_eligible_account(&invalid, &fixture).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn pending_eviction_requires_exact_transaction_and_reason_fields() {
        let fixture = fixture();
        let line = format!("WARN outbe::txpool: evicting stale pending transaction tx_hash={} sender={} nonce={} reason=\"stale_pending\" staleness_interval_secs=20", fixture.hash, fixture.sender, fixture.nonce);
        assert!(exact_pending_eviction(&line, &fixture));
        for (field, replacement) in [
            (
                format!("tx_hash={}", fixture.hash),
                format!("tx_hash={}0", fixture.hash),
            ),
            (
                format!("sender={}", fixture.sender),
                format!("sender={}0", fixture.sender),
            ),
            ("nonce=3".into(), "nonce=30".into()),
            (
                "reason=\"stale_pending\"".into(),
                "reason=\"descendant_of_stale\"".into(),
            ),
            (
                "staleness_interval_secs=20".into(),
                "staleness_interval_secs=200".into(),
            ),
            ("outbe::txpool".into(), "other::pool".into()),
        ] {
            assert!(!exact_pending_eviction(
                &line.replace(&field, &replacement),
                &fixture
            ));
        }
        assert!(!exact_pending_eviction("", &fixture));
    }

    #[test]
    fn owner_absence_requires_its_own_elapsed_canonical_interval() {
        let fixture = fixture();
        let mut sample = eth::PoolAccountAtTip {
            number: 2,
            timestamp: fixture.admitted_tip_timestamp + STALENESS_SECS,
            gas_limit: fixture.gas_limit,
            base_fee: 1,
            balance: U256::MAX,
            nonce: fixture.nonce,
        };
        require_evicted_account(&sample, &fixture).unwrap();
        sample.timestamp -= 1;
        assert!(require_evicted_account(&sample, &fixture).is_err());
        sample.timestamp = fixture.admitted_tip_timestamp - 1;
        assert!(require_evicted_account(&sample, &fixture).is_err());
    }
}
