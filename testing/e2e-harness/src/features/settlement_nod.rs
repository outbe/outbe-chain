//! The successor public Nod pays through ERC20; the original retains PayNote coverage.
use super::*;
use alloy_sol_types::{SolCall as _, SolError as _, SolValue as _};
use outbe_compressed_entities::{
    decode_stored_nod_bucket_v1, decode_stored_nod_item_v1, verify_point_read_v1, NodBucketBodyV1,
    NodItemBodyV1, PointReadRequestV1, PointReadResultV1, SelectedHeaderV1, VerifiedPointReadV1,
    WwdEntityId,
};

const NOD_CALL_GAS_LIMIT: u64 = 10_000_000;

const PAYER_KEY: &str = "0x7777777777777777777777777777777777777777777777777777777777777777";

#[then("a third party pays another public Nod in ERC20 and mines Gratis only for its owner")]
fn third_party_settles_and_mines(world: &mut World) {
    let port = world.validators.primary_port();
    let ports = world.validators.committee_ports();
    let url = world.rpc.url(port);
    let owner_key = world
        .validators
        .get(1)
        .evm_key()
        .expect("successor Tribute owner key");
    let owner = eth::address_of(&owner_key).expect("successor owner");
    let payer = eth::address_of(PAYER_KEY).expect("independent settlement payer");
    assert_ne!(owner, payer);
    let funding = world
        .rpc
        .fund_key(&world.validators.get(0), PAYER_KEY, 100)
        .expect("fund payer gas");
    assert!(world.rpc.wait_successful_receipt(&funding, 120));
    let materialization_deadline =
        Instant::now() + Duration::from_secs(MATERIALIZED_NOD_TIMEOUT_SECS);
    let (id_bytes, initial) = loop {
        let observed = stable_live_read(world, port, 0, || {
            world
                .rpc
                .materialized_nod_for_owner(port, owner)
                .expect("live materialized Nod lookup")
        });
        if let Some(nod) = observed {
            break nod;
        }
        assert!(
            Instant::now() < materialization_deadline,
            "successor Nod did not materialize"
        );
        sleep(Duration::from_millis(250));
    };
    let id = WwdEntityId::try_from(id_bytes.as_slice()).expect("public Nod identity");
    assert_eq!(
        initial.worldwideDay,
        world
            .state
            .ocomp_successor_job_request
            .as_ref()
            .expect("retained real successor JobIntent")
            .worldwide_day
    );
    assert_eq!(initial.owner, owner);
    assert!(!initial.isSettled);
    assert!(!initial.settlementCostMinor.is_zero());
    qualify_public_nod(world, id, owner, initial.floorPriceMinor, initial.issuedAt);
    let head = world.rpc.head(port).expect("qualified head");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, head, 120)
        .expect("qualification finalized");
    let qualified = nod_snapshot(world, owner, id, head);
    let qualified_bodies = nod_bodies(world, id, head);
    assert_snapshot_bodies(&qualified, &qualified_bodies);
    let body = qualified.body.as_ref().expect("qualified public Nod");
    assert!(body.isQualified);
    assert!(!body.isSettled);
    let bucket_id = qualified_bodies.1.entity_id();
    let vault = eth::read_call(
        &url,
        addresses::VAULT_ROUTER_ADDR,
        &eth::IVaultRouter::referenceCurrencyVaultAtCall {
            isoCode: USD_ISO,
            index: U256::ZERO,
        },
    )
    .expect("PayNote scenario registered USD reserve");
    assert_ne!(vault, Address::ZERO);
    let asset =
        eth::read_call(&url, vault, &ISettlementVault::assetCall {}).expect("reserve asset");
    assert_eq!(body.referenceCurrency, USD_ISO);
    // Keep enough allowance and funds for a duplicate to pay if its guard regresses.
    fund_and_approve(
        world,
        asset,
        PAYER_KEY,
        payer,
        addresses::NOD_FACTORY_ADDR,
        body.settlementCostMinor
            .checked_mul(U256::from(2))
            .expect("two settlement costs"),
    );
    let owner_keys =
        eth::derive_account_keys(&url, &owner_key, Ledger::Gratis).expect("owner Gratis keys");
    let payer_keys =
        eth::derive_account_keys(&url, PAYER_KEY, Ledger::Gratis).expect("payer Gratis keys");
    let pay = eth::INodFactory::settleNodCall {
        nodId: id.to_u256(),
        asset,
    };
    // Capture compressed-entity prestate before submitting the mutation. These
    // live observations are finalized checkpoints, not historical eth_call reads.
    let before_indexes = nod_snapshot(world, owner, id, head);
    let before = nod_bodies(world, id, head);
    assert_snapshot_bodies(&before_indexes, &before);
    assert_same_snapshot(&qualified, &before_indexes);
    let settled = eth::send_call_outcome(&url, addresses::NOD_FACTORY_ADDR, PAYER_KEY, &pay, None)
        .expect("independent ERC20 settlement");
    assert_mined_success(&settled, "third-party ERC20 settlement");
    let settled_height = finalize(world, &settled);
    assert_relay_transaction(world, &settled, payer, &pay.abi_encode());
    assert_receipt_event(
        &settled.receipt,
        addresses::NOD_FACTORY_ADDR,
        &eth::INodFactory::NodPaid {
            owner,
            nodId: id.to_u256(),
            asset,
            nullifier: B256::ZERO,
            amountCovered: body.settlementCostMinor,
        },
    );
    for &peer in &ports {
        let peer_url = world.rpc.url(peer);
        let balances_before = asset_balances(
            &peer_url,
            asset,
            [payer, vault, owner, addresses::NOD_FACTORY_ADDR],
            settled_height - 1,
        );
        let balances_after = asset_balances(
            &peer_url,
            asset,
            [payer, vault, owner, addresses::NOD_FACTORY_ADDR],
            settled_height,
        );
        assert_payment_delta(balances_before, balances_after, body.settlementCostMinor);
        let fee = crate::world::rpc::Rpc::receipt_gas_cost(&settled.receipt).expect("payer gas");
        assert_eq!(
            native_balance_at(&peer_url, payer, settled_height) + fee,
            native_balance_at(&peer_url, payer, settled_height - 1)
        );
    }
    let paid = nod_bodies(world, id, settled_height);
    assert_paid_transition(&before, &paid);
    let paid_indexes = nod_snapshot(world, owner, id, settled_height);
    assert_snapshot_bodies(&paid_indexes, &paid);
    assert_index_transition(
        &before_indexes.index,
        &paid_indexes.index,
        id.to_u256(),
        false,
    );
    let duplicate_before = nod_snapshot(world, owner, id, settled_height);
    assert_same_snapshot(&paid_indexes, &duplicate_before);
    let duplicate = assert_live_nod_revert(world, &pay, "nod is already settled");
    let duplicate_height = duplicate.block_number().expect("duplicate receipt block");
    assert_eq!(nod_bodies(world, id, duplicate_height), paid);
    let duplicate_after = nod_snapshot(world, owner, id, duplicate_height);
    assert_same_snapshot(&duplicate_before, &duplicate_after);
    for &peer in &ports {
        let peer_url = world.rpc.url(peer);
        assert_eq!(
            asset_balances(
                &peer_url,
                asset,
                [payer, vault, owner, addresses::NOD_FACTORY_ADDR],
                duplicate_height - 1
            ),
            asset_balances(
                &peer_url,
                asset,
                [payer, vault, owner, addresses::NOD_FACTORY_ADDR],
                duplicate_height
            ),
            "duplicate payment rollback"
        );
    }
    let nonce = eth::read_call(
        &url,
        addresses::GRATIS_ADDR,
        &eth::IGratis::opNonceOfCall { account: owner },
    )
    .expect("owner mint nonce");
    let pow = find_mining_pow_nonce(outbe_common::pow::MiningDomain::Nod, id.to_u256(), owner);
    let pair_chain_id = chain_id_b256(world);
    // This is a valid MAC for the payer's own account, never the Nod owner's.
    let wrong_mac = outbe_tee_enclave::gratis::modify_mac(
        &payer_keys.modify,
        payer,
        GratisOp::Mint,
        body.gratisLoadMinor,
        nonce,
        pair_chain_id,
    );
    let mut mine = eth::INodFactory::mineGratisCall {
        nodId: id.to_u256(),
        nonce: pow,
        mac: B256::from(wrong_mac),
        opNonce: nonce,
    };
    let rejected_before = nod_snapshot(world, owner, id, duplicate_height);
    assert_same_snapshot(&paid_indexes, &rejected_before);
    for &peer in &ports {
        assert_eq!(
            eth::read_call_result(
                &world.rpc.url(peer),
                addresses::GRATIS_ADDR,
                &eth::IGratis::opNonceOfCall { account: owner }
            )
            .expect("owner nonce before wrong MAC"),
            nonce
        );
    }
    let wrong_call = mine.abi_encode();
    let pair_accounts = [payer, vault, owner, addresses::NOD_FACTORY_ADDR];
    let pair_height = live_checkpoint(world, port).height;
    world
        .rpc
        .wait_finalized_checkpoint(&ports, pair_height, 120)
        .expect("MAC pair prestate finalized");
    let pair_before = mac_pair_state(world, asset, pair_accounts, pair_height);
    assert_eq!(pair_before.owner_gratis.1, nonce);
    let rejected = assert_mined_nod_rejection(world, &mine);
    let rejected_height = rejected.block_number().expect("wrong MAC receipt block");
    assert_eq!(
        mac_pair_state(world, asset, pair_accounts, rejected_height - 1),
        pair_before,
        "MAC guard inputs changed before the failed transaction"
    );
    let pair_rejected = mac_pair_state(world, asset, pair_accounts, rejected_height);
    assert_eq!(
        pair_rejected, pair_before,
        "wrong MAC must roll back raw Gratis, nonces and ERC20"
    );
    assert_eq!(
        nod_bodies(world, id, rejected_height),
        paid,
        "wrong MAC must preserve the paid item and bucket"
    );
    let rejected_after = nod_snapshot(world, owner, id, rejected_height);
    assert_same_snapshot(&rejected_before, &rejected_after);
    for &peer in &ports {
        let peer_url = world.rpc.url(peer);
        let fee =
            crate::world::rpc::Rpc::receipt_gas_cost(&rejected.receipt).expect("wrong MAC gas");
        assert_eq!(
            native_balance_at(&peer_url, payer, rejected_height) + fee,
            native_balance_at(&peer_url, payer, rejected_height - 1),
            "wrong MAC debits only relayer gas"
        );
        assert_eq!(
            eth::read_call_at_result(
                &peer_url,
                addresses::GRATIS_ADDR,
                &eth::IGratis::opNonceOfCall { account: owner },
                rejected_height
            )
            .expect("owner nonce after wrong MAC"),
            nonce
        );
        for (account, keys) in [(owner, &owner_keys), (payer, &payer_keys)] {
            assert_eq!(
                gratis_at(&peer_url, account, &keys.view, rejected_height - 1),
                gratis_at(&peer_url, account, &keys.view, rejected_height),
                "wrong MAC changed Gratis"
            );
            assert_eq!(
                eth::read_call_at_result(
                    &peer_url,
                    addresses::GRATIS_ADDR,
                    &eth::IGratis::opNonceOfCall { account },
                    rejected_height - 1
                )
                .expect("pre-rejection nonce"),
                eth::read_call_at_result(
                    &peer_url,
                    addresses::GRATIS_ADDR,
                    &eth::IGratis::opNonceOfCall { account },
                    rejected_height
                )
                .expect("post-rejection nonce")
            );
        }
    }
    mine.mac = B256::from(outbe_tee_enclave::gratis::modify_mac(
        &owner_keys.modify,
        owner,
        GratisOp::Mint,
        body.gratisLoadMinor,
        nonce,
        pair_chain_id,
    ));
    assert_only_mac_changed(&wrong_call, &mine);
    assert_eq!(
        chain_id_b256(world),
        pair_chain_id,
        "MAC chain binding changed"
    );
    let minted_before = nod_snapshot(world, owner, id, rejected_height);
    assert_same_snapshot(&paid_indexes, &minted_before);
    assert_eq!(
        nod_bodies(world, id, rejected_height),
        paid,
        "paid bodies changed before the valid retry"
    );
    let retry_height = live_checkpoint(world, port).height;
    world
        .rpc
        .wait_finalized_checkpoint(&ports, retry_height, 120)
        .expect("MAC retry prestate finalized");
    assert_eq!(
        mac_pair_state(world, asset, pair_accounts, retry_height),
        pair_rejected,
        "MAC guard inputs changed before the valid retry"
    );
    let minted = eth::send_call_outcome(&url, addresses::NOD_FACTORY_ADDR, PAYER_KEY, &mine, None)
        .expect("owner-authorized relayed mining retry");
    assert_mined_success(&minted, "owner-authorized relayed mining");
    let minted_height = finalize(world, &minted);
    assert_eq!(
        mac_pair_state(world, asset, pair_accounts, minted_height - 1),
        pair_rejected,
        "MAC guard inputs changed before valid transaction execution"
    );
    let pair_minted = mac_pair_state(world, asset, pair_accounts, minted_height);
    assert_eq!(
        pair_minted.owner_gratis.1,
        nonce.checked_add(1).expect("owner nonce increment")
    );
    assert_eq!(
        pair_minted.payer_gratis, pair_rejected.payer_gratis,
        "relayer Gratis remains unchanged"
    );
    assert_eq!(
        pair_minted.assets, pair_rejected.assets,
        "valid retry leaves settlement tokens unchanged"
    );
    for &peer in &ports {
        let tx = eth::raw_json_result(
            &world.rpc.url(peer),
            "eth_getTransactionByHash",
            serde_json::json!([minted.transaction_hash]),
        )
        .expect("valid MAC transaction gas");
        let gas = U256::from_str_radix(
            tx["gas"]
                .as_str()
                .expect("submitted gas")
                .trim_start_matches("0x"),
            16,
        )
        .expect("submitted gas hex");
        assert_eq!(
            gas,
            U256::from(NOD_CALL_GAS_LIMIT),
            "both MAC calls use identical gas limits"
        );
    }
    let minted_after = nod_snapshot(world, owner, id, minted_height);
    assert_index_transition(
        &minted_before.index,
        &minted_after.index,
        id.to_u256(),
        true,
    );
    assert!(
        minted_after.body.is_none(),
        "mined Nod left the live owner index"
    );
    assert_relay_transaction(world, &minted, payer, &mine.abi_encode());
    assert_receipt_event(
        &minted.receipt,
        addresses::NOD_FACTORY_ADDR,
        &eth::INodFactory::NodExercised {
            owner,
            nodId: id.to_u256(),
            gratisLoadMinor: body.gratisLoadMinor,
        },
    );
    assert_receipt_event(
        &minted.receipt,
        addresses::NOD_FACTORY_ADDR,
        &eth::INodFactory::NodBurned {
            owner,
            nodId: id.to_u256(),
            gratisLoadMinor: body.gratisLoadMinor,
        },
    );
    for &peer in &ports {
        let peer_url = world.rpc.url(peer);
        assert_eq!(
            gratis_at(&peer_url, owner, &owner_keys.view, minted_height),
            gratis_at(&peer_url, owner, &owner_keys.view, minted_height - 1) + body.gratisLoadMinor
        );
        assert_eq!(
            gratis_at(&peer_url, payer, &payer_keys.view, minted_height),
            gratis_at(&peer_url, payer, &payer_keys.view, minted_height - 1)
        );
        assert_eq!(
            asset_balances(
                &peer_url,
                asset,
                [payer, vault, owner, addresses::NOD_FACTORY_ADDR],
                minted_height - 1
            ),
            asset_balances(
                &peer_url,
                asset,
                [payer, vault, owner, addresses::NOD_FACTORY_ADDR],
                minted_height
            ),
            "relayed mining must not move settlement tokens"
        );
        let fee =
            crate::world::rpc::Rpc::receipt_gas_cost(&minted.receipt).expect("relayer mining gas");
        assert_eq!(
            native_balance_at(&peer_url, payer, minted_height) + fee,
            native_balance_at(&peer_url, payer, minted_height - 1)
        );
        assert!(
            compressed_body(world, peer, 2, id, minted_height).is_none(),
            "mined Nod must have authenticated absence"
        );
        // The V2 fixture issued a singleton bucket; mining its one paid right
        // must remove the bucket instead of leaving an orphan settled counter.
        assert_eq!(paid.1.settled_nods, 1);
        assert!(
            compressed_body(world, peer, 3, bucket_id, minted_height).is_none(),
            "empty paid bucket must be deleted"
        );
    }
    eprintln!("settlement_evidence kind=controlled_mac_pair rejected_tx={} accepted_tx={} negative_status=0 positive_status=1 only_call_mac_changed=true guard_state_preserved=true gas_limit={NOD_CALL_GAS_LIMIT}",
        rejected.transaction_hash, minted.transaction_hash);
    eprintln!("settlement_evidence kind=erc20_relayed_nod owner={owner:#x} payer={payer:#x} nod={} settle={} duplicate={} wrong_mac={} mine={}",
        id.to_u256(), settled.transaction_hash, duplicate.transaction_hash, rejected.transaction_hash, minted.transaction_hash);
}

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
        };
        if let Some(expected) = &expected {
            assert_eq!(expected, &state, "MAC-pair state differs across validators");
        } else {
            expected = Some(state);
        }
    }
    expected.expect("nonempty MAC-pair observer cohort")
}

