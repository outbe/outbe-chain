//! Stablecoin Factory V1 live product flow.

use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use cucumber::{then, when};
use outbe_primitives::{
    addresses::STABLECOIN_ADDRESS_PREFIX, stablecoin::predict_stablecoin,
    stablecoin_fork::STABLECOIN_CREATE_BOND,
};

use crate::{
    internal::{
        addresses,
        eth::{self, IStablecoin, IStablecoinFactory, IStablecoinPolicyRegistry, IVote},
    },
    world::{state::StablecoinFixture, World},
};

const ISSUER_KEY: &str = "0x0000000000000000000000000000000000000000000000000000000000000001";
const SECOND_ISSUER_KEY: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000002";
const RECIPIENT_KEY: &str = "0x0000000000000000000000000000000000000000000000000000000000000003";
const SPENDER_KEY: &str = "0x0000000000000000000000000000000000000000000000000000000000000004";

const TICKER: &str = "E2EUSD";
const TOKEN_NAME: &str = "E2E Dollar";
const ISO_4217_USD: u16 = 840;
const POLICY_WHITELIST: u8 = 2;
const POLICY_ID: u64 = 2;
const STABLECOIN_PROPOSAL_ID: u64 = 1;
const SUPPLY_CAP: u64 = 1_000_000_000_000;
const MINT_AMOUNT: u64 = 1_000_000_000;
const TRANSFER_AMOUNT: u64 = 100_000_000;
const MEMO_TRANSFER_AMOUNT: u64 = 50_000_000;
const PERMIT_AMOUNT: u64 = 40_000_000;
const FROZEN_AMOUNT: u64 = 25_000_000;
const FORCED_AMOUNT: u64 = 20_000_000;

fn ports(world: &World) -> Vec<u16> {
    world.validators.committee_ports()
}

fn primary_url(world: &World) -> String {
    world.rpc.url(world.validators.primary_port())
}

fn finalize_observation(world: &World) -> u64 {
    let primary = world.validators.primary_port();
    let height = world
        .rpc
        .head(primary)
        .expect("stablecoin observation head");
    for port in ports(world) {
        assert!(
            world.rpc.wait_finalized_at_least(port, height, 60),
            "RPC {port} did not finalize stablecoin observation height {height}"
        );
    }
    height
}

fn address(key: &str) -> Address {
    eth::address_of(key).expect("derive stablecoin fixture address")
}

fn wait_for_proposal_votes(world: &World, proposal_id: u64, yes: u64) -> u64 {
    let mut observed = world.rpc.vote_status(proposal_id);
    for _ in 0..12 {
        if observed.yes >= yes || observed.status != "pending" {
            break;
        }
        sleep(Duration::from_secs(2));
        observed = world.rpc.vote_status(proposal_id);
    }
    assert_eq!(observed.status, "pending", "proposal left pending early");
    assert_eq!(observed.yes, yes, "proposal yes tally");
    observed.deadline.expect("pending proposal deadline")
}

fn transaction_fee(receipt: &serde_json::Value) -> U256 {
    fn quantity(receipt: &serde_json::Value, field: &str) -> U256 {
        let value = receipt
            .get(field)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("receipt missing {field}"));
        U256::from_str_radix(value.trim_start_matches("0x"), 16)
            .unwrap_or_else(|_| panic!("invalid receipt quantity {field}: {value}"))
    }
    quantity(receipt, "gasUsed") * quantity(receipt, "effectiveGasPrice")
}

fn log_block_and_transaction(log: &serde_json::Value) -> (u64, String) {
    let block = log
        .get("blockNumber")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
        .expect("log block number");
    let transaction = log
        .get("transactionHash")
        .and_then(serde_json::Value::as_str)
        .expect("log transaction hash")
        .to_owned();
    (block, transaction)
}

