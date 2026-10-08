//! Exercise settlement and private mining through the public RPC interfaces.

use super::*;

struct RelayedNod {
    port: u16,
    ports: Vec<u16>,
    url: String,
    owner: Address,
    payer: Address,
    id: WwdEntityId,
    qualified: NodSnapshot,
    head: u64,
    body: eth::INod::NodData,
    gratis_load: U256,
    bucket_id: WwdEntityId,
    vault: Address,
    asset: Address,
    owner_keys: eth::ConfidentialAccountKeys,
    payer_keys: eth::ConfidentialAccountKeys,
    hardened: bool,
}
struct PaidNod {
    paid: (NodItemBodyV2, NodBucketBodyV1),
    paid_indexes: NodSnapshot,
    settled: eth::MinedCallOutcome,
    duplicate: crate::world::rpc::TxOutcome,
    duplicate_height: u64,
}
struct MiningAttempt {
    mine: eth::INodFactory::mineGratisCall,
    nonce: u64,
    pair_chain_id: B256,
    wrong_call: Vec<u8>,
    pair_accounts: [Address; 4],
    pair_before: MacPairState,
    rejected_before: NodSnapshot,
}
struct RejectedMining {
    rejected: crate::world::rpc::TxOutcome,
    rejected_height: u64,
    pair_rejected: MacPairState,
}
struct MinedNod {
    mine: eth::INodFactory::mineGratisCall,
    minted: eth::MinedCallOutcome,
    minted_height: u64,
}

pub(in super::super) fn run_relayed_mining(
    world: &mut World,
    owner_index: usize,
    day: u32,
    hardened: bool,
) {
    let scenario = prepare_relayed_nod(world, owner_index, day, hardened);
    let payment = settle_nod(world, &scenario);
    restart_paid_nod(world, &scenario, &payment);
    let attempt = prepare_mining_attempt(world, &scenario, &payment);
    let rejected = reject_mining_attempt(world, &scenario, &payment, &attempt);
    let minted = mine_nod(world, &scenario, &payment, &attempt, &rejected);
    assert_mined_ledgers(world, &scenario, &payment, &minted);
    restart_mined_nod(world, &scenario, &attempt, &minted);
    let owner = scenario.owner;
    let payer = scenario.payer;
    let id = scenario.id;
    let settled = &payment.settled;
    let duplicate = &payment.duplicate;
    let rejected = &rejected.rejected;
    let minted = &minted.minted;
    eprintln!("settlement_evidence kind=controlled_mac_pair rejected_tx={} accepted_tx={} negative_status=0 positive_status=1 only_call_mac_changed=true guard_state_preserved=true gas_limit={NOD_CALL_GAS_LIMIT}",
        rejected.transaction_hash, minted.transaction_hash);
    eprintln!("settlement_evidence kind=erc20_relayed_nod owner={owner:#x} payer={payer:#x} nod={} settle={} duplicate={} wrong_mac={} mine={}",
        id.to_u256(), settled.transaction_hash, duplicate.transaction_hash, rejected.transaction_hash, minted.transaction_hash);
}

fn wait_materialized_nod(
    world: &World,
    port: u16,
    owner: Address,
) -> (Vec<u8>, eth::INod::NodData) {
    let materialization_deadline =
        Instant::now() + Duration::from_secs(MATERIALIZED_NOD_TIMEOUT_SECS);
    loop {
        let observed = stable_live_read(world, port, 0, || {
            world.rpc.materialized_nod_for_owner(port, owner)
        });
        if let Some(nod) = observed {
            return nod;
        }
        assert!(
            Instant::now() < materialization_deadline,
            "successor Nod did not materialize"
        );
        sleep(Duration::from_millis(250));
    }
}

fn prepare_relayed_nod(
    world: &mut World,
    owner_index: usize,
    day: u32,
    hardened: bool,
) -> RelayedNod {
    let port = world.validators.primary_port();
    let ports = world.validators.committee_ports();
    let url = world.rpc.url(port);
    let owner_key = world
        .validators
        .get(owner_index)
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
    let (id_bytes, initial) = wait_materialized_nod(world, port, owner);
    let id = WwdEntityId::try_from(id_bytes.as_slice()).expect("public Nod identity");
    assert_eq!(initial.worldwideDay, day);
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
    let gratis_load = crate::internal::nod_keys::decrypt(world, body);
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
    .expect("the owner's Nod settlement registered the USD reserve");
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
    let body = body.clone();
    RelayedNod {
        port,
        ports,
        url,
        owner,
        payer,
        id,
        qualified,
        head,
        body,
        gratis_load,
        bucket_id,
        vault,
        asset,
        owner_keys,
        payer_keys,
        hardened,
    }
}

