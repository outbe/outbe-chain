use super::*;

struct PaidGem {
    ports: Vec<u16>,
    url: String,
    key: String,
    owner: Address,
    gem_id: U256,
    gem: eth::IGem::GemData,
    vault: Address,
    asset: Address,
    snapshot_id: U256,
    payable: U256,
    reserve_before: U256,
    keys: eth::ConfidentialAccountKeys,
}

pub(super) fn redeem(world: &mut World) {
    let scenario = prepare(world);
    let PaidGem {
        url,
        key,
        owner,
        gem_id,
        gem,
        asset,
        snapshot_id,
        keys,
        ..
    } = &scenario;
    let (owner, gem_id, asset, snapshot_id) = (*owner, *gem_id, *asset, *snapshot_id);
    let (settle, settled) = scenario.finalize(
        world,
        eth::send_call_with_gas_reserve(
            url,
            addresses::GEM_FACTORY_ADDR,
            key,
            &eth::IGemFactory::settleGemCall {
                gemId: gem_id,
                asset,
                snapshotId: snapshot_id,
            },
            None,
        )
        .expect("paid settle reward Gem"),
        "settle",
    );
    scenario.assert_settled(world, settled.height);
    let promis_before = promis_balance_at(url, owner, &keys.view, settled.height);
    let nonce = eth::read_call(
        url,
        addresses::PROMIS_ADDR,
        &eth::IPromis::opNonceOfCall { account: owner },
    )
    .expect("Promis mint nonce");
    let chain_id = chain_id_b256(world);
    let mac = outbe_tee_enclave::promis::modify_mac(
        &keys.modify,
        owner,
        PromisOp::Mint,
        gem.promisLoadMinor,
        nonce,
        chain_id,
    );
    let (mint, minted) = scenario.finalize(
        world,
        eth::send_call_with_gas_reserve(
            url,
            addresses::GEM_FACTORY_ADDR,
            key,
            &eth::IGemFactory::minePromisCall {
                gemId: gem_id,
                nonce: find_mining_pow_nonce(outbe_common::pow::MiningDomain::Gem, gem_id, owner),
                mac: B256::from(mac),
                opNonce: nonce,
            },
            None,
        )
        .expect("paid mine Promis from reward Gem"),
        "mint_promis",
    );
    scenario.assert_minted(world, &mint, minted.height, promis_before);
    let nonce = eth::read_call(
        url,
        addresses::PROMIS_ADDR,
        &eth::IPromis::opNonceOfCall { account: owner },
    )
    .expect("Promis burn nonce");
    let mac = outbe_tee_enclave::promis::modify_mac(
        &keys.modify,
        owner,
        PromisOp::Burn,
        gem.promisLoadMinor,
        nonce,
        chain_id,
    );
    let (burn, burned) = scenario.finalize(
        world,
        eth::send_call_with_gas_reserve(
            url,
            addresses::PROMIS_FACTORY_ADDR,
            key,
            &eth::IPromisFactory::mineCoenCall {
                promisMinor: gem.promisLoadMinor,
                mac: B256::from(mac),
                opNonce: nonce,
            },
            None,
        )
        .expect("paid mine COEN from validator Promis"),
        "mint_coen",
    );
    let (native_mint, fee) = scenario.assert_burned(world, &burn, burned.height, promis_before);
    eprintln!("settlement_evidence kind=paid_gem_to_promis_to_coen owner={owner:#x} gem_id={gem_id} promis={} coen={native_mint} settle_tx={} promis_tx={} coen_tx={} gas={fee}",
        gem.promisLoadMinor, settle.transaction_hash, mint.transaction_hash, burn.transaction_hash);
}