fn stablecoin_created_log(world: &World, proposal_id: u64) -> serde_json::Value {
    let signature = IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH;
    let logs = eth::raw_json_with_params(
        &primary_url(world),
        "eth_getLogs",
        serde_json::json!([{
            "address": format!("{:#x}", addresses::STABLECOIN_FACTORY_ADDR),
            "fromBlock": "0x0",
            "toBlock": "finalized",
            "topics": [format!("{signature:#x}")],
        }]),
    )
    .and_then(|value| value.as_array().cloned())
    .expect("StablecoinCreated logs");
    let proposal_word = format!("{proposal_id:064x}");
    let matching = logs
        .into_iter()
        .filter(|log| {
            log.get("data")
                .and_then(serde_json::Value::as_str)
                .and_then(|data| data.strip_prefix("0x"))
                .is_some_and(|data| data.starts_with(&proposal_word))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "proposal {proposal_id} must have one StablecoinCreated log"
    );
    matching.into_iter().next().expect("one creation log")
}

#[when(
    "a funded issuer creates a shared policy and validators approve its bonded stablecoin proposal"
)]
fn create_and_approve_stablecoin(world: &mut World) {
    let funder = world.validators.get(0);
    for (key, amount) in [
        (ISSUER_KEY, 1_000_010),
        (SECOND_ISSUER_KEY, 1_000_010),
        (RECIPIENT_KEY, 10),
        (SPENDER_KEY, 10),
    ] {
        let tx = world
            .rpc
            .fund_key(&funder, key, amount)
            .expect("fund stablecoin fixture account");
        assert!(
            world.rpc.wait_successful_receipt(&tx, 40),
            "funding transaction failed: {tx}"
        );
    }

    let issuer = address(ISSUER_KEY);
    let second_issuer = address(SECOND_ISSUER_KEY);
    let recipient = address(RECIPIENT_KEY);
    let spender = address(SPENDER_KEY);
    let url = primary_url(world);

    eth::send_call(
        &url,
        addresses::STABLECOIN_POLICY_ADDR,
        ISSUER_KEY,
        &IStablecoinPolicyRegistry::createPolicyCall {
            policyType: POLICY_WHITELIST,
            admin: issuer,
        },
        None,
    )
    .expect("create whitelist policy");
    let policy_id = U256::from(POLICY_ID);
    assert_eq!(
        eth::read_call(
            &url,
            addresses::STABLECOIN_POLICY_ADDR,
            &IStablecoinPolicyRegistry::policyExistsCall {
                policyId: policy_id
            },
        ),
        Some(true)
    );
    eth::send_call(
        &url,
        addresses::STABLECOIN_POLICY_ADDR,
        ISSUER_KEY,
        &IStablecoinPolicyRegistry::addMembersCall {
            policyId: policy_id,
            accounts: vec![issuer, second_issuer, recipient, spender],
        },
        None,
    )
    .expect("add whitelist members");

    let chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .expect("chain id");
    let (token_id, token) = predict_stablecoin(
        chain_id,
        addresses::STABLECOIN_FACTORY_ADDR,
        issuer,
        TICKER,
        STABLECOIN_ADDRESS_PREFIX,
    )
    .expect("predict stablecoin");
    let balance_before = eth::balance(&url, issuer).expect("issuer balance before proposal");
    let proposal_tx = world
        .rpc
        .stablecoin_propose(
            ISSUER_KEY,
            TOKEN_NAME,
            TICKER,
            ISO_4217_USD,
            U256::from(SUPPLY_CAP),
            policy_id,
        )
        .expect("submit stablecoin proposal");
    assert!(
        world.rpc.wait_successful_receipt(&proposal_tx, 40),
        "stablecoin proposal transaction failed: {proposal_tx}"
    );
    let proposal_receipt =
        eth::receipt_json(&url, &proposal_tx).expect("stablecoin proposal receipt");

    for index in 0..3 {
        let validator = world.validators.get(index);
        let tx = world
            .rpc
            .cast_vote(&validator, STABLECOIN_PROPOSAL_ID, true)
            .expect("cast stablecoin vote");
        assert!(
            world.rpc.wait_successful_receipt(&tx, 40),
            "stablecoin vote failed: {tx}"
        );
    }
    let deadline = wait_for_proposal_votes(world, STABLECOIN_PROPOSAL_ID, 3);
    assert!(
        world
            .rpc
            .wait_block_gt(world.validators.primary_port(), deadline, 80)
            .is_some(),
        "did not pass stablecoin proposal deadline {deadline}"
    );
    assert!(
        world
            .rpc
            .wait_vote_status(STABLECOIN_PROPOSAL_ID, "approved", 60),
        "stablecoin proposal was not approved"
    );
    finalize_observation(world);

    let balance_after = eth::balance(&url, issuer).expect("issuer balance after approval");
    assert_eq!(
        balance_after + transaction_fee(&proposal_receipt),
        balance_before,
        "approved proposal did not refund the exact bond"
    );
    let bond = eth::read_call(
        &url,
        addresses::VOTE_ADDR,
        &IVote::getProposalBondCall {
            proposalId: U256::from(STABLECOIN_PROPOSAL_ID),
        },
    )
    .expect("proposal bond");
    assert_eq!(bond.amount, STABLECOIN_CREATE_BOND);
    assert_eq!(bond.settlement, IVote::BondSettlement::Refunded);
    assert_eq!(
        eth::read_call(
            &url,
            addresses::VOTE_ADDR,
            &IVote::unsettledBondLiabilitiesCall {},
        ),
        Some(U256::ZERO)
    );

    let created = stablecoin_created_log(world, STABLECOIN_PROPOSAL_ID);
    let (created_block, hook_tx) = log_block_and_transaction(&created);
    let hook_receipt = eth::receipt_json(&url, &hook_tx).expect("HookEvents receipt");
    assert_eq!(
        hook_receipt
            .get("status")
            .and_then(serde_json::Value::as_str),
        Some("0x1")
    );
    let receipt_logs = hook_receipt
        .get("logs")
        .and_then(serde_json::Value::as_array)
        .expect("HookEvents receipt logs");
    assert_eq!(
        receipt_logs
            .iter()
            .filter(|log| {
                log.get("address")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|address| {
                        address.eq_ignore_ascii_case(&format!(
                            "{:#x}",
                            addresses::STABLECOIN_FACTORY_ADDR
                        ))
                    })
            })
            .count(),
        1
    );

    world.state.stablecoin = Some(StablecoinFixture {
        issuer_key: ISSUER_KEY.to_owned(),
        second_issuer_key: SECOND_ISSUER_KEY.to_owned(),
        recipient_key: RECIPIENT_KEY.to_owned(),
        spender_key: SPENDER_KEY.to_owned(),
        issuer,
        second_issuer,
        recipient,
        spender,
        policy_id,
        proposal_id: STABLECOIN_PROPOSAL_ID,
        token_id,
        token,
        created_block,
        snapshot_height: None,
        snapshot_supply: None,
        snapshot_balances: None,
    });
}

