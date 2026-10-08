//! A third party pays the successor public Nod by ERC20. Its owner paid the original.
use super::*;
use alloy_sol_types::{SolCall as _, SolError as _, SolValue as _};
use outbe_compressed_entities::{
    decode_stored_nod_bucket_v1, decode_stored_nod_item_v2, verify_point_read_v1, NodBucketBodyV1,
    NodItemBodyV2, PointReadRequestV1, PointReadResultV1, SelectedHeaderV1, VerifiedPointReadV1,
    WwdEntityId,
};

const NOD_CALL_GAS_LIMIT: u64 = 10_000_000;

const PAYER_KEY: &str = "0x7777777777777777777777777777777777777777777777777777777777777777";

#[then("a third party pays another public Nod in ERC20 and mines Gratis only for its owner")]
fn third_party_settles_and_mines(world: &mut World) {
    let day = world
        .state
        .ocomp_successor_job_request
        .as_ref()
        .expect("successor JobIntent")
        .worldwide_day;
    run_relayed_mining(world, 1, day, false);
}

#[path = "settlement_nod/relayed.rs"]
mod relayed;
pub(super) use relayed::run_relayed_mining;

fn assert_only_mac_changed(wrong_call: &[u8], correct: &eth::INodFactory::mineGratisCall) {
    let wrong =
        eth::INodFactory::mineGratisCall::abi_decode(wrong_call).expect("canonical wrong-MAC call");
    assert_ne!(
        wrong.mac, correct.mac,
        "controlled retry must change the MAC"
    );
    let restored = eth::INodFactory::mineGratisCall {
        nodId: correct.nodId,
        nonce: correct.nonce,
        mac: wrong.mac,
        opNonce: correct.opNonce,
    };
    assert_eq!(
        restored.abi_encode(),
        wrong_call,
        "only the call MAC may change"
    );
}

#[derive(Debug, PartialEq, Eq)]
struct MacPairState {
    assets: [U256; 4],
    owner_gratis: (alloy_primitives::Bytes, u64),
    payer_gratis: (alloy_primitives::Bytes, u64),
    fidelity: [Vec<U256>; 2],
    first_qualified_start: U256,
}

/// Ordinary EVM storage remains available at exact receipt boundaries. The
/// account order matches settlement accounting: payer, vault, owner, factory.
fn mac_pair_state(
    world: &World,
    asset: Address,
    accounts: [Address; 4],
    height: u64,
) -> MacPairState {
    let mut expected = None;
    for port in world.validators.committee_ports() {
        let url = world.rpc.url(port);
        let gratis = |account| {
            (
                eth::read_call_at_result(
                    &url,
                    addresses::GRATIS_ADDR,
                    &eth::IGratis::balanceOfCall { account },
                    height,
                )
                .expect("raw MAC-pair Gratis ciphertext"),
                eth::read_call_at_result(
                    &url,
                    addresses::GRATIS_ADDR,
                    &eth::IGratis::opNonceOfCall { account },
                    height,
                )
                .expect("MAC-pair authorization nonce"),
            )
        };
        let state = MacPairState {
            assets: asset_balances(&url, asset, accounts, height),
            owner_gratis: gratis(accounts[2]),
            payer_gratis: gratis(accounts[0]),
            fidelity: [
                fidelity_storage_at(&url, accounts[2], height),
                fidelity_storage_at(&url, accounts[0], height),
            ],
            first_qualified_start: storage_word_at(
                &url,
                outbe_primitives::addresses::FIDELITY_ADDRESS,
                U256::ONE,
                height,
            ),
        };
        if let Some(expected) = &expected {
            assert_eq!(expected, &state, "MAC-pair state differs across validators");
        } else {
            expected = Some(state);
        }
    }
    expected.expect("nonempty MAC-pair observer cohort")
}

fn storage_word_at(url: &str, address: Address, slot: U256, height: u64) -> U256 {
    let word = eth::raw_json_result(
        url,
        "eth_getStorageAt",
        serde_json::json!([
            format!("{address:#x}"),
            format!("{slot:#066x}"),
            format!("0x{height:x}")
        ]),
    )
    .expect("raw finalized ledger storage");
    U256::from_str_radix(
        word.as_str()
            .expect("storage word")
            .trim_start_matches("0x"),
        16,
    )
    .expect("hex storage word")
}