pub(super) fn qualify_public_nod(
    world: &mut World,
    id: WwdEntityId,
    owner: Address,
    floor: U256,
    issued_at: u64,
) {
    assert_ne!(
        issued_at, 0,
        "public Nod must have a sealed issuance timestamp"
    );
    let first_full_day = outbe_primitives::time::first_full_day(issued_at);
    let port = world.validators.primary_port();
    let ports = world.validators.committee_ports();
    let head = world.rpc.head(port).expect("successor qualification head");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, head, 120)
        .expect("qualification precondition finality");
    if successor_is_qualified(world, owner, id, head) {
        return;
    }
    let rate = floor
        .checked_mul(U256::from(2))
        .expect("successor qualification quote");
    assert!(rate > floor);
    let mut first_boundary_day = None;
    // The first closed day can be the partial issuance day or contain earlier
    // low-price samples. The next entire UTC day uses only the declared quote.
    // Two transitions are sufficient; no Nod state or Oracle history is injected.
    for boundary in 0..2 {
        crate::features::price_oracle::publish_controlled_quote(world, rate);
        let publication = world
            .price_oracle
            .last_oracle_block()
            .expect("fresh high quote finalized");
        let published_at = world
            .rpc
            .block_timestamp(port, publication)
            .expect("qualification quote timestamp");
        let now = world
            .rpc
            .latest_block_timestamp(port)
            .expect("qualification clock");
        assert_eq!(
            published_at / 86_400,
            now / 86_400,
            "high quote must finalize within the day being closed"
        );
        if let Some(expected_day) = first_boundary_day {
            assert_eq!(
                published_at / 86_400,
                expected_day,
                "fallback must close the complete subsequent UTC day"
            );
        }
        let target = (now / 86_400 + 1)
            .checked_mul(86_400)
            .and_then(|v| v.checked_add(1))
            .expect("next UTC boundary");
        let closed_day = outbe_primitives::time::timestamp_to_date_key(now);
        let (_, _, height, pending) =
            crate::features::ocomp::restart_committee_at_logical_time(world, target);
        for &peer in &ports {
            assert!(world.rpc.wait_finalized_at_least(peer, height, 240));
        }
        let deadline = Instant::now() + Duration::from_secs(120);
        if let Some(pending) = pending {
            while !crate::features::price_oracle::observe_pending_publication(world, &pending) {
                assert!(
                    Instant::now() < deadline,
                    "post-jump feeder did not finalize"
                );
                sleep(Duration::from_millis(500));
            }
        }
        loop {
            let latest = world
                .rpc
                .finalized(port)
                .expect("qualification finalized height");
            let checkpoint = world
                .rpc
                .wait_finalized_checkpoint(&ports, latest, 120)
                .expect("common closed-day qualification checkpoint");
            let values = ports
                .iter()
                .map(|&peer| {
                    eth::read_call_at(
                        &world.rpc.url(peer),
                        outbe_primitives::addresses::ORACLE_ADDRESS,
                        &eth::IOracle::getUtcDayVwapCall {
                            base: Address::ZERO,
                            quote: outbe_primitives::asset_type::currency_address(USD_ISO),
                            utcDay: closed_day,
                        },
                        checkpoint.height,
                    )
                })
                .collect::<Vec<_>>();
            if let Some(vwap) = values.first().copied().flatten() {
                if values.iter().all(|value| *value == Some(vwap)) {
                    if boundary == 1 {
                        assert_eq!(
                            vwap, rate,
                            "the full fallback UTC day must contain only the declared high quote"
                        );
                    }
                    if closed_day < first_full_day || vwap <= floor {
                        assert_eq!(boundary, 0, "isolated high-price day must exceed Nod floor");
                        eprintln!("settlement_evidence kind=qualification_fallback day={closed_day} first_full_day={first_full_day} vwap={vwap} floor={floor}");
                        first_boundary_day = Some(target / 86_400);
                        break;
                    }
                    if successor_is_qualified(world, owner, id, checkpoint.height) {
                        eprintln!("settlement_evidence kind=successor_qualified day={closed_day} vwap={vwap} floor={floor} boundary={} height={}", boundary + 1, checkpoint.height);
                        return;
                    }
                }
            }
            assert!(Instant::now() < deadline,
                "successor qualification failed: boundary={} day={closed_day} floor={floor} rate={rate} finalized={} observed_vwaps={values:?}",
                boundary + 1, checkpoint.height);
            sleep(Duration::from_millis(500));
        }
    }
    panic!("successor remained unqualified after two closed UTC days");
}

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
    before: &(NodItemBodyV1, NodBucketBodyV1),
    after: &(NodItemBodyV1, NodBucketBodyV1),
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

