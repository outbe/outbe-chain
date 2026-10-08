use super::*;

struct SponsoredGem {
    key: String,
    payer_key: String,
    payer: Address,
    owner: Address,
    gem_id: U256,
    gem: eth::IGem::GemData,
    fixture: SettlementFixture,
    url: String,
    payable: U256,
    snapshot_id: U256,
    keys: eth::ConfidentialAccountKeys,
}

pub(super) fn redeem(world: &mut World) {
    let scenario = prepare(world);
    assert_sponsorship(&scenario);
    let SponsoredGem {
        key,
        payer_key: _payer_key,
        payer,
        owner,
        gem_id,
        gem,
        fixture,
        url,
        payable,
        snapshot_id,
        keys,
    } = scenario;
    let settle = eth::send_sponsored_call(
        &url,
        &key,
        addresses::GEM_FACTORY_ADDR,
        500_000,
        &eth::IGemFactory::settleGemCall {
            gemId: gem_id,
            asset: fixture.asset,
            snapshotId: snapshot_id,
        },
    )
    .expect("sponsored settle reward Gem");
    assert_mined_success(&settle, "sponsored settle reward Gem");
    eprintln!(
        "settlement_evidence kind=sponsored_settle_gem tx={} gas_limit=500000 gas_used={}",
        settle.transaction_hash, settle.receipt["gasUsed"]
    );
    assert_eq!(eth::balance(&url, owner), Some(U256::ZERO));
    assert_eq!(
        eth::read_call(
            &url,
            fixture.asset,
            &ISettlementAsset::balanceOfCall {
                account: fixture.vault,
            },
        ),
        Some(payable),
        "reserve vault did not receive exact Gem cost"
    );

    let promis_before = promis_balance(&url, owner, &keys.view);
    let promis_nonce = eth::read_call(
        &url,
        addresses::PROMIS_ADDR,
        &eth::IPromis::opNonceOfCall { account: owner },
    )
    .expect("Promis nonce before Gem mining");
    let chain_id = chain_id_b256(world);
    let mint_mac = outbe_tee_enclave::promis::modify_mac(
        &keys.modify,
        owner,
        PromisOp::Mint,
        gem.promisLoadMinor,
        promis_nonce,
        chain_id,
    );
    let pow = find_mining_pow_nonce(outbe_common::pow::MiningDomain::Gem, gem_id, owner);
    let mine_promis = eth::send_sponsored_call(
        &url,
        &key,
        addresses::GEM_FACTORY_ADDR,
        500_000,
        &eth::IGemFactory::minePromisCall {
            gemId: gem_id,
            nonce: pow,
            mac: B256::from(mint_mac),
            opNonce: promis_nonce,
        },
    )
    .expect("sponsored mine Promis from settled Gem");
    assert_mined_success(&mine_promis, "sponsored mine Promis from settled Gem");
    assert_eq!(eth::balance(&url, owner), Some(U256::ZERO));
    assert_eq!(
        promis_balance(&url, owner, &keys.view),
        promis_before + gem.promisLoadMinor,
        "Gem load was not minted exactly into validator Promis"
    );

    let burn_nonce = eth::read_call(
        &url,
        addresses::PROMIS_ADDR,
        &eth::IPromis::opNonceOfCall { account: owner },
    )
    .expect("Promis nonce before COEN mining");
    let burn_mac = outbe_tee_enclave::promis::modify_mac(
        &keys.modify,
        owner,
        PromisOp::Burn,
        gem.promisLoadMinor,
        burn_nonce,
        chain_id,
    );
    let mine_coen = eth::send_sponsored_call(
        &url,
        &key,
        addresses::PROMIS_FACTORY_ADDR,
        500_000,
        &eth::IPromisFactory::mineCoenCall {
            promisMinor: gem.promisLoadMinor,
            mac: B256::from(burn_mac),
            opNonce: burn_nonce,
        },
    )
    .expect("sponsored mine COEN from validator Promis");
    assert!(
        mine_coen.success,
        "mine COEN from validator Promis reverted: {}",
        mine_coen.receipt
    );
    let native_after = eth::balance(&url, owner).expect("native balance after Promis burn");
    assert_eq!(promis_balance(&url, owner, &keys.view), promis_before);
    assert_eq!(
        native_after,
        checked_protocol_to_native(gem.promisLoadMinor).expect("Gem load fits native COEN"),
        "three sponsored calls must charge no native fee to the validator"
    );
    let counter_after = eth::read_call(
        &url,
        addresses::ZEROFEE_ADDR,
        &eth::IZeroFee::getCounterCall { signer: owner },
    )
    .expect("ZeroFee counter after Gem redemption");
    assert_eq!(
        counter_after.count, 3,
        "settleGem, minePromis, and mineCoen must consume three of eight sponsored slots"
    );
    eprintln!(
        "settlement_evidence kind=zerofee_gem_to_coen owner={owner:#x} payer={payer:#x} gem_id={gem_id} asset={:#x} vault={:#x} amount={} settle_tx={} promis_tx={} coen_tx={} quota_used={} native_before=0 native_after={}",
        fixture.asset, fixture.vault, gem.promisLoadMinor, settle.transaction_hash, mine_promis.transaction_hash, mine_coen.transaction_hash, counter_after.count, native_after
    );
}