fn fidelity_storage_at(url: &str, owner: Address, height: u64) -> Vec<U256> {
    let address = outbe_primitives::addresses::FIDELITY_ADDRESS;
    let slot = owner.mapping_slot(U256::ZERO);
    let base = storage_word_at(url, address, slot, height);
    let mut words = vec![base];
    if base.bit(0) {
        let len = ((base - U256::ONE) / U256::from(2)).to::<usize>();
        assert!(len <= 1 << 20, "bounded Fidelity cohort fixture");
        let start = U256::from_be_bytes(alloy_primitives::keccak256(slot.to_be_bytes::<32>()).0);
        for offset in 0..len.div_ceil(32) {
            words.push(storage_word_at(
                url,
                address,
                start + U256::from(offset),
                height,
            ));
        }
    }
    words
}

#[path = "settlement_nod/qualification.rs"]
mod qualification;
pub(super) use qualification::qualify_public_nod;

/// The Nod owner is an active voter and may receive delayed native fee credits.
/// Prove the relayer's exact transaction and gas debit instead of assuming the
/// owner's whole-block native balance is constant. Token deltas stay exact.
fn assert_relay_transaction(
    world: &World,
    outcome: &eth::MinedCallOutcome,
    payer: Address,
    input: &[u8],
) {
    for port in world.validators.committee_ports() {
        let tx = eth::raw_json_result(
            &world.rpc.url(port),
            "eth_getTransactionByHash",
            serde_json::json!([outcome.transaction_hash]),
        )
        .expect("finalized relay transaction");
        assert_relay_envelope(&tx, payer, input);
        assert_eq!(tx["blockHash"], outcome.receipt["blockHash"]);
        assert_eq!(tx["blockNumber"], outcome.receipt["blockNumber"]);
    }
}

fn assert_relay_envelope(tx: &serde_json::Value, payer: Address, input: &[u8]) {
    assert_eq!(tx["from"], serde_json::json!(format!("{payer:#x}")));
    assert_eq!(
        tx["to"],
        serde_json::json!(format!("{:#x}", addresses::NOD_FACTORY_ADDR))
    );
    assert_eq!(tx["value"], "0x0");
    assert_eq!(
        tx["input"],
        serde_json::json!(format!("0x{}", hex::encode(input)))
    );
}

fn finalize(world: &World, outcome: &eth::MinedCallOutcome) -> u64 {
    world
        .rpc
        .finalize_outcome(
            &crate::world::rpc::TxOutcome {
                transaction_hash: outcome.transaction_hash.clone(),
                success: outcome.success,
                receipt: outcome.receipt.clone(),
            },
            &world.validators.committee_ports(),
            120,
        )
        .expect("settlement receipt parity and finality")
        .height
}

fn asset_balances(url: &str, asset: Address, accounts: [Address; 4], height: u64) -> [U256; 4] {
    accounts.map(|account| {
        eth::read_call_at_result(
            url,
            asset,
            &ISettlementAsset::balanceOfCall { account },
            height,
        )
        .expect("historical settlement asset balance")
    })
}

fn assert_payment_delta(before: [U256; 4], after: [U256; 4], amount: U256) {
    assert!(!amount.is_zero());
    assert_eq!(after[0], before[0].checked_sub(amount).expect("payer cost"));
    assert_eq!(
        after[1],
        before[1].checked_add(amount).expect("reserve cost")
    );
    assert_eq!(after[2], before[2], "owner must not pay ERC20");
    assert_eq!(after[3], before[3], "factory must retain no payment");
}

