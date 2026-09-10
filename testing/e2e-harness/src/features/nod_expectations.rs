//! Nod arithmetic expectations derived from the executed public input fixture.

use alloy_primitives::{Address, U256};
use cucumber::then;
use outbe_compressed_entities::{
    decode_stored_tribute_v1, verify_point_read_v1, PointReadRequestV1, PointReadResultV1,
    SelectedHeaderV1, TributeBodyV1, VerifiedPointReadV1,
};
use outbe_ocomp_protocol::league_snapshot::league_snapshot_slot;

use crate::internal::{eth, nod_reference};
use crate::world::ocomp::{OCOMP_PUBLIC_TRIBUTE_AMOUNT_ATTO, OCOMP_PUBLIC_TRIBUTE_AMOUNT_BASE};
use crate::world::World;

fn submitted_inputs(world: &World) -> Vec<(String, Address, U256)> {
    let transactions = &world.state.ocomp_capacity_tribute_tx_hashes;
    if transactions.is_empty() {
        assert!(
            world
                .state
                .ocomp_agent_reward
                .as_ref()
                .is_some_and(|reward| reward.offer_execution_block_number.is_some()),
            "executed singleton reward-bearing offer"
        );
        let key = world
            .validators
            .get(0)
            .evm_key()
            .expect("singleton owner key");
        return vec![(
            world
                .state
                .tribute_tx_hash
                .clone()
                .expect("singleton submitted transaction"),
            eth::address_of(&key).expect("singleton owner"),
            U256::from(100_000_000),
        )];
    }
    assert_eq!(transactions.len(), 10, "ten submitted capacity offers");
    let keys = world
        .state
        .ocomp_capacity_tribute_private_keys
        .get(..10)
        .expect("submitted capacity keys");
    let amount = super::tribute_expectations::amount_minor(
        OCOMP_PUBLIC_TRIBUTE_AMOUNT_BASE,
        OCOMP_PUBLIC_TRIBUTE_AMOUNT_ATTO,
    );
    transactions
        .iter()
        .zip(keys)
        .map(|(tx, key)| {
            (
                tx.clone(),
                eth::address_of(key).expect("capacity owner"),
                amount,
            )
        })
        .collect()
}

fn verified_input(
    world: &World,
    tx: &str,
    owner: Address,
    amount: U256,
    day: u32,
) -> TributeBodyV1 {
    let projected = world
        .projection
        .projected_tribute(0, tx)
        .expect("executed public input Tribute");
    let body =
        decode_stored_tribute_v1(&projected.stored_body).expect("canonical public input body");
    assert_eq!(body.owner, owner, "submitted owner");
    let height = super::tribute_expectations::offer_height(world, tx);
    let (nominal, price) =
        super::tribute_expectations::usd_offer_terms_at(world, day, height, amount);
    assert_eq!(
        body.nominal_amount_minor, nominal,
        "independent normalized nominal"
    );
    assert_eq!(
        body.tribute_price_minor, price,
        "independent effective input price"
    );
    assert_eq!(body.issuance_amount_minor, amount, "submitted USD amount");
    assert_eq!(body.issuance_currency, 840);
    assert_eq!(body.reference_currency, 840);
    assert_eq!(body.worldwide_day.value(), day);
    assert!(!body.exclude_from_intex_issuance);
    let request = PointReadRequestV1 {
        domain_id: 1,
        raw_id: body.tribute_id,
    };
    let ports = world.validators.committee_ports();
    for &port in &ports {
        let package = world
            .rpc
            .compressed_entity(port, request)
            .expect("finalized public input proof");
        let height = package.header.block_number;
        world
            .rpc
            .wait_finalized_checkpoint(&ports, height, 120)
            .expect("input proof height finalized on the complete committee");
        // Do not trust the proof server's supplied extraData. Obtain the exact
        // header commitment independently on every committee port.
        let canonical = eth::block_commitment(&world.rpc.url(ports[0]), height)
            .expect("canonical input header");
        for &peer in &ports {
            assert_eq!(
                eth::block_commitment(&world.rpc.url(peer), height).expect("peer input header"),
                canonical,
                "input header parity on {peer}"
            );
        }
        assert_eq!(package.header.block_hash, canonical.0);
        let trusted = SelectedHeaderV1 {
            block_number: height,
            block_hash: canonical.0,
            extra_data: canonical.2.to_vec(),
        };
        let chain_id = world.rpc.chain_id(port).expect("input proof chain ID");
        assert_eq!(
            verify_point_read_v1(chain_id, request, &trusted, &package.result)
                .expect("verify public input proof"),
            VerifiedPointReadV1::Present,
            "input proof on {port}"
        );
        let PointReadResultV1::Present { body_bytes, .. } = package.result else {
            panic!("verified input must be present")
        };
        assert_eq!(
            body_bytes.as_ref(),
            projected.stored_body,
            "input body on {port}"
        );
        assert_eq!(
            eth::block_commitment(&world.rpc.url(port), height).expect("recheck input header"),
            canonical
        );
    }
    body
}