fn settle_nod(world: &mut World, scenario: &RelayedNod) -> PaidNod {
    let pay = eth::INodFactory::settleNodCall {
        nodId: scenario.id.to_u256(),
        asset: scenario.asset,
        snapshotId: U256::ZERO,
    };
    // Capture compressed-entity prestate before submitting the mutation. These
    // live observations are finalized checkpoints, not historical eth_call reads.
    let before_indexes = nod_snapshot(world, scenario.owner, scenario.id, scenario.head);
    let before = nod_bodies(world, scenario.id, scenario.head);
    assert_snapshot_bodies(&before_indexes, &before);
    assert_same_snapshot(&scenario.qualified, &before_indexes);
    let settled = eth::send_call_outcome(
        &scenario.url,
        addresses::NOD_FACTORY_ADDR,
        PAYER_KEY,
        &pay,
        None,
    )
    .expect("independent ERC20 settlement");
    assert_mined_success(&settled, "third-party ERC20 settlement");
    let settled_height = finalize(world, &settled);
    assert_relay_transaction(world, &settled, scenario.payer, &pay.abi_encode());
    assert_receipt_event(
        &settled.receipt,
        addresses::NOD_FACTORY_ADDR,
        &eth::INodFactory::NodPaid {
            owner: scenario.owner,
            nodId: scenario.id.to_u256(),
            asset: scenario.asset,
            paymentMinor: scenario.body.settlementCostMinor,
        },
    );
    for &peer in &scenario.ports {
        let peer_url = world.rpc.url(peer);
        let balances_before = asset_balances(
            &peer_url,
            scenario.asset,
            [
                scenario.payer,
                scenario.vault,
                scenario.owner,
                addresses::NOD_FACTORY_ADDR,
            ],
            settled_height - 1,
        );
        let balances_after = asset_balances(
            &peer_url,
            scenario.asset,
            [
                scenario.payer,
                scenario.vault,
                scenario.owner,
                addresses::NOD_FACTORY_ADDR,
            ],
            settled_height,
        );
        assert_payment_delta(
            balances_before,
            balances_after,
            scenario.body.settlementCostMinor,
        );
        let fee = crate::world::rpc::Rpc::receipt_gas_cost(&settled.receipt).expect("payer gas");
        assert_eq!(
            native_balance_at(&peer_url, scenario.payer, settled_height) + fee,
            native_balance_at(&peer_url, scenario.payer, settled_height - 1)
        );
    }
    let paid = nod_bodies(world, scenario.id, settled_height);
    assert_paid_transition(&before, &paid);
    let paid_indexes = nod_snapshot(world, scenario.owner, scenario.id, settled_height);
    assert_snapshot_bodies(&paid_indexes, &paid);
    assert_index_transition(
        &before_indexes.index,
        &paid_indexes.index,
        scenario.id.to_u256(),
        false,
    );
    let duplicate_before = nod_snapshot(world, scenario.owner, scenario.id, settled_height);
    assert_same_snapshot(&paid_indexes, &duplicate_before);
    let duplicate = assert_live_nod_revert(world, &pay, "nod is already settled");
    let duplicate_height = duplicate.block_number().expect("duplicate receipt block");
    assert_eq!(nod_bodies(world, scenario.id, duplicate_height), paid);
    let duplicate_after = nod_snapshot(world, scenario.owner, scenario.id, duplicate_height);
    assert_same_snapshot(&duplicate_before, &duplicate_after);
    for &peer in &scenario.ports {
        let peer_url = world.rpc.url(peer);
        assert_eq!(
            asset_balances(
                &peer_url,
                scenario.asset,
                [
                    scenario.payer,
                    scenario.vault,
                    scenario.owner,
                    addresses::NOD_FACTORY_ADDR
                ],
                duplicate_height - 1
            ),
            asset_balances(
                &peer_url,
                scenario.asset,
                [
                    scenario.payer,
                    scenario.vault,
                    scenario.owner,
                    addresses::NOD_FACTORY_ADDR
                ],
                duplicate_height
            ),
            "duplicate payment rollback"
        );
    }
    PaidNod {
        paid,
        paid_indexes,
        settled,
        duplicate,
        duplicate_height,
    }
}

