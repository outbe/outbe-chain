//! L2 registry registration of the operators that submit Tribute offers.
//!
//! `TributeFactory` only admits a caller L2Registry knows about: a registered
//! network with `zk_enabled == false` passes the non-ZK path, a registered
//! network with `zk_enabled == true` must carry a signed, whitelisted proof,
//! and an unregistered caller reverts. Every fixture that expects a Tribute
//! receipt therefore has to establish the registration precondition before it
//! offers: either through real validator governance (small fixtures and the
//! governance-focused zk-gate scenarios), or - where a fixture offers from tens
//! or hundreds of distinct owners and cannot pay a governance window per owner
//! inside its genesis-bound OFFERING window - seeded into the not-yet-started
//! genesis by the fixture itself.
//!
//! This module owns that precondition. It registers an operator exactly once
//! per scenario, accepts an operator whose registration the scenario already
//! established (the bulk-owner fixtures seed theirs into genesis; the zk-gate
//! scenarios register chain `0xdead` with `zk_enabled = true`), and proves the
//! canonical mapping the offer guard will read instead of assuming it.
//!
//! Two vote-module rules shape the orchestration: a validator owns at most one
//! pending proposal at a time, and a proposal is only tallied once its voting
//! deadline has passed - so the L2Registry mutation is unobservable until then.

use std::collections::{BTreeMap, BTreeSet};
use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::Address;
use commonware_codec::Encode;
use commonware_cryptography::bls12381::primitives::{ops, variant::MinSig};

use crate::internal::addresses::L2_REGISTRY_ADDR;
use crate::world::validators::Validator;
use crate::world::World;

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
    ensure_zk_disabled_operators(world, &addresses);
}

/// Register the single operator key that is about to submit a Tribute offer.
pub(super) fn ensure_tribute_offer_operator(world: &mut World, key: &str) {
    ensure_tribute_offer_operators(world, &[key.to_owned()]);
}

/// Register every `l1_address` that has no L2Registry entry yet as a network
/// with zk verification disabled, then prove the registration is canonical.
///
/// An address that is already registered - by an earlier call in this
/// scenario, or by a governed zk-enabled registration - keeps its record and
/// its policy, so this is safe to call from any offer fixture.
fn ensure_zk_disabled_operators(world: &mut World, l1_addresses: &[Address]) {
    let unique: BTreeSet<Address> = l1_addresses.iter().copied().collect();
    let mut pending: BTreeMap<Address, u64> = BTreeMap::new();
    for l1_address in unique {
        if world
            .state
            .l2_disabled_operator_chains
            .contains_key(&l1_address)
        {
            continue;
        }
        // An operator that is already registered keeps its record and its
        // policy: the bulk-owner fixtures seed their registrations into genesis,
        // and the zk-gate scenarios register chain 0xdead with zk enabled.
        let registered = world
            .rpc
            .l2_chain_by_l1_address(l1_address)
            .expect("read the L2Registry mapping for the offer operator");
        if registered != 0 {
            assert_registered_zk_disabled(world, l1_address, registered);
            world
                .state
                .l2_disabled_operator_chains
                .insert(l1_address, registered);
            continue;
        }
        let chain_id = world.state.l2_next_disabled_chain_id;
        world.state.l2_next_disabled_chain_id = chain_id
            .checked_add(1)
            .expect("harness L2 chain id space");
        pending.insert(l1_address, chain_id);
    }
    if pending.is_empty() {
        return;
    }

    for (l1_address, chain_id) in pending {
        let payload = zk_disabled_registration_payload(l1_address, chain_id);
        govern_l2_registry_payload(world, &payload);
        assert_registered_zk_disabled(world, l1_address, chain_id);
        world
            .state
            .l2_disabled_operator_chains
            .insert(l1_address, chain_id);
    }
}

/// Prove an operator resolves to `chain_id` with zk verification disabled.
///
/// The Tribute offer guard reads exactly this state, so every offer fixture
/// asserts the precondition rather than assuming that seeding or governance
/// produced it.
fn assert_registered_zk_disabled(world: &World, l1_address: Address, chain_id: u64) {
    assert_eq!(
        world
            .rpc
            .l2_chain_by_l1_address(l1_address)
            .expect("read the L2Registry mapping for the offer operator"),
        chain_id,
        "operator {l1_address:#x} is not registered as chain {chain_id}"
    );
    let (registered_address, _, zk_enabled) = world
        .rpc
        .l2_network(chain_id)
        .expect("read the registered L2 network");
    assert_eq!(
        registered_address, l1_address,
        "chain {chain_id} is not owned by the operator that offers"
    );
    assert!(
        !zk_enabled,
        "chain {chain_id} must keep zk verification disabled for the non-ZK offer path"
    );
}

/// Propose `payload` to L2Registry and drive it to `approved`, returning the
/// proposal id the vote module allocated.
///
/// Used by the governed zk-gate scenarios: a later `setZkEnabled` proposal must
/// observe the registration it toggles, so the two cannot share one deadline.
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
                .and_then(|hex| hex.parse().ok())
                .is_some_and(|address| active.contains(&address))
        })
        .collect();
    assert!(
        !validators.is_empty(),
        "no ACTIVE validator key is available to govern the L2 registry"
    );
    validators
}

/// `zk_enabled = false` registration payload for `l1_address`.
///
/// The stored key must be a valid MinSig G2 group key even though the non-ZK
/// path never verifies a signature against it, so each registration carries a
/// freshly generated one.
fn zk_disabled_registration_payload(l1_address: Address, chain_id: u64) -> String {
    let (_, public) = ops::keypair::<_, MinSig>(&mut rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    serde_json::json!({
        "operation": "register",
        "chainId": chain_id,
        "l1Address": format!("{l1_address:#x}"),
        "publicKey": format!("0x{}", hex::encode(public.encode())),
        "zkEnabled": false,
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

/// Wait until proposal `proposal_id` has observed every ballot cast for it.
///
/// The expected count comes from the ACTIVE set, not a fixed quorum constant:
/// a proposal that carries the whole live set cannot be short of 2/3 whatever
/// the set size is.
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
        assert_eq!(
            proposal.status, "pending",
            "proposal #{proposal_id} left pending before its votes were tallied"
        );
        if proposal.yes == approvals {
            return;
        }
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