#[then("the token is registered with one HookEvents creation receipt and the bond is refunded")]
fn token_registered(world: &mut World) {
    finalize_observation(world);
    let fixture = world.state.stablecoin.as_ref().expect("stablecoin fixture");
    for port in ports(world) {
        let url = world.rpc.url(port);
        assert_eq!(
            eth::read_call(
                &url,
                addresses::STABLECOIN_FACTORY_ADDR,
                &IStablecoinFactory::tokenCountCall {},
            ),
            Some(U256::from(1))
        );
        assert_eq!(
            eth::read_call(
                &url,
                addresses::STABLECOIN_FACTORY_ADDR,
                &IStablecoinFactory::tokenByIdCall {
                    tokenId: fixture.token_id,
                },
            ),
            Some(fixture.token)
        );
        assert_eq!(
            eth::read_call(
                &url,
                addresses::STABLECOIN_FACTORY_ADDR,
                &IStablecoinFactory::tokenByTickerCall {
                    ticker: TICKER.to_owned(),
                },
            ),
            Some(fixture.token)
        );
        assert_eq!(
            eth::code(&url, fixture.token).map(|code| code.to_vec()),
            Some(vec![0xef]),
            "token marker on RPC {port}"
        );
    }
}

fn sign_permit(
    key: &str,
    domain: B256,
    owner: Address,
    spender: Address,
    value: U256,
    nonce: U256,
    deadline: U256,
) -> (u8, B256, B256) {
    let typehash = keccak256(
        "Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)",
    );
    let struct_hash = keccak256((typehash, owner, spender, value, nonce, deadline).abi_encode());
    let mut preimage = [0u8; 66];
    preimage[..2].copy_from_slice(&[0x19, 0x01]);
    preimage[2..34].copy_from_slice(domain.as_slice());
    preimage[34..].copy_from_slice(struct_hash.as_slice());
    let signer: PrivateKeySigner = key.parse().expect("permit signer");
    assert_eq!(signer.address(), owner);
    let signature = signer
        .sign_hash_sync(&keccak256(preimage))
        .expect("sign permit");
    let v = 27 + u8::from(signature.v());
    (
        v,
        B256::from(signature.r().to_be_bytes::<32>()),
        B256::from(signature.s().to_be_bytes::<32>()),
    )
}

