//! Explicit L2 registration prerequisites for Tribute offer fixtures.
//!
//! Small fixtures use validator governance; bulk owner sets are seeded in
//! genesis. Both store the deterministic per-chain root-signing key used by
//! their real offer proofs.

use std::collections::BTreeSet;
use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::Address;

use crate::internal::addresses::L2_REGISTRY_ADDR;
use crate::internal::l2_fixture;
use crate::world::validators::Validator;
use crate::world::World;

/// Keep governed fixture ids disjoint from genesis-seeded bulk operator ids.
const GOVERNED_FIXTURE_CHAIN_ID_END: u64 = 0xE2E0_FFFF;

/// EOA behind one operator key used by a CLI or raw-ABI offer path.
pub(super) fn operator_address(world: &World, key: &str) -> Address {
    world
        .rpc
        .address_of(key)
        .expect("operator address")
        .parse()
        .expect("operator address hex")
}

/// Register each operator key that is about to submit a Tribute offer.
pub(super) fn ensure_tribute_offer_operators(world: &mut World, keys: &[String]) {
    let addresses: Vec<Address> = keys
        .iter()
        .map(|key| operator_address(world, key))
        .collect();
    ensure_registered_operators(world, &addresses);
}

/// Register the single operator key that is about to submit a Tribute offer.
pub(super) fn ensure_tribute_offer_operator(world: &mut World, key: &str) {
    let address = operator_address(world, key);
    ensure_registered_operators(world, &[address]);
}

/// Register missing operators; existing entries must carry the fixture key.
fn ensure_registered_operators(world: &mut World, l1_addresses: &[Address]) {
    for l1_address in l1_addresses.iter().copied().collect::<BTreeSet<_>>() {
        let mut chain_id = world
            .rpc
            .l2_chain_by_l1_address(l1_address)
            .expect("read the L2Registry mapping for the offer operator");
        if chain_id == 0 {
            chain_id = world.state.l2_next_governed_chain_id;
            assert!(
                chain_id <= GOVERNED_FIXTURE_CHAIN_ID_END,
                "governed fixture chain ids exhausted their reserved namespace"
            );
            world.state.l2_next_governed_chain_id =
                chain_id.checked_add(1).expect("harness L2 chain id space");
            let payload = registration_payload(l1_address, chain_id);
            govern_l2_registry_payload(world, &payload);
        }
        assert_registered_operator(world, l1_address, chain_id);
    }
}

/// Check the operator mapping and the key used to verify its offer roots.
fn assert_registered_operator(world: &World, l1_address: Address, chain_id: u64) {
    assert_eq!(
        world
            .rpc
            .l2_chain_by_l1_address(l1_address)
            .expect("read the L2Registry mapping for the offer operator"),
        chain_id,
        "operator {l1_address:#x} is not registered as chain {chain_id}"
    );
    let (registered_address, public_key) = world
        .rpc
        .l2_network(chain_id)
        .expect("read the registered L2 network");
    assert_eq!(
        registered_address, l1_address,
        "chain {chain_id} is not owned by the operator that offers"
    );
    assert_eq!(
        public_key,
        l2_fixture::root_signing_public_key(chain_id),
        "chain {chain_id} is registered under a different key than the fixture signs with"
    );
}

/// Propose `payload` to L2Registry and drive it to `approved`, returning the
/// proposal id the vote module allocated.
///
/// Used by the governed zk-gate scenarios: the registration must be approved and
/// observable before the scenario's offers run, so it is driven to its deadline
/// here rather than left pending.
pub(super) fn govern_l2_registry_payload(world: &mut World, payload: &str) -> u64 {
    let proposer = active_validators(world)
        .into_iter()
        .next()
        .expect("at least one ACTIVE validator can propose");
    let proposal_id = propose_l2_registry(world, &proposer, payload);
    await_l2_proposal_visible(world, proposal_id);

    // The approvals must land inside this proposal's voting window, which is a
    // handful of blocks wide, so they are cast before anything else is proposed.
    let approvals = cast_l2_approvals(world, &active_validators(world), proposal_id);
    await_l2_approval_tally(world, proposal_id, approvals);
    await_l2_voting_deadline(world, proposal_id);

    assert!(
        world
            .rpc
            .wait_vote_status(proposal_id, "approved", 60)
            .expect("observe the L2 registry proposal approval"),
        "L2 registry proposal #{proposal_id} was not approved"
    );
    proposal_id
}