fn prepare(world: &mut World) -> PaidGem {
    let port = world.validators.primary_port();
    let ports = world.validators.committee_ports();
    let url = world.rpc.url(port);
    let key = world.validators.get(0).evm_key().expect("validator 0 key");
    let (owner, gem_id, gem) = wait_for_validator_reward_gem(world);
    assert_eq!(gem.owner, owner);
    assert!(
        matches!(gem.gemType, 0 | 1),
        "expected a protocol reward Gem"
    );
    assert!(
        !gem.promisLoadMinor.is_zero(),
        "reward Gem must carry Promis"
    );
    let delivery = find_canonical_reward_gem_delivery_block_number(world, gem_id)
        .expect("reward Gem must originate from canonical RewardsGemDelivery");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, delivery, 120)
        .expect("all validators finalize the reward Gem delivery");
    let qualified = || crate::features::gem_lifecycle::gem_is_qualified(&url, gem_id);
    if !qualified() {
        // A reward Gem qualifies on a closed day that it held in full. This one was
        // delivered minutes ago. Stamp it behind the day that is then seeded above its floor.
        let now = world
            .rpc
            .latest_block_timestamp(port)
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
            gem.floorPriceMinor
                .checked_mul(U256::from(2u64))
                .expect("qualifying day VWAP")
                .max(gem.entryPriceMinor),
        )
        .expect("seed the closed day's VWAP above the reward Gem floor");
        let deadline = Instant::now() + Duration::from_secs(240);
        while !qualified() {
            assert!(
                Instant::now() < deadline,
                "reward Gem qualification timed out"
            );
            sleep(Duration::from_millis(250));
        }
    }

    // The preceding Nod redemption already registered a real settlement vault.
    // Reuse it and assert a balance delta, not an empty-vault fixture balance.
    let vault = eth::read_call(
        &url,
        addresses::VAULT_ROUTER_ADDR,
        &eth::IVaultRouter::referenceCurrencyVaultAtCall {
            isoCode: USD_ISO,
            index: U256::ZERO,
        },
    )
    .expect("existing USD settlement vault");
    assert_ne!(vault, Address::ZERO);
    let asset = eth::read_call(&url, vault, &ISettlementVault::assetCall {})
        .expect("existing settlement vault asset");
    let quote = eth::read_call(
        &url,
        addresses::GEM_FACTORY_ADDR,
        &eth::IGemFactory::quoteSettlementCall {
            gemId: gem_id,
            asset,
        },
    )
    .expect("quote reward Gem settlement");
    let payable = quote.paymentMinor;
    assert!(!payable.is_zero());
    let reserve_before = eth::read_call(
        &url,
        asset,
        &ISettlementAsset::balanceOfCall { account: vault },
    )
    .expect("reserve before settlement");
    fund_and_approve(
        world,
        crate::features::settlement::SettlementFunding {
            asset,
            owner_key: &key,
            owner,
            spender: addresses::GEM_FACTORY_ADDR,
            amount: payable,
        },
    );

    let keys = eth::derive_account_keys(&url, &key, Ledger::Promis)
        .expect("derive validator Promis keys through TEE");
    let snapshot_id = quote.snapshotId;
    PaidGem {
        ports,
        url,
        key,
        owner,
        gem_id,
        gem,
        vault,
        asset,
        snapshot_id,
        payable,
        reserve_before,
        keys,
    }
}

impl PaidGem {
    fn assert_settled(&self, world: &World, height: u64) {
        let ports = &self.ports;
        let (owner, gem_id, asset, vault, reserve_before, payable) = (
            self.owner,
            self.gem_id,
            self.asset,
            self.vault,
            self.reserve_before,
            self.payable,
        );
        let gem = &self.gem;
        for &p in ports {
            let observed = eth::read_call_at_result(
                &world.rpc.url(p),
                addresses::GEM_ADDR,
                &eth::IGem::getGemStatusCall { gemId: gem_id },
                height,
            )
            .expect("finalized settled reward Gem");
            assert_eq!(observed.state, 3, "reward Gem must be Settled");
            assert_eq!(observed.owner, owner);
            assert_eq!(observed.promisLoadMinor, gem.promisLoadMinor);
            assert_eq!(
                eth::read_call_at_result(
                    &world.rpc.url(p),
                    asset,
                    &ISettlementAsset::balanceOfCall { account: vault },
                    height,
                )
                .expect("finalized reserve after settlement"),
                reserve_before + payable,
                "settlement must credit the exact additional Gem cost"
            );
        }
    }