fn prepare(world: &mut World) -> SponsoredGem {
    let validator = world.validators.get(0);
    let key = validator.evm_key().expect("validator 0 EVM key");
    let payer_key = world
        .validators
        .get(1)
        .evm_key()
        .expect("validator 1 sponsorship payer key");
    let payer = eth::address_of(&payer_key).expect("validator 1 payer address");
    let (owner, gem_id, mut gem) = wait_for_validator_reward_gem(world);
    let fixture = deploy_settlement_fixture(world);
    let url = world.rpc.url(world.validators.primary_port());
    // A reward Gem qualifies on a closed day that it held in full. This one was delivered
    // minutes ago. Stamp it behind the day that is then seeded above its floor.
    if !crate::features::gem_lifecycle::gem_is_qualified(&url, gem_id) {
        let now = world
            .rpc
            .latest_block_timestamp(world.validators.primary_port())
            .expect("committee head timestamp");
        eth::send_call(
            &url,
            addresses::GEM_ADDR,
            DEPLOYER_KEY,
            &crate::features::gem_lifecycle::IGemTestArming::backdateGemForTestCall {
                gemId: gem_id,
                issuedAt: now.saturating_sub(3 * 86_400),
            },
            None,
        )
        .expect("backdate the reward Gem's issuance stamp");
        test_issuance::seed_day_vwaps(
            &url,
            DEPLOYER_KEY,
            USD_ISO,
            1,
            // A Genesis floor is zero, and a zero VWAP is no price at all.
            gem.floorPriceMinor
                .checked_mul(U256::from(2u64))
                .expect("qualifying day VWAP")
                .max(gem.entryPriceMinor),
        )
        .expect("seed the closed day's VWAP above the reward Gem floor");
        // Qualification is derived on read once the seeded day is closed.
        let deadline = Instant::now() + Duration::from_secs(240);
        while !crate::features::gem_lifecycle::gem_is_qualified(&url, gem_id) {
            assert!(
                Instant::now() < deadline,
                "reward Gem qualification timed out"
            );
            sleep(Duration::from_millis(250));
        }
    }
    gem = eth::read_call(
        &url,
        addresses::GEM_ADDR,
        &eth::IGem::getGemStatusCall { gemId: gem_id },
    )
    .expect("read the same reward Gem after qualification");
    assert_eq!(
        gem.state, 1,
        "reward Gem must read Qualified for settlement"
    );
    assert!(
        crate::features::gem_lifecycle::gem_is_qualified(&url, gem_id),
        "reward Gem must be qualified for settlement"
    );
    // The cost is derived, so the amount to fund is the factory's own quote. That quote
    // is already in the settlement asset's units. The reference amount never was.
    let quote = eth::read_call(
        &url,
        addresses::GEM_FACTORY_ADDR,
        &eth::IGemFactory::quoteSettlementCall {
            gemId: gem_id,
            asset: fixture.asset,
        },
    )
    .expect("quote settling the reward Gem");
    let payable = quote.paymentMinor;
    // The token is not a sponsored target. So the approval pays its own gas before the drain.
    fund_and_approve(
        world,
        fixture.asset,
        &key,
        owner,
        addresses::GEM_FACTORY_ADDR,
        payable,
    );
    let keys =
        eth::derive_account_keys(&url, &key, Ledger::Promis).expect("derive validator Promis keys");

    SponsoredGem {
        key,
        payer_key,
        payer,
        owner,
        gem_id,
        gem,
        fixture,
        url,
        payable,
        snapshot_id: quote.snapshotId,
        keys,
    }
}

fn assert_sponsorship(scenario: &SponsoredGem) {
    let url = &scenario.url;
    let key = &scenario.key;
    let payer_key = &scenario.payer_key;
    let owner = scenario.owner;
    let payer = scenario.payer;
    let drain = eth::drain_native_balance(url, key, payer)
        .expect("drain validator spendable COEN before ZeroFee proof");
    assert_mined_success(&drain, "drain validator spendable COEN");
    assert_eq!(
        eth::balance(url, owner),
        Some(U256::ZERO),
        "validator must enter the sponsored redemption path with exactly zero COEN"
    );

    let delegation =
        eth::install_delegation_for_authority(url, payer_key, key, addresses::ZEROFEE_ADDR)
            .expect("install sponsor-paid ZeroFee delegation for zero-balance validator");
    assert_eq!(
        delegation.get("status").and_then(serde_json::Value::as_str),
        Some("0x1"),
        "sponsor-paid delegation reverted: {delegation}"
    );
    assert_eq!(
        eth::balance(url, owner),
        Some(U256::ZERO),
        "delegation payer, not validator, must pay installation gas"
    );
    assert_eq!(
        eth::read_call(
            url,
            addresses::ZEROFEE_ADDR,
            &eth::IZeroFee::authorizeSponsorshipCall { signer: owner },
        ),
        Some(true),
        "ZeroFee must authorize an under-quota address at zero native balance"
    );
    let counter_before = eth::read_call(
        url,
        addresses::ZEROFEE_ADDR,
        &eth::IZeroFee::getCounterCall { signer: owner },
    )
    .expect("ZeroFee counter before Gem redemption");
    assert_eq!(counter_before.count, 0);
}