fn receipt_has_event(url: &str, tx: &str, signature: B256) -> bool {
    eth::receipt_json(url, tx)
        .and_then(|receipt| receipt.get("logs").cloned())
        .and_then(|logs| logs.as_array().cloned())
        .is_some_and(|logs| {
            logs.iter().any(|log| {
                log.get("topics")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|topics| topics.first())
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|topic| topic.eq_ignore_ascii_case(&format!("{signature:#x}")))
            })
        })
}

#[when("the issuer exercises mint transfer permit memo pause freeze and forced transfer")]
fn exercise_ledger(world: &mut World) {
    let fixture = world.state.stablecoin.as_ref().expect("stablecoin fixture");
    let url = primary_url(world);

    eth::send_call(
        &url,
        fixture.token,
        &fixture.issuer_key,
        &IStablecoin::mintCall {
            to: fixture.issuer,
            amount: U256::from(MINT_AMOUNT),
        },
        None,
    )
    .expect("mint stablecoin");
    eth::send_call(
        &url,
        fixture.token,
        &fixture.issuer_key,
        &IStablecoin::transferCall {
            to: fixture.recipient,
            value: U256::from(TRANSFER_AMOUNT),
        },
        None,
    )
    .expect("transfer stablecoin");

    let domain = eth::read_call(&url, fixture.token, &IStablecoin::DOMAIN_SEPARATORCall {})
        .expect("stablecoin domain separator");
    let nonce = eth::read_call(
        &url,
        fixture.token,
        &IStablecoin::noncesCall {
            owner: fixture.issuer,
        },
    )
    .expect("stablecoin permit nonce");
    let deadline = U256::from(
        world
            .rpc
            .latest_block_timestamp(world.validators.primary_port())
            .expect("latest block timestamp")
            + 3_600,
    );
    let (v, r, s) = sign_permit(
        &fixture.issuer_key,
        domain,
        fixture.issuer,
        fixture.spender,
        U256::from(PERMIT_AMOUNT),
        nonce,
        deadline,
    );
    eth::send_call(
        &url,
        fixture.token,
        &fixture.spender_key,
        &IStablecoin::permitCall {
            owner: fixture.issuer,
            spender: fixture.spender,
            value: U256::from(PERMIT_AMOUNT),
            deadline,
            v,
            r,
            s,
        },
        None,
    )
    .expect("submit stablecoin permit");

    let memo = keccak256("stablecoin-e2e-memo");
    let memo_tx = eth::send_call(
        &url,
        fixture.token,
        &fixture.issuer_key,
        &IStablecoin::transferWithMemoCall {
            to: fixture.recipient,
            amount: U256::from(MEMO_TRANSFER_AMOUNT),
            memo,
        },
        None,
    )
    .expect("transfer with memo");
    assert!(receipt_has_event(
        &url,
        &memo_tx,
        IStablecoin::TransferWithMemo::SIGNATURE_HASH
    ));

    eth::send_call(
        &url,
        fixture.token,
        &fixture.issuer_key,
        &IStablecoin::pauseCall {},
        None,
    )
    .expect("pause token");
    let paused_transfer = IStablecoin::transferCall {
        to: fixture.recipient,
        value: U256::from(1),
    };
    let paused_error = eth::raw_json_result(
        &url,
        "eth_call",
        serde_json::json!([{
            "from": format!("{:#x}", fixture.issuer),
            "to": format!("{:#x}", fixture.token),
            "data": format!("0x{}", hex::encode(paused_transfer.abi_encode())),
        }, "latest"]),
    )
    .expect_err("paused transfer must revert");
    assert!(
        paused_error.to_string().contains("revert"),
        "unexpected paused transfer error: {paused_error:#}"
    );
    eth::send_call(
        &url,
        fixture.token,
        &fixture.issuer_key,
        &IStablecoin::unpauseCall {},
        None,
    )
    .expect("unpause token");
    eth::send_call(
        &url,
        fixture.token,
        &fixture.issuer_key,
        &IStablecoin::setFrozenTokensCall {
            account: fixture.recipient,
            amount: U256::from(FROZEN_AMOUNT),
        },
        None,
    )
    .expect("freeze recipient balance");
    eth::send_call(
        &url,
        fixture.token,
        &fixture.issuer_key,
        &IStablecoin::forcedTransferCall {
            from: fixture.recipient,
            to: fixture.issuer,
            value: U256::from(FORCED_AMOUNT),
        },
        None,
    )
    .expect("forced transfer");
}

