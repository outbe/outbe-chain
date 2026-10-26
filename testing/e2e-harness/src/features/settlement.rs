//! Release E2E evidence for the existing Gem and Nod settlement/redemption paths.

mod paid_gem;
mod sponsored_gem;

#[path = "settlement_nod.rs"]
mod erc20_nod;

pub(crate) fn mine_encrypted_nod(world: &mut crate::world::World, day: u32) {
    erc20_nod::run_relayed_mining(world, 0, day, true);
}

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::sol;
use cucumber::{then, when};
use outbe_primitives::storage::types::StorageKey as _;
use outbe_primitives::units::checked_protocol_to_native;
use outbe_tee::protocol::{GratisOp, Ledger, PromisOp};

use crate::env::environment;
use crate::internal::{addresses, eth};
use crate::world::forge::{self, address_from, DEPLOYER_KEY};
use crate::world::World;

const USD_ISO: u16 = 840;
const REWARD_GEM_TIMEOUT_SECS: u64 = 900;
const MATERIALIZED_NOD_TIMEOUT_SECS: u64 = 60;

sol! {
    interface ISettlementAsset {
        function mint(address to, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
        function decimals() external view returns (uint8);
        function isoCode() external view returns (uint16);
    }

    interface ISettlementVault {
        function asset() external view returns (address);
        function owner() external view returns (address);
        function balanceOf(address account) external view returns (uint256);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SettlementFixture {
    pub(crate) asset: Address,
    pub(crate) vault: Address,
}

#[then("validator 0 claims its validator reward as a Gem")]
fn validator_claims_reward_gem(world: &mut World) {
    let reward = validator_reward_gem(world);
    assert!(
        !reward.gem.promisLoadMinor.is_zero(),
        "reward Gem load must be non-zero"
    );
    assert!(
        !reward.gem.entryPriceMinor.is_zero(),
        "reward Gem entry price must be non-zero"
    );
    eprintln!(
        "settlement_evidence kind=reward_gem owner={:#x} gem_id={} load={} entry={} claim_block_number={}",
        reward.owner, reward.gem_id, reward.gem.promisLoadMinor, reward.gem.entryPriceMinor, reward.block_number,
    );
}

#[then("validator 0 with zero COEN uses ZeroFee to redeem that Gem into exact COEN")]
fn validator_redeems_reward_gem(world: &mut World) {
    sponsored_gem::redeem(world);
}

#[then("validator 0 settles its protocol reward Gem and redeems its exact Promis into COEN")]
fn validator_redeems_reward_gem_with_paid_transactions(world: &mut World) {
    paid_gem::redeem(world);
}

fn promis_balance_at(url: &str, owner: Address, view_key: &[u8; 32], height: u64) -> U256 {
    let blob = eth::read_call_at_result(
        url,
        addresses::PROMIS_ADDR,
        &eth::IPromis::balanceOfCall { account: owner },
        height,
    )
    .expect("finalized Promis ciphertext");
    decrypted_balance_or_zero(blob.as_ref(), |ciphertext| {
        outbe_tee_enclave::promis::decrypt_balance(view_key, owner, ciphertext)
            .expect("decrypt finalized Promis balance")
    })
}

fn decrypted_balance_or_zero(ciphertext: &[u8], decrypt: impl FnOnce(&[u8]) -> U256) -> U256 {
    if ciphertext.is_empty() {
        U256::ZERO
    } else {
        decrypt(ciphertext)
    }
}

fn native_balance_at(url: &str, owner: Address, height: u64) -> U256 {
    let value = eth::raw_json_result(
        url,
        "eth_getBalance",
        serde_json::json!([format!("{owner:#x}"), format!("{height:#x}")]),
    )
    .expect("native balance RPC must succeed");
    serde_json::from_value(value).expect("native balance must decode")
}

pub(crate) fn assert_receipt_event<E: alloy_sol_types::SolEvent>(
    receipt: &serde_json::Value,
    emitter: Address,
    event: &E,
) {
    let expected = event.encode_log_data();
    let topics: Vec<String> = expected
        .topics()
        .iter()
        .map(|topic| format!("{topic:#x}"))
        .collect();
    let logs = receipt["logs"].as_array().expect("receipt logs");
    let matches = logs
        .iter()
        .filter(|log| {
            log["address"] == serde_json::json!(format!("{emitter:#x}"))
                && log["topics"] == serde_json::json!(topics)
                && log["data"] == serde_json::json!(format!("0x{}", hex::encode(&expected.data)))
        })
        .count();
    assert_eq!(
        matches,
        1,
        "receipt must contain exactly one matching {} event",
        E::SIGNATURE
    );
}

#[when("the feeder publishes a fresh quote before the next UTC day")]
fn publish_fresh_quote(world: &mut World) {
    let key = world
        .validators
        .get(0)
        .evm_key()
        .expect("public Tribute owner key");
    let owner = world
        .rpc
        .address_of(&key)
        .expect("public Tribute owner address")
        .parse::<Address>()
        .expect("canonical owner address");
    let (_, body) = wait_for_materialized_nod(world, world.validators.primary_port(), owner);
    // Publish before the scenario's existing V2 day transition, with room for earlier samples.
    let rate = body
        .entryPriceMinor
        .checked_mul(U256::from(3))
        .expect("the quote fits a scale-6 amount");
    assert!(rate > body.entryPriceMinor);
    crate::features::price_oracle::publish_controlled_quote(world, rate);
}

#[then("the public Tribute owner settles its Nod and redeems its exact Gratis into COEN")]
fn owner_redeems_materialized_nod(world: &mut World) {
    let key = world
        .validators
        .get(0)
        .evm_key()
        .expect("public Tribute owner key");
    let owner = world
        .rpc
        .address_of(&key)
        .expect("public Tribute owner address")
        .parse::<Address>()
        .expect("canonical public Tribute owner address");
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let (nod_id, body) = wait_for_materialized_nod(world, port, owner);
    assert!(
        !body.settlementCostMinor.is_zero(),
        "settlement E2E requires a Nod with a nonzero cost"
    );
    let gratis_load = crate::internal::nod_keys::decrypt(world, &body);
    assert!(!gratis_load.is_zero());

    let fixture = deploy_settlement_fixture(world);
    let nod_word = U256::from_be_slice(&nod_id);
    let quote = eth::read_call(
        &url,
        addresses::NOD_FACTORY_ADDR,
        &eth::INodFactory::quoteSettlementCall {
            nodId: nod_word,
            asset: fixture.asset,
        },
    )
    .expect("quote settling the materialized Nod");
    assert_eq!(quote.paymentMinor, body.settlementCostMinor);
    fund_and_approve(
        world,
        crate::features::settlement::SettlementFunding {
            asset: fixture.asset,
            owner_key: &key,
            owner,
            spender: addresses::NOD_FACTORY_ADDR,
            amount: quote.paymentMinor,
        },
    );

    let settlement = eth::send_call_outcome(
        &url,
        addresses::NOD_FACTORY_ADDR,
        &key,
        &eth::INodFactory::settleNodCall {
            nodId: nod_word,
            asset: fixture.asset,
            snapshotId: quote.snapshotId,
        },
        None,
    )
    .expect("settle Nod by ERC20");
    assert_mined_success(&settlement, "settle Nod by ERC20");
    assert_eq!(
        eth::read_call(
            &url,
            fixture.asset,
            &ISettlementAsset::balanceOfCall {
                account: fixture.vault,
            },
        ),
        Some(body.settlementCostMinor),
        "reserve vault did not receive exact Nod cost"
    );

    let keys = eth::derive_account_keys(&url, &key, Ledger::Gratis)
        .expect("derive public Tribute owner Gratis keys");
    let gratis_before = gratis_balance(&url, owner, &keys.view);
    let mint_nonce = eth::read_call(
        &url,
        addresses::GRATIS_ADDR,
        &eth::IGratis::opNonceOfCall { account: owner },
    )
    .expect("Gratis nonce before Nod mining");
    let chain_id = chain_id_b256(world);
    let mint_mac = outbe_tee_enclave::gratis::modify_mac(
        &keys.modify,
        &outbe_tee_enclave::gratis::ModifyOperation {
            account: owner,
            op: GratisOp::Mint,
            amount: gratis_load,
            op_nonce: mint_nonce,
            chain_id,
        },
    );
    let pow = find_mining_pow_nonce(
        outbe_common::pow::MiningDomain::Nod,
        U256::from_be_slice(&nod_id),
        owner,
    );
    let mine_gratis = eth::send_call_outcome(
        &url,
        addresses::NOD_FACTORY_ADDR,
        &key,
        &eth::INodFactory::mineGratisCall {
            nodId: U256::from_be_slice(&nod_id),
            nonce: pow,
            mac: B256::from(mint_mac),
            opNonce: mint_nonce,
        },
        None,
    )
    .expect("mine the paid Nod");
    assert_mined_success(&mine_gratis, "mine the paid Nod");
    assert_eq!(
        gratis_balance(&url, owner, &keys.view),
        gratis_before + gratis_load,
        "Nod load was not minted exactly into owner Gratis"
    );

    let burn_nonce = eth::read_call(
        &url,
        addresses::GRATIS_ADDR,
        &eth::IGratis::opNonceOfCall { account: owner },
    )
    .expect("Gratis nonce before COEN mining");
    let burn_mac = outbe_tee_enclave::gratis::modify_mac(
        &keys.modify,
        &outbe_tee_enclave::gratis::ModifyOperation {
            account: owner,
            op: GratisOp::Burn,
            amount: gratis_load,
            op_nonce: burn_nonce,
            chain_id,
        },
    );
    let native_before = eth::balance(&url, owner).expect("native balance before Gratis burn");
    let mine_coen = eth::send_call_outcome(
        &url,
        addresses::GRATIS_FACTORY_ADDR,
        &key,
        &eth::IGratisFactory::mineCoenCall {
            gratisMinor: gratis_load,
            mac: B256::from(burn_mac),
            opNonce: burn_nonce,
        },
        None,
    )
    .expect("mine COEN from public Tribute owner Gratis");
    assert!(
        mine_coen.success,
        "mine COEN from public Tribute owner Gratis reverted: {}",
        mine_coen.receipt
    );
    let fee =
        crate::world::rpc::Rpc::receipt_gas_cost(&mine_coen.receipt).expect("Gratis burn gas cost");
    let native_after = eth::balance(&url, owner).expect("native balance after Gratis burn");
    assert_eq!(gratis_balance(&url, owner, &keys.view), gratis_before);
    assert_eq!(
        native_after + fee,
        native_before
            + checked_protocol_to_native(gratis_load).expect("Gratis load fits native COEN")
    );
    eprintln!(
        "settlement_evidence kind=nod_to_coen owner={owner:#x} nod_id=0x{} asset={:#x} vault={:#x} cost={} gratis={} tx={} native_before={} native_after={} gas={fee}",
        hex::encode(&nod_id), fixture.asset, fixture.vault, body.settlementCostMinor,
        gratis_load, mine_coen.transaction_hash, native_before, native_after
    );
}

fn wait_for_materialized_nod(
    world: &World,
    port: u16,
    owner: Address,
) -> (Vec<u8>, crate::internal::eth::INod::NodData) {
    let deadline = Instant::now() + Duration::from_secs(MATERIALIZED_NOD_TIMEOUT_SECS);
    loop {
        match world.rpc.nod_for_owner(port, owner) {
            Ok(Some(nod)) => return nod,
            Ok(None) if Instant::now() < deadline => {}
            Err(_) if Instant::now() < deadline => {}
            Ok(None) => panic!(
                "public Tribute owner's materialized Nod did not appear within {MATERIALIZED_NOD_TIMEOUT_SECS}s: owner={owner:#x} head={:?} finalized={:?}",
                world.rpc.head(port),
                world.rpc.finalized(port)
            ),
            Err(error) => panic!(
                "public Tribute owner's materialized Nod remained unreadable after {MATERIALIZED_NOD_TIMEOUT_SECS}s: owner={owner:#x} error={error} head={:?} finalized={:?}",
                world.rpc.head(port),
                world.rpc.finalized(port)
            ),
        }
        sleep(Duration::from_millis(250));
    }
}

pub(crate) fn deploy_settlement_fixture(world: &World) -> SettlementFixture {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let funder = world.validators.get(0);
    let funding = world
        .rpc
        .fund_key(&funder, DEPLOYER_KEY, 1_000)
        .expect("fund settlement fixture deployer");
    assert!(world.rpc.wait_successful_receipt(&funding, 60));

    let intex = environment().repo.join("contracts/intex");
    let asset = address_from(
        &forge::run_with_ctor(
            &intex,
            &[
                "create",
                "test/mocks/MockReferenceStablecoin.sol:MockReferenceStablecoin",
            ],
            &[&USD_ISO.to_string()],
            &[],
            &url,
        )
        .expect("deploy reference stablecoin fixture"),
        "Deployed to:",
    )
    .expect("reference stablecoin address");
    let vault = address_from(
        &forge::run_with_ctor(
            &intex,
            &[
                "create",
                "test/mocks/MockSettlementVault.sol:MockSettlementVault",
            ],
            &[&format!("{asset:#x}")],
            &[],
            &url,
        )
        .expect("deploy settlement vault fixture"),
        "Deployed to:",
    )
    .expect("settlement vault address");

    assert_eq!(
        eth::read_call(&url, asset, &ISettlementAsset::decimalsCall {}),
        Some(6)
    );
    assert_eq!(
        eth::read_call(&url, asset, &ISettlementAsset::isoCodeCall {}),
        Some(USD_ISO)
    );
    assert_eq!(
        eth::read_call(&url, vault, &ISettlementVault::ownerCall {}),
        Some(Address::ZERO)
    );
    assert_eq!(
        eth::read_call(&url, vault, &ISettlementVault::assetCall {}),
        Some(asset)
    );

    crate::world::settlement_currency::register_vault(world, asset, vault, USD_ISO)
        .expect("approve and apply settlement vault registration");
    assert_eq!(
        eth::read_call(
            &url,
            addresses::VAULT_ROUTER_ADDR,
            &eth::IVaultRouter::referenceCurrencyAssetsCall {
                isoCode: USD_ISO,
                offset: U256::ZERO,
                limit: U256::MAX,
            },
        ),
        Some(vec![asset])
    );
    SettlementFixture { asset, vault }
}

pub(crate) fn fund_and_approve(world: &World, funding: SettlementFunding<'_>) {
    let SettlementFunding {
        asset,
        owner_key,
        owner,
        spender,
        amount,
    } = funding;
    let url = world.rpc.url(world.validators.primary_port());
    let mint = eth::send_call(
        &url,
        asset,
        DEPLOYER_KEY,
        &ISettlementAsset::mintCall { to: owner, amount },
        None,
    )
    .expect("mint exact settlement amount");
    assert_success(&url, &mint, "mint exact settlement amount");
    let approve = eth::send_call(
        &url,
        asset,
        owner_key,
        &ISettlementAsset::approveCall { spender, amount },
        None,
    )
    .expect("approve settlement factory");
    assert_success(&url, &approve, "approve settlement factory");
}

#[derive(Clone)]
pub(crate) struct ValidatorRewardGem {
    pub(crate) owner: Address,
    pub(crate) gem_id: U256,
    pub(crate) gem: eth::IGem::GemData,
    /// Block of the claim that issued the Gem.
    pub(crate) block_number: u64,
}

impl std::fmt::Debug for ValidatorRewardGem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidatorRewardGem")
            .field("owner", &self.owner)
            .field("gem_id", &self.gem_id)
            .field("block_number", &self.block_number)
            .finish_non_exhaustive()
    }
}

/// Validator 0's reward Gem: the one it already claimed, or a fresh claim of its
/// whole validator pool once the day's settlement has credited it.
fn validator_reward_gem(world: &mut World) -> ValidatorRewardGem {
    if let Some(reward) = world.state.validator_reward_gem.clone() {
        return reward;
    }
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let key = world.validators.get(0).evm_key().expect("validator 0 key");
    let owner = validator_reward_owner(world);
    let deadline = Instant::now() + Duration::from_secs(REWARD_GEM_TIMEOUT_SECS);
    // The claim prices on the previous UTC day's VWAP, which can land after the credit.
    let receipt = loop {
        let claimable = eth::read_call(
            &url,
            addresses::AGENT_REWARD_ADDR,
            &eth::IAgentReward::getPoolClaimableBalanceCall {
                account: owner,
                pool: eth::VALIDATOR_POOL,
            },
        );
        let last_error = if claimable.is_some_and(|claimable| !claimable.is_zero()) {
            match world
                .rpc
                .claim_agent_reward_gem(&key, eth::VALIDATOR_POOL, USD_ISO, USD_ISO)
            {
                Ok(receipt) => break receipt,
                Err(error) => Some(error),
            }
        } else {
            None
        };
        assert!(
            Instant::now() < deadline,
            "validator 0 did not claim its validator reward; last_error={last_error:?} head={:?} finalized={:?}",
            world.rpc.head(port),
            world.rpc.finalized(port)
        );
        sleep(Duration::from_millis(500));
    };
    let issued =
        eth::receipt_event::<eth::IGemFactory::GemIssued>(&receipt, addresses::GEM_FACTORY_ADDR);
    let block_number = receipt
        .get("blockNumber")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
        .expect("validator reward claim receipt block");
    let gem = eth::read_call(
        &url,
        addresses::GEM_ADDR,
        &eth::IGem::getGemCall {
            gemId: issued.gemId,
        },
    )
    .expect("validator reward Gem data");
    assert_eq!(gem.owner, owner);
    let reward = ValidatorRewardGem {
        owner,
        gem_id: issued.gemId,
        gem,
        block_number,
    };
    world.state.validator_reward_gem = Some(reward.clone());
    reward
}

fn validator_reward_owner(world: &World) -> Address {
    let key = world.validators.get(0).evm_key().expect("validator 0 key");
    world
        .rpc
        .address_of(&key)
        .expect("validator 0 address")
        .parse()
        .expect("canonical validator 0 address")
}

pub(crate) fn chain_id_b256(world: &World) -> B256 {
    B256::from(U256::from(
        world
            .rpc
            .chain_id(world.validators.primary_port())
            .expect("settlement chain ID"),
    ))
}

/// The shared mining preimage: Nod and Gem bind the right, its owner and their own domain.
pub(crate) fn find_mining_pow_nonce(
    domain: outbe_common::pow::MiningDomain,
    id: U256,
    owner: Address,
) -> u64 {
    (0_u64..1_000_000)
        .find(|nonce| {
            outbe_common::pow::validate_mining_pow(
                domain,
                id,
                owner,
                outbe_common::pow::SINGLE_MINING_SEQUENCE,
                *nonce,
            )
            .is_ok()
        })
        .expect("bounded mining PoW nonce")
}

pub(crate) fn promis_balance(url: &str, owner: Address, view_key: &[u8; 32]) -> U256 {
    let blob = eth::read_call(
        url,
        addresses::PROMIS_ADDR,
        &eth::IPromis::balanceOfCall { account: owner },
    )
    .expect("Promis ciphertext");
    if blob.is_empty() {
        U256::ZERO
    } else {
        outbe_tee_enclave::promis::decrypt_balance(view_key, owner, blob.as_ref())
            .expect("decrypt Promis balance")
    }
}

pub(crate) fn gratis_balance(url: &str, owner: Address, view_key: &[u8; 32]) -> U256 {
    let blob = eth::read_call(
        url,
        addresses::GRATIS_ADDR,
        &eth::IGratis::balanceOfCall { account: owner },
    )
    .expect("Gratis ciphertext");
    if blob.is_empty() {
        U256::ZERO
    } else {
        outbe_tee_enclave::gratis::decrypt_balance(view_key, owner, blob.as_ref())
            .expect("decrypt Gratis balance")
    }
}

fn assert_success(url: &str, tx: &str, label: &str) {
    let _ = successful_receipt(url, tx, label);
}

pub(crate) fn assert_mined_success(outcome: &eth::MinedCallOutcome, label: &str) {
    assert!(outcome.success, "{label} reverted: {}", outcome.receipt);
}

fn successful_receipt(url: &str, tx: &str, label: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(receipt) = eth::receipt_json(url, tx) {
            assert_eq!(
                receipt.get("status").and_then(serde_json::Value::as_str),
                Some("0x1"),
                "{label} reverted: {receipt}"
            );
            return receipt;
        }
        assert!(Instant::now() < deadline, "{label} receipt timed out: {tx}");
        sleep(Duration::from_millis(250));
    }
}