#[then("the submitted Nod input bodies are independently authenticated before processing")]
fn authenticate_submitted_inputs(world: &mut World) {
    assert!(world.state.ocomp_nod_input_bodies.is_none());
    let day = world
        .state
        .wwd
        .as_deref()
        .expect("submitted WorldwideDay")
        .parse::<u32>()
        .expect("numeric submitted WorldwideDay");
    let inputs = submitted_inputs(world)
        .into_iter()
        .map(|(tx, owner, nominal)| verified_input(world, &tx, owner, nominal, day))
        .collect();
    world.state.ocomp_nod_input_bodies = Some(inputs);
}

#[then("the one-league Nod fields and root match the public input arithmetic on every validator")]
fn nod_fields_match_public_inputs(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("public JobIntent");
    let generation = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .expect("certified Nod set");
    let ports = world.validators.committee_ports();
    let height = request.request_height;
    world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 120)
        .expect("input checkpoint");
    let checkpoint = world
        .rpc
        .checkpoint_at(ports[0], height)
        .expect("input checkpoint identity");
    assert_eq!(checkpoint.block_hash, request.request_block_hash);
    let intent = world
        .rpc
        .ocomp_job_record_at_on(ports[0], request.intent_id, height)
        .expect("input JobIntent at request height")
        .intent;
    // Processing retires the current Tribute partition. Retain independently
    // authenticated inputs before that transition, never read result bodies as
    // the source of expected economic fields.
    let inputs = world
        .state
        .ocomp_nod_input_bodies
        .as_ref()
        .expect("authenticated inputs captured before processing");
    for body in inputs {
        assert_eq!(body.worldwide_day.value(), request.worldwide_day);
    }
    assert_eq!(intent.authenticated_day_count as usize, inputs.len());
    assert_eq!(
        intent.authenticated_day_nominal,
        inputs
            .iter()
            .map(|body| body.nominal_amount_minor)
            .sum::<U256>()
    );
    let mut expected_league = None;
    for &port in &ports {
        assert_eq!(
            world
                .rpc
                .ocomp_job_record_at_on(port, request.intent_id, height)
                .expect("peer input JobIntent")
                .intent,
            intent,
            "input intent on {port}"
        );
        for body in inputs {
            let slot = league_snapshot_slot(request.worldwide_day, body.owner);
            let raw = eth::raw_json_with_params(
                &world.rpc.url(port),
                "eth_getStorageAt",
                serde_json::json!([
                    crate::internal::addresses::WWD_ADDR,
                    slot,
                    format!("0x{height:x}")
                ]),
            )
            .expect("input league slot");
            let value: U256 = serde_json::from_value(raw).expect("canonical input league word");
            let league = u16::try_from(value).expect("input league fits u16");
            assert!(league > 0, "input league is available");
            assert_eq!(
                *expected_league.get_or_insert(league),
                league,
                "one-league fixture and all-validator input parity"
            );
        }
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, height)
                .expect("recheck input checkpoint"),
            checkpoint
        );
    }
    let entry_price = if inputs.len() == 1 {
        let expected = super::oracle_expectations::fresh_wwd_price(world, height);
        assert_eq!(
            intent.frozen_metadosis_values.current_vwap, expected,
            "JobIntent froze the independently recomputed WWD VWAP"
        );
        expected
    } else {
        // This ten-owner profile seeds its day before startup; it has no live
        // FORMING interval. Its frozen price is an input to the Nod arithmetic.
        intent.frozen_metadosis_values.current_vwap
    };
    let expected = nod_reference::single_league_actions(
        inputs,
        expected_league.expect("input league"),
        intent.frozen_metadosis_values.lysis_budget,
        entry_price,
        intent.logical_evaluation_time,
    );
    let expected_root = nod_reference::nod_root(&expected);
    assert_eq!(
        generation.nod_root, expected_root,
        "Nod root from input arithmetic"
    );
    for (index, port) in ports.iter().enumerate() {
        let actual = super::ocomp::result_nod_actions_on(world, index, generation.job_id);
        assert_eq!(
            actual, expected,
            "exact Nod fields from input arithmetic on {port}"
        );
    }
    eprintln!(
        "NOD_INPUT_ARITHMETIC {}",
        serde_json::json!({
            "job_id": generation.job_id, "input_checkpoint": {
                "height": checkpoint.height, "block_hash": checkpoint.block_hash,
                "state_root": checkpoint.state_root,
            },
            "input_count": inputs.len(), "league": expected_league,
            "budget": intent.frozen_metadosis_values.lysis_budget,
            "input_vwap": intent.frozen_metadosis_values.current_vwap,
            "logical_evaluation_time": intent.logical_evaluation_time,
            "expected_root": expected_root,
            "expected_actions": expected.iter().map(|action| format!("{action:?}")).collect::<Vec<_>>(),
        })
    );
}