fn assert_paid_transition(
    before: &(NodItemBodyV2, NodBucketBodyV1),
    after: &(NodItemBodyV2, NodBucketBodyV1),
) {
    assert!(!before.0.is_settled);
    let mut item = before.0.clone();
    item.is_settled = true;
    assert_eq!(after.0, item, "payment changes only the Nod paid flag");
    let mut bucket = before.1.clone();
    bucket.settled_nods = bucket.settled_nods.checked_add(1).expect("settled counter");
    assert_eq!(
        after.1, bucket,
        "payment increments only the live paid count"
    );
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NodIndexSnapshot {
    owner_ids: Vec<U256>,
    total_supply: U256,
}

struct NodSnapshot {
    index: NodIndexSnapshot,
    body: Option<eth::INod::NodData>,
}

/// Read live CE views without mixing blocks.
/// Retry successful reads across head changes and corresponding CE parent races.
/// Retry other read failures twice. Stop the scenario if all three attempts fail.
fn stable_live_read<T>(
    world: &World,
    port: u16,
    minimum: u64,
    read: impl Fn() -> Result<T, String>,
) -> T {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        assert!(
            Instant::now() < deadline,
            "live Nod checkpoint did not stabilize on {port}"
        );
        let observation = crate::features::entity_lifecycle::chain::settlement_read(|| {
            let before = live_checkpoint(world, port);
            if before.height < minimum {
                return Ok(None);
            }
            let result = read();
            let after = live_checkpoint(world, port);
            crate::internal::live_ce_read::resolve_live_ce_read(result, before != after)
                .map(|value| value.map(|value| (before, value)))
        });
        let Some((before, value)) = observation.expect("live Nod observation failed") else {
            sleep(Duration::from_millis(100));
            continue;
        };
        world
            .rpc
            .wait_finalized_checkpoint(&world.validators.committee_ports(), before.height, 120)
            .expect("live Nod observation finalized with cohort parity");
        for peer in world.validators.committee_ports() {
            assert_eq!(
                world
                    .rpc
                    .checkpoint_at(peer, before.height)
                    .expect("recheck original live Nod checkpoint"),
                before,
                "original live Nod checkpoint differs on peer {peer}"
            );
        }
        eprintln!("settlement_evidence kind=live_nod_checkpoint port={port} minimum={minimum} height={} hash={:#x}", before.height, before.block_hash);
        return value;
    }
}

/// A Nod rejection uses the same strict receipt and ABI evidence as the shared
/// negative helper, with live CE replay both before and after the mined receipt.
fn assert_live_nod_revert<C: alloy_sol_types::SolCall>(
    world: &World,
    call: &C,
    reason: &str,
) -> crate::world::rpc::TxOutcome {
    let payer = eth::address_of(PAYER_KEY).expect("Nod rejection payer");
    let expected = alloy_sol_types::Revert {
        reason: reason.to_owned(),
    }
    .abi_encode();
    let ports = world.validators.committee_ports();
    let primary = world.validators.primary_port();
    let before = live_checkpoint(world, primary);
    let assert_reason = |minimum, gas_limit| {
        for &port in &ports {
            stable_live_read(world, port, minimum, || {
                let actual = eth::read_call_revert_data_at_block(
                    &world.rpc.url(port),
                    addresses::NOD_FACTORY_ADDR,
                    payer,
                    call,
                    U256::ZERO,
                    alloy_eips::BlockId::latest(),
                    gas_limit,
                )
                .map_err(|error| error.to_string())?;
                assert_eq!(
                    actual.as_ref(),
                    expected.as_slice(),
                    "unexpected live Nod guard on {port}"
                );
                Ok(())
            });
        }
    };
    assert_reason(before.height, NOD_CALL_GAS_LIMIT);
    let outcome = assert_mined_nod_rejection(world, call);
    let height = outcome.block_number().expect("duplicate receipt height");
    assert_reason(height, NOD_CALL_GAS_LIMIT);
    eprintln!("settlement_evidence kind=live_nod_rejection tx={} receipt_height={height} precondition_minimum={} expected_revert=0x{}",
        outcome.transaction_hash, before.height, hex::encode(expected));
    outcome
}