/// Read live CE views without mixing blocks. Only successful observations that
/// straddle a head change are retried; RPC and decoding failures remain fatal.
fn stable_live_read<T>(world: &World, port: u16, minimum: u64, read: impl Fn() -> T) -> T {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        assert!(
            Instant::now() < deadline,
            "live Nod checkpoint did not stabilize on {port}"
        );
        let before = live_checkpoint(world, port);
        if before.height < minimum {
            sleep(Duration::from_millis(100));
            continue;
        }
        let value = read();
        let after = live_checkpoint(world, port);
        if before != after {
            continue;
        }
        let finalized = world
            .rpc
            .wait_finalized_checkpoint(&world.validators.committee_ports(), before.height, 120)
            .expect("live Nod observation finalized with cohort parity");
        assert_eq!(
            finalized, before,
            "live Nod observation changed before finality"
        );
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, before.height)
                .expect("recheck live Nod checkpoint"),
            before
        );
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
                .expect("live Nod rejection with exact EVM revert data");
                assert_eq!(
                    actual.as_ref(),
                    expected.as_slice(),
                    "unexpected live Nod guard on {port}"
                );
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
/// Wrong-MAC authorization is established separately by the controlled retry.
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
        let snapshot = stable_live_read(world, port, minimum, || {
            let count: usize = eth::read_call_result(
                &url,
                addresses::NOD_ADDR,
                &eth::INod::balanceOfCall { owner },
            )
            .expect("live Nod owner count")
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
                    .expect("live Nod owner index")
                })
                .collect::<Vec<_>>();
            assert_eq!(
                owner_ids
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len(),
                count,
                "duplicate Nod owner index"
            );
            let total_supply =
                eth::read_call_result(&url, addresses::NOD_ADDR, &eth::INod::totalSupplyCall {})
                    .expect("live Nod supply");
            let body = owner_ids.contains(&id.to_u256()).then(|| {
                let body = eth::read_call_result(
                    &url,
                    addresses::NOD_ADDR,
                    &eth::INod::nodDataCall {
                        nodId: id.to_u256(),
                    },
                )
                .expect("live Nod body");
                assert_eq!(body.owner, owner);
                assert_eq!(body.nodId, id.to_u256());
                body
            });
            NodSnapshot {
                index: NodIndexSnapshot {
                    owner_ids,
                    total_supply,
                },
                body,
            }
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