/// The live ACTIVE validators this harness can send transactions from.
fn active_validators(world: &World) -> Vec<Validator> {
    let port = world.validators.primary_port();
    let active: BTreeSet<Address> = world
        .rpc
        .active_validators(port)
        .expect("read the ACTIVE validator set")
        .into_iter()
        .collect();
    let validators: Vec<Validator> = (0..=world.validators.size())
        .map(|index| world.validators.get(index))
        .filter(|validator| {
            // The configured committee may include a joiner whose identity is
            // not provisioned yet; it cannot propose or vote anyway.
            validator
                .evm_key()
                .ok()
                .and_then(|key| world.rpc.address_of(&key))
                .and_then(|hex| hex.parse::<Address>().ok())
                .is_some_and(|address| active.contains(&address))
        })
        .collect();
    assert!(
        !validators.is_empty(),
        "no ACTIVE validator key is available to govern the L2 registry"
    );
    validators
}

/// Register the same fixture key that signs the offer's Merkle root.
fn registration_payload(l1_address: Address, chain_id: u64) -> String {
    serde_json::json!({
        "operation": "register",
        "chainId": chain_id,
        "l1Address": format!("{l1_address:#x}"),
        "publicKey": format!("0x{}", hex::encode(l2_fixture::root_signing_public_key(chain_id))),
    })
    .to_string()
}

/// Submit one L2Registry proposal from `proposer` and read back the id the vote
/// module allocated to it.
fn propose_l2_registry(world: &World, proposer: &Validator, payload: &str) -> u64 {
    let operator = world
        .validators
        .operator(&format!("validator-{}", proposer.index))
        .expect("proposer operator");
    let target = format!("{L2_REGISTRY_ADDR:#x}");
    let tx = world
        .rpc
        .send_propose(&operator, &target, payload)
        .expect("submit L2 registry proposal");
    assert!(
        world.rpc.wait_tx(&tx, 40),
        "L2 registry proposal tx not mined: {tx}"
    );
    world
        .rpc
        .proposal_id_from_receipt(world.validators.primary_port(), &tx)
        .expect("read the proposal id the vote module allocated")
}

/// Wait until the freshly submitted proposal is observable and still pending.
fn await_l2_proposal_visible(world: &World, proposal_id: u64) {
    for _ in 0..10 {
        let proposal = world
            .rpc
            .vote_status(proposal_id)
            .expect("observe L2 registry proposal");
        if proposal.visible {
            assert_eq!(
                proposal.status, "pending",
                "L2 registry proposal #{proposal_id} is not pending"
            );
            return;
        }
        sleep(Duration::from_secs(2));
    }
    panic!("L2 registry proposal #{proposal_id} is not observable");
}

/// Cast one approval per ACTIVE validator for `proposal_id` and report the
/// number of ballots cast.
///
/// The quorum is measured against the ACTIVE set, which is wider than the
/// configured committee while a joiner is promoted, so the ballots come from
/// every active validator the harness holds a key for rather than a fixed
/// subset that could fall short of quorum.
fn cast_l2_approvals(world: &World, voters: &[Validator], proposal_id: u64) -> u64 {
    for validator in voters {
        world
            .rpc
            .cast_vote(validator, proposal_id, true)
            .expect("cast L2 registry vote");
    }
    voters.len() as u64
}

/// Wait for all cast approvals, or a proposal already approved at its deadline.
fn await_l2_approval_tally(world: &World, proposal_id: u64, approvals: u64) {
    for _ in 0..10 {
        let proposal = world
            .rpc
            .vote_status(proposal_id)
            .expect("observe L2 registry proposal votes");
        assert_eq!(
            proposal.target,
            format!("{L2_REGISTRY_ADDR:#x}"),
            "proposal #{proposal_id} does not target L2Registry"
        );
        if proposal.yes == approvals || proposal.status == "approved" {
            return;
        }
        assert_eq!(
            proposal.status, "pending",
            "L2 registry proposal #{proposal_id} failed before approval"
        );
        sleep(Duration::from_secs(2));
    }
    let proposal = world
        .rpc
        .vote_status(proposal_id)
        .expect("observe L2 registry proposal votes");
    panic!(
        "proposal #{proposal_id} tallied {} of {approvals} cast approvals",
        proposal.yes
    );
}

/// Advance the chain past proposal `proposal_id`'s voting deadline.
///
/// `process_begin_block` only tallies a proposal once its deadline has passed,
/// so the approval - and the L2Registry mutation it applies - is unobservable
/// until then.
fn await_l2_voting_deadline(world: &World, proposal_id: u64) {
    let proposal = world
        .rpc
        .vote_status(proposal_id)
        .expect("observe L2 registry proposal deadline");
    let deadline = proposal.deadline.expect("proposal deadline");
    let height = world
        .rpc
        .wait_block_gt(world.validators.primary_port(), deadline, 80)
        .expect("chain progress past the L2 registry proposal deadline");
    assert!(
        height > deadline,
        "did not pass L2 registry proposal deadline {deadline}"
    );
}