/// Observe an actual failed transaction without assigning it a revert reason.
/// The controlled retry establishes wrong-MAC authorization separately.
fn assert_mined_nod_rejection<C: alloy_sol_types::SolCall>(
    world: &World,
    call: &C,
) -> crate::world::rpc::TxOutcome {
    let payer = eth::address_of(PAYER_KEY).expect("Nod rejection payer");
    let ports = world.validators.committee_ports();
    let primary = world.validators.primary_port();
    let mined = eth::send_call_outcome(
        &world.rpc.url(primary),
        addresses::NOD_FACTORY_ADDR,
        PAYER_KEY,
        call,
        Some(U256::ZERO),
    )
    .expect("fixed-gas Nod rejection mined");
    let outcome = crate::world::rpc::TxOutcome {
        transaction_hash: mined.transaction_hash,
        success: mined.success,
        receipt: mined.receipt,
    };
    assert!(!outcome.success, "Nod rejection unexpectedly succeeded");
    let tx = eth::raw_json_result(
        &world.rpc.url(primary),
        "eth_getTransactionByHash",
        serde_json::json!([outcome.transaction_hash]),
    )
    .expect("mined Nod rejection transaction");
    assert_relay_envelope(&tx, payer, &call.abi_encode());
    let quantity = |value: &serde_json::Value, field: &str| {
        U256::from_str_radix(
            value[field]
                .as_str()
                .expect("rejection quantity")
                .trim_start_matches("0x"),
            16,
        )
        .expect("rejection quantity hex")
    };
    let submitted_gas: u64 = quantity(&tx, "gas")
        .try_into()
        .expect("bounded submitted gas");
    assert!(
        quantity(&outcome.receipt, "gasUsed") < U256::from(submitted_gas),
        "Nod rejection exhausted its gas limit"
    );
    assert_eq!(
        submitted_gas, NOD_CALL_GAS_LIMIT,
        "Nod pair uses the fixed gas limit"
    );
    let height = outcome
        .block_number()
        .expect("Nod rejection receipt height");
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 120)
        .expect("Nod rejection finalized on every validator");
    assert_eq!(
        outcome.receipt["blockHash"],
        serde_json::json!(format!("{:#x}", checkpoint.block_hash))
    );
    assert_eq!(tx["blockHash"], outcome.receipt["blockHash"]);
    assert_eq!(tx["blockNumber"], outcome.receipt["blockNumber"]);
    for &port in &ports {
        let receipt = eth::raw_json_result(
            &world.rpc.url(port),
            "eth_getTransactionReceipt",
            serde_json::json!([outcome.transaction_hash]),
        )
        .expect("Nod rejection receipt parity");
        for field in [
            "transactionHash",
            "blockHash",
            "blockNumber",
            "status",
            "gasUsed",
            "effectiveGasPrice",
            "logs",
        ] {
            assert!(
                receipt.get(field).is_some(),
                "rejection receipt missing {field}"
            );
            assert_eq!(
                receipt[field], outcome.receipt[field],
                "rejection receipt {field} differs on {port}"
            );
        }
        assert_eq!(receipt["status"], "0x0");
        assert_eq!(receipt["logs"], serde_json::json!([]));
    }
    outcome
}

fn live_checkpoint(world: &World, port: u16) -> crate::world::rpc::FinalizedCheckpoint {
    let block = eth::raw_json_result(
        &world.rpc.url(port),
        "eth_getBlockByNumber",
        serde_json::json!(["latest", false]),
    )
    .expect("live Nod header");
    crate::world::rpc::FinalizedCheckpoint {
        height: u64::from_str_radix(
            block["number"]
                .as_str()
                .expect("live height")
                .trim_start_matches("0x"),
            16,
        )
        .expect("live height hex"),
        block_hash: block["hash"]
            .as_str()
            .expect("live block hash")
            .parse()
            .expect("live block hash hex"),
        state_root: block["stateRoot"]
            .as_str()
            .expect("live state root")
            .parse()
            .expect("live state root hex"),
    }
}

fn nod_snapshot(world: &World, owner: Address, id: WwdEntityId, minimum: u64) -> NodSnapshot {
    let mut expected = None;
    for port in world.validators.committee_ports() {
        let url = world.rpc.url(port);
        let snapshot = stable_live_read(world, port, minimum, || -> Result<_, String> {
            let count: usize = eth::read_call_result(
                &url,
                addresses::NOD_ADDR,
                &eth::INod::balanceOfCall { owner },
            )?
            .try_into()
            .expect("bounded Nod count");
            assert!(count <= 32, "bounded Nod owner inventory");
            let owner_ids = (0..count)
                .map(|index| {
                    eth::read_call_result(
                        &url,
                        addresses::NOD_ADDR,
                        &eth::INod::tokenOfOwnerByIndexCall {
                            owner,
                            index: U256::from(index),
                        },
                    )
                })
                .collect::<Result<Vec<_>, String>>()?;
            assert_eq!(
                owner_ids
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len(),
                count,
                "duplicate Nod owner index"
            );
            let total_supply =
                eth::read_call_result(&url, addresses::NOD_ADDR, &eth::INod::totalSupplyCall {})?;
            let body = if owner_ids.contains(&id.to_u256()) {
                let body = eth::read_call_result(
                    &url,
                    addresses::NOD_ADDR,
                    &eth::INod::nodDataCall {
                        nodId: id.to_u256(),
                    },
                )?;
                assert_eq!(body.owner, owner);
                assert_eq!(body.nodId, id.to_u256());
                Some(body)
            } else {
                None
            };
            Ok(NodSnapshot {
                index: NodIndexSnapshot {
                    owner_ids,
                    total_supply,
                },
                body,
            })
        });
        if let Some(expected) = &expected {
            assert_same_snapshot(expected, &snapshot);
        } else {
            expected = Some(snapshot);
        }
    }
    expected.expect("nonempty Nod observer cohort")
}