fn assert_snapshot_bodies(snapshot: &NodSnapshot, bodies: &(NodItemBodyV1, NodBucketBodyV1)) {
    let body = snapshot.body.as_ref().expect("live Nod body present");
    let item = &bodies.0;
    assert_eq!(body.nodId, item.nod_id.to_u256());
    assert_eq!(body.owner, item.owner);
    assert_eq!(body.worldwideDay, item.worldwide_day.value());
    assert_eq!(body.leagueId, item.league_id);
    assert_eq!(body.floorPriceMinor, item.floor_price_minor);
    assert_eq!(body.gratisLoadMinor, item.gratis_load_minor);
    assert_eq!(body.issuanceCurrency, item.issuance_currency);
    assert_eq!(body.referenceCurrency, item.reference_currency);
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
    if encrypted.is_empty() {
        U256::ZERO
    } else {
        outbe_tee_enclave::gratis::decrypt_balance(view, owner, encrypted.as_ref())
            .expect("decrypt finalized Gratis")
    }
}

fn nod_bodies(world: &World, id: WwdEntityId, minimum: u64) -> (NodItemBodyV1, NodBucketBodyV1) {
    let mut expected = None;
    for port in world.validators.committee_ports() {
        let item = decode_stored_nod_item_v1(
            &compressed_body(world, port, 2, id, minimum).expect("Nod body present"),
        )
        .expect("canonical Nod body");
        let bucket_id = WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key);
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
    let canonical = eth::block_commitment(&world.rpc.url(world.validators.primary_port()), height)
        .expect("canonical proof header");
    world
        .rpc
        .wait_finalized_checkpoint(&world.validators.committee_ports(), height, 120)
        .expect("proof header finalized");
    for peer in world.validators.committee_ports() {
        assert_eq!(
            eth::block_commitment(&world.rpc.url(peer), height).expect("peer proof header"),
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
        let item = NodItemBodyV1 {
            nod_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(1)),
            owner: Address::repeat_byte(3),
            gratis_load_minor: U256::from(100),
            worldwide_day: day,
            league_id: 0,
            floor_price_minor: U256::from(10),
            bucket_key,
            issuance_currency: 840,
            reference_currency: 840,
            issued_at: 1,
            is_settled: false,
        };
        let bucket = NodBucketBodyV1 {
            bucket_key,
            worldwide_day: day,
            floor_price_minor: U256::from(10),
            is_qualified: true,
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
                1 => bad.0.owner = Address::repeat_byte(9),
                2 => bad.0.gratis_load_minor += U256::ONE,
                _ => bad.0.is_settled = false,
            }
            assert!(std::panic::catch_unwind(|| assert_paid_transition(&before, &bad)).is_err());
        }
    }
}