fn token_state(url: &str, fixture: &StablecoinFixture) -> (U256, [U256; 3]) {
    let supply = eth::read_call(url, fixture.token, &IStablecoin::totalSupplyCall {})
        .expect("stablecoin supply");
    let balances = [
        eth::read_call(
            url,
            fixture.token,
            &IStablecoin::balanceOfCall {
                account: fixture.issuer,
            },
        )
        .expect("issuer token balance"),
        eth::read_call(
            url,
            fixture.token,
            &IStablecoin::balanceOfCall {
                account: fixture.recipient,
            },
        )
        .expect("recipient token balance"),
        eth::read_call(
            url,
            fixture.token,
            &IStablecoin::balanceOfCall {
                account: fixture.spender,
            },
        )
        .expect("spender token balance"),
    ];
    (supply, balances)
}

#[then("the stablecoin ledger has the expected final state on every validator")]
fn ledger_state(world: &mut World) {
    finalize_observation(world);
    let fixture = world.state.stablecoin.as_ref().expect("stablecoin fixture");
    let expected_supply = U256::from(MINT_AMOUNT);
    let expected_balances = [
        U256::from(MINT_AMOUNT - TRANSFER_AMOUNT - MEMO_TRANSFER_AMOUNT + FORCED_AMOUNT),
        U256::from(TRANSFER_AMOUNT + MEMO_TRANSFER_AMOUNT - FORCED_AMOUNT),
        U256::ZERO,
    ];
    for port in ports(world) {
        let url = world.rpc.url(port);
        assert_eq!(
            token_state(&url, fixture),
            (expected_supply, expected_balances),
            "token state on RPC {port}"
        );
        assert_eq!(
            eth::read_call(
                &url,
                fixture.token,
                &IStablecoin::allowanceCall {
                    owner: fixture.issuer,
                    spender: fixture.spender,
                },
            ),
            Some(U256::from(PERMIT_AMOUNT))
        );
        assert_eq!(
            eth::read_call(
                &url,
                fixture.token,
                &IStablecoin::noncesCall {
                    owner: fixture.issuer,
                },
            ),
            Some(U256::from(1))
        );
        assert_eq!(
            eth::read_call(
                &url,
                fixture.token,
                &IStablecoin::getFrozenTokensCall {
                    account: fixture.recipient,
                },
            ),
            Some(U256::from(FROZEN_AMOUNT))
        );
        assert_eq!(
            eth::read_call(&url, fixture.token, &IStablecoin::pausedCall {}),
            Some(false)
        );
    }
}

#[when("a second issuer proposes the same global ticker")]
fn duplicate_global_ticker(world: &mut World) {
    let fixture = world.state.stablecoin.as_ref().expect("stablecoin fixture");
    let before = eth::balance(&primary_url(world), fixture.second_issuer)
        .expect("second issuer native balance before rejection");
    let error = world
        .rpc
        .stablecoin_propose_rejection(
            &fixture.second_issuer_key,
            "Duplicate E2E Dollar",
            TICKER,
            ISO_4217_USD,
            U256::from(SUPPLY_CAP),
            fixture.policy_id,
        )
        .expect("duplicate ticker must fail during RPC preflight");
    assert!(
        !error.trim().is_empty(),
        "duplicate ticker returned no error"
    );
    let after = eth::balance(&primary_url(world), fixture.second_issuer)
        .expect("second issuer native balance after rejection");
    assert_eq!(after, before, "rejected proposal debited second issuer");
}