fn assert_same_snapshot(before: &NodSnapshot, after: &NodSnapshot) {
    assert_eq!(
        before.index, after.index,
        "live Nod inventory/supply parity or rollback"
    );
    assert_eq!(
        before.body.as_ref().map(|body| body.abi_encode()),
        after.body.as_ref().map(|body| body.abi_encode()),
        "live Nod body parity or rollback"
    );
}

fn assert_index_transition(
    before: &NodIndexSnapshot,
    after: &NodIndexSnapshot,
    id: U256,
    removed: bool,
) {
    for snapshot in [before, after] {
        assert!(snapshot.owner_ids.len() <= 32);
        assert_eq!(
            snapshot
                .owner_ids
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            snapshot.owner_ids.len(),
            "duplicate owner index"
        );
    }
    assert!(before.owner_ids.contains(&id));
    if removed {
        let expected = before
            .owner_ids
            .iter()
            .copied()
            .filter(|value| *value != id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            after
                .owner_ids
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            expected,
            "mining must remove only the target Nod"
        );
        assert_eq!(after.owner_ids.len() + 1, before.owner_ids.len());
        assert_eq!(
            after
                .total_supply
                .checked_add(U256::ONE)
                .expect("Nod supply increment"),
            before.total_supply
        );
    } else {
        assert_eq!(
            after, before,
            "settlement must preserve the owner index and supply"
        );
    }
}

fn assert_snapshot_bodies(snapshot: &NodSnapshot, bodies: &(NodItemBodyV2, NodBucketBodyV1)) {
    let body = snapshot.body.as_ref().expect("live Nod body present");
    let item = &bodies.0;
    assert_eq!(body.nodId, item.encrypted.terms.nod_id.to_u256());
    assert_eq!(body.owner, item.encrypted.terms.owner);
    assert_eq!(
        body.worldwideDay,
        item.encrypted.terms.worldwide_day.value()
    );
    assert_eq!(body.leagueId, item.encrypted.terms.league_id);
    assert_eq!(
        Some(body.floorPriceMinor),
        outbe_nod::NodContract::floor_price_minor(bodies.1.entry_price_minor)
    );
    assert_eq!(
        body.encryptedGratisAmount.as_ref(),
        item.encrypted.encrypted_gratis_amount
    );
    assert_eq!(
        body.encryptedCreatorPublicKey.as_ref(),
        item.encrypted.encrypted_creator_public_key
    );
    assert_eq!(body.encryptionBinding, item.encrypted.encryption_binding);
    assert_eq!(body.chainId, item.encrypted.terms.chain_id);
    assert_eq!(
        body.issuanceCurrency,
        item.encrypted.terms.issuance_currency
    );
    assert_eq!(
        body.referenceCurrency,
        item.encrypted.terms.reference_currency
    );
    assert_eq!(body.issuedAt, item.issued_at);
    assert_eq!(body.isSettled, item.is_settled);
}

fn successor_is_qualified(world: &World, owner: Address, id: WwdEntityId, minimum: u64) -> bool {
    let snapshot = nod_snapshot(world, owner, id, minimum);
    let bodies = nod_bodies(world, id, minimum);
    assert_snapshot_bodies(&snapshot, &bodies);
    snapshot
        .body
        .as_ref()
        .expect("live Nod body present")
        .isQualified
}

fn gratis_at(url: &str, owner: Address, view: &[u8; 32], height: u64) -> U256 {
    let encrypted = eth::read_call_at_result(
        url,
        addresses::GRATIS_ADDR,
        &eth::IGratis::balanceOfCall { account: owner },
        height,
    )
    .expect("finalized Gratis ciphertext");
    decrypted_balance_or_zero(encrypted.as_ref(), |ciphertext| {
        outbe_tee_enclave::gratis::decrypt_balance(view, owner, ciphertext)
            .expect("decrypt finalized Gratis")
    })
}