fn restart_paid_nod(world: &mut World, scenario: &RelayedNod, payment: &PaidNod) {
    if scenario.hardened {
        world
            .localnet
            .restart_validator_and_enclave(0)
            .expect("restart paid NOD reader with sealed keys");
        assert!(world
            .rpc
            .wait_bootstrapped(120, || world.localnet.ensure_committee_alive())
            .unwrap());
        assert_eq!(
            nod_bodies(world, scenario.id, payment.duplicate_height),
            payment.paid,
            "paid encrypted NOD survives restart"
        );
        let reopened = nod_snapshot(world, scenario.owner, scenario.id, payment.duplicate_height);
        assert_same_snapshot(&payment.paid_indexes, &reopened);
        assert_eq!(
            crate::internal::nod_keys::decrypt(world, reopened.body.as_ref().unwrap()),
            scenario.gratis_load
        );
    }
}

fn prepare_mining_attempt(
    world: &World,
    scenario: &RelayedNod,
    payment: &PaidNod,
) -> MiningAttempt {
    let nonce = eth::read_call(
        &scenario.url,
        addresses::GRATIS_ADDR,
        &eth::IGratis::opNonceOfCall {
            account: scenario.owner,
        },
    )
    .expect("owner mint nonce");
    let pow = find_mining_pow_nonce(
        outbe_common::pow::MiningDomain::Nod,
        scenario.id.to_u256(),
        scenario.owner,
    );
    let pair_chain_id = chain_id_b256(world);
    // This is a valid MAC for the payer's own account, never the Nod owner's.
    let wrong_mac = outbe_tee_enclave::gratis::modify_mac(
        &scenario.payer_keys.modify,
        scenario.payer,
        GratisOp::Mint,
        scenario.gratis_load,
        nonce,
        pair_chain_id,
    );
    let mine = eth::INodFactory::mineGratisCall {
        nodId: scenario.id.to_u256(),
        nonce: pow,
        mac: B256::from(wrong_mac),
        opNonce: nonce,
    };
    let rejected_before =
        nod_snapshot(world, scenario.owner, scenario.id, payment.duplicate_height);
    assert_same_snapshot(&payment.paid_indexes, &rejected_before);
    for &peer in &scenario.ports {
        assert_eq!(
            eth::read_call_result(
                &world.rpc.url(peer),
                addresses::GRATIS_ADDR,
                &eth::IGratis::opNonceOfCall {
                    account: scenario.owner
                }
            )
            .expect("owner nonce before wrong MAC"),
            nonce
        );
    }
    let wrong_call = mine.abi_encode();
    let pair_accounts = [
        scenario.payer,
        scenario.vault,
        scenario.owner,
        addresses::NOD_FACTORY_ADDR,
    ];
    let pair_height = live_checkpoint(world, scenario.port).height;
    world
        .rpc
        .wait_finalized_checkpoint(&scenario.ports, pair_height, 120)
        .expect("MAC pair prestate finalized");
    let pair_before = mac_pair_state(world, scenario.asset, pair_accounts, pair_height);
    assert_eq!(pair_before.owner_gratis.1, nonce);
    MiningAttempt {
        mine,
        nonce,
        pair_chain_id,
        wrong_call,
        pair_accounts,
        pair_before,
        rejected_before,
    }
}

fn reject_mining_attempt(
    world: &mut World,
    scenario: &RelayedNod,
    payment: &PaidNod,
    attempt: &MiningAttempt,
) -> RejectedMining {
    if scenario.hardened {
        let mut stale_nonce = attempt.mine.clone();
        stale_nonce.opNonce = attempt.nonce.checked_add(1).unwrap();
        let rejected_nonce = assert_mined_nod_rejection(world, &stale_nonce);
        let height = rejected_nonce.block_number().unwrap();
        assert_eq!(
            mac_pair_state(world, scenario.asset, attempt.pair_accounts, height),
            attempt.pair_before,
            "wrong nonce must preserve both ledgers including Fidelity"
        );
        assert_eq!(nod_bodies(world, scenario.id, height), payment.paid);
    }
    let rejected = assert_mined_nod_rejection(world, &attempt.mine);
    let rejected_height = rejected.block_number().expect("wrong MAC receipt block");
    assert_eq!(
        mac_pair_state(
            world,
            scenario.asset,
            attempt.pair_accounts,
            rejected_height - 1
        ),
        attempt.pair_before,
        "MAC guard inputs changed before the failed transaction"
    );
    let pair_rejected = mac_pair_state(
        world,
        scenario.asset,
        attempt.pair_accounts,
        rejected_height,
    );
    assert_eq!(
        pair_rejected, attempt.pair_before,
        "wrong MAC must roll back raw Gratis, nonces and ERC20"
    );
    assert_eq!(
        nod_bodies(world, scenario.id, rejected_height),
        payment.paid,
        "wrong MAC must preserve the paid item and bucket"
    );
    let rejected_after = nod_snapshot(world, scenario.owner, scenario.id, rejected_height);
    assert_same_snapshot(&attempt.rejected_before, &rejected_after);
    assert_rejected_mining_on_peers(world, scenario, attempt, &rejected);
    RejectedMining {
        rejected,
        rejected_height,
        pair_rejected,
    }
}