    fn assert_burned(
        &self,
        world: &World,
        burn: &crate::world::rpc::TxOutcome,
        height: u64,
        promis_before: U256,
    ) -> (U256, U256) {
        let ports = &self.ports;
        let owner = self.owner;
        let gem = &self.gem;
        let keys = &self.keys;
        let native_mint =
            checked_protocol_to_native(gem.promisLoadMinor).expect("native COEN amount");
        assert_receipt_event(
            &burn.receipt,
            addresses::PROMIS_FACTORY_ADDR,
            &eth::IPromisFactory::CoenMined {
                sender: owner,
                coenMinor: native_mint,
            },
        );
        let fee =
            crate::world::rpc::Rpc::receipt_gas_cost(&burn.receipt).expect("paid COEN mint fee");
        for &p in ports {
            let peer_url = world.rpc.url(p);
            assert_eq!(
                promis_balance_at(&peer_url, owner, &keys.view, height),
                promis_before,
                "exact finalized Promis burn"
            );
            let before = native_balance_at(&peer_url, owner, height - 1);
            let after = native_balance_at(&peer_url, owner, height);
            assert_eq!(
                after + fee,
                before + native_mint,
                "exact finalized COEN credit plus paid gas"
            );
        }
        (native_mint, fee)
    }

    fn finalize(
        &self,
        world: &World,
        tx: String,
        label: &str,
    ) -> (
        crate::world::rpc::TxOutcome,
        crate::world::rpc::FinalizedCheckpoint,
    ) {
        let url = &self.url;
        let ports = &self.ports;
        let gem_id = self.gem_id;

        let receipt = successful_receipt(url, &tx, label);
        let outcome = crate::world::rpc::TxOutcome {
            transaction_hash: tx,
            success: true,
            receipt,
        };
        let checkpoint = world
            .rpc
            .finalize_outcome(&outcome, ports, 120)
            .expect("paid redemption receipt must finalize identically on all validators");
        eprintln!("settlement_evidence kind=paid_reward_gem stage={label} gem_id={gem_id} tx={} finalized_height={} block_hash={}",
            outcome.transaction_hash, checkpoint.height, checkpoint.block_hash);
        (outcome, checkpoint)
    }

    fn assert_minted(
        &self,
        world: &World,
        mint: &crate::world::rpc::TxOutcome,
        height: u64,
        promis_before: U256,
    ) {
        let ports = &self.ports;
        let owner = self.owner;
        let gem_id = self.gem_id;
        let gem = &self.gem;
        let keys = &self.keys;
        assert_receipt_event(
            &mint.receipt,
            addresses::GEM_FACTORY_ADDR,
            &eth::IGemFactory::GemExercised {
                gemId: gem_id,
                owner,
                promisLoadMinor: gem.promisLoadMinor,
            },
        );
        for &p in ports {
            let peer_url = world.rpc.url(p);
            assert_eq!(
                promis_balance_at(&peer_url, owner, &keys.view, height),
                promis_before + gem.promisLoadMinor,
                "exact finalized Promis mint"
            );
            let count = eth::read_call_at_result(
                &peer_url,
                addresses::GEM_ADDR,
                &eth::IGem::balanceOfCall { owner },
                height,
            )
            .expect("Gem enumeration count");
            for index in 0..u64::try_from(count).expect("Gem enumeration fits u64") {
                let remaining = eth::read_call_at_result(
                    &peer_url,
                    addresses::GEM_ADDR,
                    &eth::IGem::tokenOfOwnerByIndexCall {
                        owner,
                        index: U256::from(index),
                    },
                    height,
                )
                .expect("read remaining Gem, not RPC failure as absence");
                assert_ne!(
                    remaining, gem_id,
                    "mined Gem must leave the owner's inventory"
                );
            }
        }
    }
}
