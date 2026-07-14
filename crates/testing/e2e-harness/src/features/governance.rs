//! Steps for `features/governance_oip_gip.feature` — committee approval of
//! OIP/GIP via the governance VoteTarget. Reuses vote cast / deadline / tally
//! steps from [`super::update`] where the Gherkin lines match.

use std::thread::sleep;
use std::time::Duration;

use cucumber::{then, when};

use crate::internal::addresses::GOVERNANCE_ADDR;
use crate::world::World;

const APPROVED: u8 = 1;

fn propose_kind(world: &mut World, name: &str, kind: &str, text: &str) {
    let operator = world.validators.operator(name).expect("resolve operator");
    let payload = serde_json::json!({
        "kind": kind,
        "text": text,
    })
    .to_string();

    let tx = world
        .rpc
        .send_propose(&operator, &format!("{GOVERNANCE_ADDR:#x}"), &payload)
        .expect("send propose");
    assert!(world.rpc.wait_tx(&tx, 40), "propose tx not mined: {tx}");
}

#[when(expr = "operator {string} proposes an OIP with text {string}")]
fn propose_oip(world: &mut World, name: String, text: String) {
    propose_kind(world, &name, "oip", &text);
}

#[when(expr = "operator {string} proposes a GIP with text {string}")]
fn propose_gip(world: &mut World, name: String, text: String) {
    propose_kind(world, &name, "gip", &text);
}

#[then(
    expr = "proposal {int} is pending and targets the governance module with kind {string}"
)]
fn proposal_pending_governance(world: &mut World, id: u64, kind: String) {
    let mut vs = world.rpc.vote_status(id);
    for _ in 0..10 {
        if vs.visible {
            break;
        }
        sleep(Duration::from_secs(3));
        vs = world.rpc.vote_status(id);
    }
    assert!(vs.visible, "proposal #{id} not visible after propose");
    assert_eq!(vs.status, "pending", "proposal should be pending");
    let gov_addr = format!("{GOVERNANCE_ADDR:#x}");
    assert!(
        vs.target.eq_ignore_ascii_case(&gov_addr),
        "target {} != governance module {gov_addr}",
        vs.target
    );
    let kind_frag = format!(r#""kind":"{kind}""#);
    assert!(
        vs.payload.contains(&kind_frag),
        "payload missing {kind_frag}: {}",
        vs.payload
    );
}

/// Cast yes votes on an explicit proposal id (dual OIP/GIP flow).
///
/// Unlike the update fire-and-forget cast, each ballot waits for mining so the
/// same validators can cast on proposal 2 next without nonce collisions.
#[when(expr = "validators {string} cast yes votes on proposal {int}")]
fn cast_yes_votes_on_proposal(world: &mut World, names: String, id: u64) {
    for name in names.split(',') {
        let validator = world
            .validators
            .by_name(name.trim())
            .expect("resolve validator");
        match world.rpc.cast_vote(&validator, id, true) {
            Ok(tx) => {
                assert!(
                    world.rpc.wait_tx(&tx, 40),
                    "cast vote tx not mined for {name} on proposal {id}: {tx}"
                );
            }
            Err(err) => {
                // Proposer double-vote (or similar) may revert; tally is authoritative.
                eprintln!("cast vote on proposal {id} by {name} failed (ignored): {err}");
            }
        }
    }
}

#[then(expr = "proposal {int} is approved")]
fn proposal_approved(world: &mut World, id: u64) {
    assert!(
        world.rpc.wait_vote_status(id, "approved", 60),
        "proposal #{id} not approved after deadline"
    );
}

fn assert_approved_record(
    world: &mut World,
    kind: &str,
    id: u64,
    text: &str,
    author_name: &str,
) {
    let validator = world
        .validators
        .by_name(author_name)
        .expect("resolve author validator");
    let key = validator.evm_key().expect("author evm key");
    let want_author = world
        .rpc
        .address_of(&key)
        .expect("derive author address");

    let (status, author, body) = match kind {
        "oip" => world.rpc.get_oip(id).expect("getOip"),
        "gip" => world.rpc.get_gip(id).expect("getGip"),
        other => panic!("unknown kind {other}"),
    };
    assert_eq!(status, APPROVED, "{kind} #{id} status");
    assert!(
        format!("{author:#x}").eq_ignore_ascii_case(&want_author),
        "{kind} #{id} author {author:#x} != {want_author}"
    );
    assert_eq!(body, text, "{kind} #{id} text");
}

#[then(expr = "OIP {int} is Approved with text {string} authored by {string}")]
fn oip_approved_record(world: &mut World, id: u64, text: String, author: String) {
    assert_approved_record(world, "oip", id, &text, &author);
}

#[then(expr = "GIP {int} is Approved with text {string} authored by {string}")]
fn gip_approved_record(world: &mut World, id: u64, text: String, author: String) {
    assert_approved_record(world, "gip", id, &text, &author);
}