fn assert_rejected_mining_on_peers(
    world: &World,
    scenario: &RelayedNod,
    attempt: &MiningAttempt,
    rejected: &crate::world::rpc::TxOutcome,
) {
    let rejected_height = rejected.block_number().expect("wrong MAC receipt block");
    for &peer in &scenario.ports {
        let peer_url = world.rpc.url(peer);
        let fee =
            crate::world::rpc::Rpc::receipt_gas_cost(&rejected.receipt).expect("wrong MAC gas");
        assert_eq!(
            native_balance_at(&peer_url, scenario.payer, rejected_height) + fee,
            native_balance_at(&peer_url, scenario.payer, rejected_height - 1),
            "wrong MAC debits only relayer gas"
        );
        assert_eq!(
            eth::read_call_at_result(
                &peer_url,
                addresses::GRATIS_ADDR,
                &eth::IGratis::opNonceOfCall {
                    account: scenario.owner
                },
                rejected_height
            )
            .expect("owner nonce after wrong MAC"),
            attempt.nonce
        );
        for (account, keys) in [
            (scenario.owner, &scenario.owner_keys),
            (scenario.payer, &scenario.payer_keys),
        ] {
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
}

fn mine_nod(
    world: &mut World,
    scenario: &RelayedNod,
    payment: &PaidNod,
    attempt: &MiningAttempt,
    rejected: &RejectedMining,
) -> MinedNod {
    let mut mine = attempt.mine.clone();
    mine.mac = B256::from(outbe_tee_enclave::gratis::modify_mac(
        &scenario.owner_keys.modify,
        scenario.owner,
        GratisOp::Mint,
        scenario.gratis_load,
        attempt.nonce,
        attempt.pair_chain_id,
    ));
    assert_only_mac_changed(&attempt.wrong_call, &mine);
    assert_eq!(
        chain_id_b256(world),
        attempt.pair_chain_id,
        "MAC chain binding changed"
    );
    let minted_before = nod_snapshot(world, scenario.owner, scenario.id, rejected.rejected_height);
    assert_same_snapshot(&payment.paid_indexes, &minted_before);
    assert_eq!(
        nod_bodies(world, scenario.id, rejected.rejected_height),
        payment.paid,
        "paid bodies changed before the valid retry"
    );
    let retry_height = live_checkpoint(world, scenario.port).height;
    world
        .rpc
        .wait_finalized_checkpoint(&scenario.ports, retry_height, 120)
        .expect("MAC retry prestate finalized");
    assert_eq!(
        mac_pair_state(world, scenario.asset, attempt.pair_accounts, retry_height),
        rejected.pair_rejected,
        "MAC guard inputs changed before the valid retry"
    );
    let minted = eth::send_call_outcome(
        &scenario.url,
        addresses::NOD_FACTORY_ADDR,
        PAYER_KEY,
        &mine,
        None,
    )
    .expect("owner-authorized relayed mining retry");
    assert_mined_success(&minted, "owner-authorized relayed mining");
    let minted_height = finalize(world, &minted);
    assert_eq!(
        mac_pair_state(
            world,
            scenario.asset,
            attempt.pair_accounts,
            minted_height - 1
        ),
        rejected.pair_rejected,
        "MAC guard inputs changed before valid transaction execution"
    );
    let pair_minted = mac_pair_state(world, scenario.asset, attempt.pair_accounts, minted_height);
    assert_eq!(
        pair_minted.owner_gratis.1,
        attempt.nonce.checked_add(1).expect("owner nonce increment")
    );
    assert_eq!(
        pair_minted.payer_gratis, rejected.pair_rejected.payer_gratis,
        "relayer Gratis remains unchanged"
    );
    assert_eq!(
        pair_minted.assets, rejected.pair_rejected.assets,
        "valid retry leaves settlement tokens unchanged"
    );
    assert_mining_gas(world, scenario, &minted);
    let minted_after = nod_snapshot(world, scenario.owner, scenario.id, minted_height);
    assert_index_transition(
        &minted_before.index,
        &minted_after.index,
        scenario.id.to_u256(),
        true,
    );
    assert!(
        minted_after.body.is_none(),
        "mined Nod left the live owner index"
    );
    assert_mining_receipt(world, scenario, &minted, &mine);
    MinedNod {
        mine,
        minted,
        minted_height,
    }
}

fn assert_mining_gas(world: &World, scenario: &RelayedNod, minted: &eth::MinedCallOutcome) {
    for &peer in &scenario.ports {
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
}

fn assert_mining_receipt(
    world: &World,
    scenario: &RelayedNod,
    minted: &eth::MinedCallOutcome,
    mine: &eth::INodFactory::mineGratisCall,
) {
    assert_relay_transaction(world, minted, scenario.payer, &mine.abi_encode());
    assert_receipt_event(
        &minted.receipt,
        addresses::NOD_FACTORY_ADDR,
        &eth::INodFactory::NodExercised {
            owner: scenario.owner,
            nodId: scenario.id.to_u256(),
            encryptedGratisAmount: scenario.body.encryptedGratisAmount.clone(),
        },
    );
    assert_receipt_event(
        &minted.receipt,
        addresses::NOD_FACTORY_ADDR,
        &eth::INodFactory::NodBurned {
            owner: scenario.owner,
            nodId: scenario.id.to_u256(),
            encryptedGratisAmount: scenario.body.encryptedGratisAmount.clone(),
        },
    );
}

fn assert_mined_ledgers(
    world: &World,
    scenario: &RelayedNod,
    payment: &PaidNod,
    minted: &MinedNod,
) {
    for &peer in &scenario.ports {
        let peer_url = world.rpc.url(peer);
        assert_eq!(
            gratis_at(
                &peer_url,
                scenario.owner,
                &scenario.owner_keys.view,
                minted.minted_height
            ),
            gratis_at(
                &peer_url,
                scenario.owner,
                &scenario.owner_keys.view,
                minted.minted_height - 1
            ) + scenario.gratis_load
        );
        assert_eq!(
            gratis_at(
                &peer_url,
                scenario.payer,
                &scenario.payer_keys.view,
                minted.minted_height
            ),
            gratis_at(
                &peer_url,
                scenario.payer,
                &scenario.payer_keys.view,
                minted.minted_height - 1
            )
        );
        assert_eq!(
            asset_balances(
                &peer_url,
                scenario.asset,
                [
                    scenario.payer,
                    scenario.vault,
                    scenario.owner,
                    addresses::NOD_FACTORY_ADDR
                ],
                minted.minted_height - 1
            ),
            asset_balances(
                &peer_url,
                scenario.asset,
                [
                    scenario.payer,
                    scenario.vault,
                    scenario.owner,
                    addresses::NOD_FACTORY_ADDR
                ],
                minted.minted_height
            ),
            "relayed mining must not move settlement tokens"
        );
        let fee = crate::world::rpc::Rpc::receipt_gas_cost(&minted.minted.receipt)
            .expect("relayer mining gas");
        assert_eq!(
            native_balance_at(&peer_url, scenario.payer, minted.minted_height) + fee,
            native_balance_at(&peer_url, scenario.payer, minted.minted_height - 1)
        );
        assert!(
            compressed_body(world, peer, 2, scenario.id, minted.minted_height).is_none(),
            "mined Nod must have authenticated absence"
        );
        // The V2 fixture issued a singleton bucket. Mining its one paid right
        // must remove the bucket instead of leaving an orphan settled counter.
        assert_eq!(payment.paid.1.settled_nods, 1);
        assert!(
            compressed_body(world, peer, 3, scenario.bucket_id, minted.minted_height).is_none(),
            "empty paid bucket must be deleted"
        );
    }
}

fn restart_mined_nod(
    world: &mut World,
    scenario: &RelayedNod,
    attempt: &MiningAttempt,
    minted: &MinedNod,
) {
    if scenario.hardened {
        let stable = mac_pair_state(
            world,
            scenario.asset,
            attempt.pair_accounts,
            minted.minted_height,
        );
        let duplicate_mint = assert_mined_nod_rejection(world, &minted.mine);
        let height = duplicate_mint.block_number().unwrap();
        assert_eq!(
            mac_pair_state(world, scenario.asset, attempt.pair_accounts, height),
            stable
        );
        world
            .localnet
            .restart_validator_and_enclave(0)
            .expect("restart after encrypted NOD mint");
        assert!(world
            .rpc
            .wait_bootstrapped(120, || world.localnet.ensure_committee_alive())
            .unwrap());
        let checkpoint = live_checkpoint(world, scenario.port).height;
        assert_eq!(
            mac_pair_state(world, scenario.asset, attempt.pair_accounts, checkpoint),
            stable
        );
        assert!(
            nod_snapshot(world, scenario.owner, scenario.id, checkpoint)
                .body
                .is_none(),
            "minted NOD resurrected after restart"
        );
    }
}