pub(crate) struct SettlementFunding<'a> {
    pub(crate) asset: Address,
    pub(crate) owner_key: &'a str,
    pub(crate) owner: Address,
    pub(crate) spender: Address,
    pub(crate) amount: U256,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_sol_types::SolEvent as _;

    #[test]
    fn redemption_event_requires_exact_emitter_amount_and_one_occurrence() {
        let emitter = addresses::PROMIS_FACTORY_ADDR;
        let event = eth::IPromisFactory::CoenMined {
            sender: Address::repeat_byte(1),
            coenMinor: U256::from(123),
        };
        let encoded = event.encode_log_data();
        let log = serde_json::json!({
            "address": format!("{emitter:#x}"),
            "topics": encoded.topics().iter().map(|topic| format!("{topic:#x}")).collect::<Vec<_>>(),
            "data": format!("0x{}", hex::encode(&encoded.data)),
        });
        let valid = serde_json::json!({"logs": [log.clone()]});
        assert_receipt_event(&valid, emitter, &event);
        let mut wrong_amount = valid.clone();
        wrong_amount["logs"][0]["data"] = serde_json::json!(format!("{:#066x}", U256::from(124)));
        for invalid in [
            serde_json::json!({"logs": []}),
            serde_json::json!({"logs": [log.clone(), log]}),
            wrong_amount,
            serde_json::json!({}),
        ] {
            assert!(
                std::panic::catch_unwind(|| assert_receipt_event(&invalid, emitter, &event))
                    .is_err()
            );
        }
        assert!(std::panic::catch_unwind(|| {
            assert_receipt_event(&valid, Address::repeat_byte(2), &event)
        })
        .is_err());
    }
}