fn nod_bodies(world: &World, id: WwdEntityId, minimum: u64) -> (NodItemBodyV2, NodBucketBodyV1) {
    let mut expected = None;
    for port in world.validators.committee_ports() {
        let item = decode_stored_nod_item_v2(
            &compressed_body(world, port, 2, id, minimum).expect("Nod body present"),
        )
        .expect("canonical Nod body");
        let bucket_id =
            WwdEntityId::from_day_and_digest(item.encrypted.terms.worldwide_day, item.bucket_key);
        let bucket = decode_stored_nod_bucket_v1(
            &compressed_body(world, port, 3, bucket_id, minimum).expect("bucket present"),
        )
        .expect("canonical bucket body");
        let bodies = (item, bucket);
        assert_eq!(
            *expected.get_or_insert(bodies.clone()),
            bodies,
            "all-validator paid body/counter parity"
        );
    }
    expected.expect("nonempty validator cohort")
}

fn compressed_body(
    world: &World,
    port: u16,
    domain_id: u16,
    id: WwdEntityId,
    minimum: u64,
) -> Option<Vec<u8>> {
    let request = PointReadRequestV1 {
        domain_id,
        raw_id: id,
    };
    let package = world
        .rpc
        .compressed_entity_ready(port, request)
        .expect("finalized compressed Nod proof");
    assert!(
        package.header.block_number >= minimum,
        "proof predates finalized transaction"
    );
    let height = package.header.block_number;
    world
        .rpc
        .wait_finalized_checkpoint(&world.validators.committee_ports(), height, 120)
        .expect("proof header finalized");
    let primary = world.validators.primary_port();
    let canonical = eth::block_commitment_result(&world.rpc.url(primary), height)
        .unwrap_or_else(|error| {
            panic!("canonical proof header primary={primary} selected={port} height={height} domain={domain_id} hash={:#x}: {error:#}", package.header.block_hash)
        });
    for peer in world.validators.committee_ports() {
        assert_eq!(
            eth::block_commitment_result(&world.rpc.url(peer), height).unwrap_or_else(|error| {
                panic!("peer proof header peer={peer} selected={port} height={height} domain={domain_id} hash={:#x}: {error:#}", package.header.block_hash)
            }),
            canonical
        );
    }
    assert_eq!(package.header.block_hash, canonical.0);
    let trusted = SelectedHeaderV1 {
        block_number: height,
        block_hash: canonical.0,
        extra_data: canonical.2.to_vec(),
    };
    let verified = verify_point_read_v1(
        world.rpc.chain_id(port).expect("proof chain"),
        request,
        &trusted,
        &package.result,
    )
    .expect("independent Nod proof verification");
    match package.result {
        PointReadResultV1::Present { body_bytes, .. } => {
            assert_eq!(verified, VerifiedPointReadV1::Present);
            Some(body_bytes.to_vec())
        }
        PointReadResultV1::Absent { .. } => {
            assert!(matches!(verified, VerifiedPointReadV1::Absent));
            None
        }
        PointReadResultV1::Unavailable => panic!("Nod proof unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relayed_transaction_oracle_rejects_owner_sender_and_native_value() {
        let payer = Address::repeat_byte(1);
        let tx = serde_json::json!({"from": format!("{payer:#x}"),
            "to": format!("{:#x}", addresses::NOD_FACTORY_ADDR), "value": "0x0", "input": "0xaabb"});
        assert_relay_envelope(&tx, payer, &[0xaa, 0xbb]);
        for (field, value) in [
            ("from", format!("{:#x}", Address::repeat_byte(2))),
            ("value", "0x1".to_owned()),
            ("input", "0xaabc".to_owned()),
        ] {
            let mut bad = tx.clone();
            bad[field] = serde_json::Value::String(value);
            assert!(
                std::panic::catch_unwind(|| assert_relay_envelope(&bad, payer, &[0xaa, 0xbb]))
                    .is_err()
            );
        }
    }

    #[test]
    fn erc20_payment_oracle_rejects_owner_debit_short_reserve_and_factory_dust() {
        let before = [200, 30, 7, 2].map(U256::from);
        let correct = [100, 130, 7, 2].map(U256::from);
        assert_payment_delta(before, correct, U256::from(100));
        for bad in [
            [200, 130, 7, 2],
            [100, 129, 7, 2],
            [100, 130, 6, 2],
            [100, 130, 7, 3],
        ] {
            assert!(std::panic::catch_unwind(|| assert_payment_delta(
                before,
                bad.map(U256::from),
                U256::from(100)
            ))
            .is_err());
        }
    }

    #[test]
    fn mac_pair_oracle_rejects_changed_id_pow_nonce_or_authorization_nonce() {
        let call = |nod_id, pow, mac, op_nonce| eth::INodFactory::mineGratisCall {
            nodId: U256::from(nod_id),
            nonce: pow,
            mac: B256::repeat_byte(mac),
            opNonce: op_nonce,
        };
        let wrong = call(1_u64, 2_u64, 3_u8, 4_u64).abi_encode();
        assert_only_mac_changed(&wrong, &call(1, 2, 5, 4));
        for bad in [
            call(9, 2, 5, 4),
            call(1, 9, 5, 4),
            call(1, 2, 5, 9),
            call(1, 2, 3, 4),
        ] {
            assert!(std::panic::catch_unwind(|| assert_only_mac_changed(&wrong, &bad)).is_err());
        }
    }

    #[test]
    fn nod_index_oracle_rejects_duplicate_substitution_and_wrong_supply() {
        let id = U256::from(2);
        let before = NodIndexSnapshot {
            owner_ids: vec![U256::ONE, id, U256::from(3)],
            total_supply: U256::from(8),
        };
        // Swap-removal may reorder surviving IDs without changing ownership.
        let after = NodIndexSnapshot {
            owner_ids: vec![U256::from(3), U256::ONE],
            total_supply: U256::from(7),
        };
        assert_index_transition(&before, &before, id, false);
        assert_index_transition(&before, &after, id, true);
        for bad in [
            NodIndexSnapshot {
                owner_ids: vec![U256::ONE, U256::ONE],
                ..after.clone()
            },
            NodIndexSnapshot {
                owner_ids: vec![U256::ONE, U256::from(4)],
                ..after.clone()
            },
            NodIndexSnapshot {
                owner_ids: vec![U256::ONE, id],
                ..after.clone()
            },
            NodIndexSnapshot {
                total_supply: before.total_supply,
                ..after.clone()
            },
        ] {
            assert!(
                std::panic::catch_unwind(|| assert_index_transition(&before, &bad, id, true))
                    .is_err()
            );
        }
        let mut changed = before.clone();
        changed.total_supply -= U256::ONE;
        assert!(
            std::panic::catch_unwind(|| assert_index_transition(&before, &changed, id, false))
                .is_err()
        );
    }

    #[test]
    fn paid_nod_oracle_rejects_missing_count_and_changed_owner_or_load() {
        let day = outbe_primitives::time::WorldwideDay::new(20260917);
        let bucket_key = B256::repeat_byte(2);
        let item = NodItemBodyV2 {
            encrypted: outbe_primitives::nod_encryption::EncryptedNodV2 {
                terms: outbe_primitives::nod_encryption::NodTermsV2 {
                    chain_id: 1,
                    nod_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(1)),
                    owner: Address::repeat_byte(3),
                    worldwide_day: day,
                    league_id: 0,
                    entry_price_minor: U256::from(9),
                    issuance_currency: 840,
                    reference_currency: 840,
                },
                encryption_binding: B256::repeat_byte(4),
                encrypted_gratis_amount: vec![0x23; 56],
                encrypted_creator_public_key: vec![0x24; 56],
            },
            bucket_key,
            issued_at: 1,
            is_settled: false,
        };
        let bucket = NodBucketBodyV1 {
            bucket_key,
            worldwide_day: day,
            entry_price_minor: U256::from(9),
            reference_currency: 840,
            settled_nods: 0,
        };
        let before = (item, bucket);
        let mut paid = before.clone();
        paid.0.is_settled = true;
        paid.1.settled_nods = 1;
        assert_paid_transition(&before, &paid);
        for mutation in 0..4 {
            let mut bad = paid.clone();
            match mutation {
                0 => bad.1.settled_nods = 0,
                1 => bad.0.encrypted.terms.owner = Address::repeat_byte(9),
                2 => bad.0.encrypted.encrypted_gratis_amount[8] ^= 1,
                _ => bad.0.is_settled = false,
            }
            assert!(std::panic::catch_unwind(|| assert_paid_transition(&before, &bad)).is_err());
        }
    }
}