#[then("the duplicate ticker is rejected without a proposal or bond debit")]
fn duplicate_ticker_preserves_state(world: &mut World) {
    finalize_observation(world);
    let fixture = world.state.stablecoin.as_ref().expect("stablecoin fixture");
    assert!(
        !world.rpc.vote_status(fixture.proposal_id + 1).visible,
        "duplicate ticker allocated a Vote proposal"
    );
    for port in ports(world) {
        let url = world.rpc.url(port);
        assert_eq!(
            eth::read_call(
                &url,
                addresses::STABLECOIN_FACTORY_ADDR,
                &IStablecoinFactory::tokenCountCall {},
            ),
            Some(U256::from(1))
        );
        assert_eq!(
            eth::read_call(
                &url,
                addresses::VOTE_ADDR,
                &IVote::unsettledBondLiabilitiesCall {},
            ),
            Some(U256::ZERO)
        );
    }
}

fn historical_token_state(
    url: &str,
    fixture: &StablecoinFixture,
    height: u64,
) -> (U256, [U256; 3]) {
    let supply = eth::read_call_at(url, fixture.token, &IStablecoin::totalSupplyCall {}, height)
        .expect("historical stablecoin supply");
    let balances = [
        eth::read_call_at(
            url,
            fixture.token,
            &IStablecoin::balanceOfCall {
                account: fixture.issuer,
            },
            height,
        )
        .expect("historical issuer balance"),
        eth::read_call_at(
            url,
            fixture.token,
            &IStablecoin::balanceOfCall {
                account: fixture.recipient,
            },
            height,
        )
        .expect("historical recipient balance"),
        eth::read_call_at(
            url,
            fixture.token,
            &IStablecoin::balanceOfCall {
                account: fixture.spender,
            },
            height,
        )
        .expect("historical spender balance"),
    ];
    (supply, balances)
}

#[when("the entire stablecoin committee restarts")]
fn restart_committee(world: &mut World) {
    let primary = world.validators.primary_port();
    let height = world
        .rpc
        .finalized(primary)
        .expect("stablecoin snapshot finalized height");
    let url = world.rpc.url(primary);
    let fixture = world.state.stablecoin.as_mut().expect("stablecoin fixture");
    let (supply, balances) = historical_token_state(&url, fixture, height);
    fixture.snapshot_height = Some(height);
    fixture.snapshot_supply = Some(supply);
    fixture.snapshot_balances = Some(balances);

    world
        .localnet
        .restart_committee_and_enclaves()
        .expect("restart stablecoin committee");
    for port in ports(world) {
        assert!(
            world.rpc.wait_block(port, height, 90).is_some(),
            "RPC {port} did not recover stablecoin height {height}"
        );
    }
}

#[then("stablecoin historical and current reads remain identical on every validator")]
fn historical_and_current_parity(world: &mut World) {
    let fixture = world.state.stablecoin.as_ref().expect("stablecoin fixture");
    let height = fixture.snapshot_height.expect("stablecoin snapshot height");
    let expected = (
        fixture.snapshot_supply.expect("stablecoin snapshot supply"),
        fixture
            .snapshot_balances
            .expect("stablecoin snapshot balances"),
    );
    for port in ports(world) {
        let url = world.rpc.url(port);
        assert_eq!(
            historical_token_state(&url, fixture, height),
            expected,
            "historical token state on RPC {port}"
        );
        assert_eq!(
            token_state(&url, fixture),
            expected,
            "current token state on RPC {port}"
        );
        assert_eq!(
            eth::code(&url, fixture.token).map(|code| code.to_vec()),
            Some(vec![0xef]),
            "token marker after restart on RPC {port}"
        );
        assert_eq!(
            eth::read_call(
                &url,
                addresses::STABLECOIN_FACTORY_ADDR,
                &IStablecoinFactory::tokenByIdCall {
                    tokenId: fixture.token_id,
                },
            ),
            Some(fixture.token)
        );
    }
}
