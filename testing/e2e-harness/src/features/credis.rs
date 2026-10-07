//! Live reserve, direct Gratis pledge, issuance, and interest-bearing repayments.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{keccak256, Address, B256, U256};
use cucumber::{given, then, when};
use outbe_primitives::addresses::{
    CCA_REGISTRY_ADDRESS, CREDIS_ADDRESS, CREDIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS,
};
use outbe_tee::protocol::{GratisOp, Ledger, PromisOp};

use crate::features::settlement::{assert_receipt_event, chain_id_b256, find_mining_pow_nonce};
use crate::internal::{addresses, eth};
use crate::world::credis::{
    self, event, execute, send, snapshot, CredisFixture, ICcaRegistry, ICredis, ICredisFactory,
    IFixtureToken, IFixtureVault, DAY, INITIAL_GRATIS, INITIAL_STABLES, LIQUIDITY, PRINCIPAL, USD,
};
use crate::world::forge::DEPLOYER_KEY;
use crate::world::localnet::{BootstrapProfile, StartOpts};
use crate::world::{settlement_currency, test_issuance, World};

#[given("a prepared Credis localnet with funded actors and vault liquidity")]
fn prepare(world: &mut World) {
    let (user, cca_key, cca) = credis::actors();
    let profile = BootstrapProfile::default()
        .with_credis_accounts(user, cca)
        .expect("Credis seed profile");
    world.state.voting_window = 6;
    world.state.wwd = Some(crate::world::localnet::worldwide_day());
    world
        .localnet
        .bootstrap_with_profile(world.validators.size(), &profile)
        .expect("bootstrap Credis localnet");
    crate::features::common::start_bootstrapped_localnet(world, &StartOpts::with_voting_window(6));
    let url = world.rpc.url(world.validators.primary_port());
    let owner_key = world
        .validators
        .get(0)
        .evm_key()
        .expect("vault router owner");
    let currency = settlement_currency::deploy(
        &crate::env::environment().repo.join("contracts/intex"),
        &url,
        &owner_key,
    )
    .expect("deploy and register USD vault");
    let account = credis::deploy_account(&url, user, cca);
    send(
        &url,
        currency.asset,
        DEPLOYER_KEY,
        &IFixtureToken::mintCall {
            to: account,
            amount: INITIAL_STABLES,
        },
        None,
    );
    send(
        &url,
        currency.asset,
        DEPLOYER_KEY,
        &IFixtureToken::mintCall {
            to: user,
            amount: LIQUIDITY,
        },
        None,
    );
    send(
        &url,
        currency.asset,
        DEPLOYER_KEY,
        &IFixtureToken::approveCall {
            spender: currency.vault,
            amount: LIQUIDITY,
        },
        None,
    );
    send(
        &url,
        currency.vault,
        DEPLOYER_KEY,
        &IFixtureVault::depositCall {
            assets: LIQUIDITY,
            onBehalf: VAULT_ROUTER_ADDRESS,
        },
        None,
    );
    send(
        &url,
        CCA_REGISTRY_ADDRESS,
        &cca_key,
        &ICcaRegistry::bondCall {
            name: "Credis E2E CCA".into(),
        },
        Some(eth::coen(1_000_000_000)),
    );
    assert_eq!(
        eth::read_call(
            &url,
            CCA_REGISTRY_ADDRESS,
            &ICcaRegistry::getCcaStateCall { cca }
        ),
        Some(ICcaRegistry::State::Active)
    );

    // Prove the CCA can operate the same account the user will repay through.
    execute(
        &url,
        account,
        &cca_key,
        currency.asset,
        &IFixtureToken::approveCall {
            spender: CREDIS_FACTORY_ADDRESS,
            amount: U256::ZERO,
        },
    );
    let promis_keys =
        eth::derive_account_keys(&url, DEPLOYER_KEY, Ledger::Promis).expect("user Promis keys");
    let keys =
        eth::derive_account_keys(&url, DEPLOYER_KEY, Ledger::Gratis).expect("user Gratis keys");
    let chain = chain_id_b256(world);
    let gem = eth::read_call(
        &url,
        addresses::GEM_ADDR,
        &eth::IGem::tokenOfOwnerByIndexCall {
            owner: user,
            index: U256::ZERO,
        },
    )
    .expect("seeded settled Gem");
    let promis_nonce = eth::read_call(
        &url,
        addresses::PROMIS_ADDR,
        &eth::IPromis::opNonceOfCall { account: user },
    )
    .expect("Promis mint nonce");
    send(
        &url,
        addresses::GEM_FACTORY_ADDR,
        DEPLOYER_KEY,
        &eth::IGemFactory::minePromisCall {
            gemId: gem,
            nonce: find_mining_pow_nonce(outbe_common::pow::MiningDomain::Gem, gem, user),
            mac: outbe_tee_enclave::promis::modify_mac(
                &promis_keys.modify,
                user,
                PromisOp::Mint,
                INITIAL_GRATIS,
                promis_nonce,
                chain,
            )
            .into(),
            opNonce: promis_nonce,
        },
        None,
    );
    let promis_nonce = eth::read_call(
        &url,
        addresses::PROMIS_ADDR,
        &eth::IPromis::opNonceOfCall { account: user },
    )
    .expect("Promis burn nonce");
    let gratis_nonce = eth::read_call(
        &url,
        addresses::GRATIS_ADDR,
        &eth::IGratis::opNonceOfCall { account: user },
    )
    .expect("Gratis mint nonce");
    send(
        &url,
        addresses::PROMIS_FACTORY_ADDR,
        DEPLOYER_KEY,
        &eth::IPromisFactory::mineGratisCall {
            promisMinor: INITIAL_GRATIS,
            promisMac: outbe_tee_enclave::promis::modify_mac(
                &promis_keys.modify,
                user,
                PromisOp::Burn,
                INITIAL_GRATIS,
                promis_nonce,
                chain,
            )
            .into(),
            promisOpNonce: promis_nonce,
            gratisMac: outbe_tee_enclave::gratis::modify_mac(
                &keys.modify,
                user,
                GratisOp::Mint,
                INITIAL_GRATIS,
                gratis_nonce,
                chain,
            )
            .into(),
            gratisOpNonce: gratis_nonce,
        },
        None,
    );
    world.state.credis = Some(CredisFixture {
        user,
        cca,
        cca_key,
        account,
        currency,
        keys,
        reservation: U256::ZERO,
        gratis_minor: U256::ZERO,
        position_id: U256::ZERO,
        initial_native: U256::ZERO,
        interest_paid: U256::ZERO,
    });
    let state = snapshot(world);
    assert_eq!(state.liquid, INITIAL_GRATIS);
    assert_eq!(state.pledged, U256::ZERO);
    assert_eq!(state.account_stables, INITIAL_STABLES);
    assert_eq!(state.cca_stables, U256::ZERO);
    assert_eq!(state.vault_stables, LIQUIDITY);
    assert_eq!(state.shares, LIQUIDITY);
    assert_eq!(state.router_stables, U256::ZERO);
    world.state.credis.as_mut().expect("fixture").initial_native = state.native;
}

#[then("the pledge valuation window closes over that quote")]
fn pledge_window_closes(world: &mut World) {
    crate::features::entity_lifecycle::markets::close_price_window(world, &[USD]);
}

#[when("the CCA reserves 300 stablecoins for the user's smart account")]
fn reserve(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    // Reuse Intex's E2E-only history fixture; live current prices come from the feeder.
    test_issuance::seed_day_vwaps(&url, DEPLOYER_KEY, USD, 1, U256::from(1_000_000))
        .expect("previous closed USD VWAP");
    let f = world.state.credis.as_ref().expect("fixture");
    assert_eq!(
        eth::read_call(
            &url,
            VAULT_ROUTER_ADDRESS,
            &eth::IVaultRouter::hasLiquidityCall {
                asset: f.currency.asset,
                amount: PRINCIPAL
            }
        ),
        Some(true)
    );
    let receipt = send(
        &url,
        VAULT_ROUTER_ADDRESS,
        &f.cca_key,
        &eth::IVaultRouter::reserveStablesCall {
            referenceCurrency: USD,
            smartAccount: f.account,
            source: f.user,
            asset: f.currency.asset,
            amount: PRINCIPAL,
        },
        None,
    );
    let reserved = event::<eth::IVaultRouter::ReservationCreated>(&receipt, VAULT_ROUTER_ADDRESS);
    assert_eq!(reserved.smartAccount, f.account);
    assert_eq!(reserved.cca, f.cca);
    assert_eq!(reserved.source, f.user);
    assert_eq!(reserved.asset, f.currency.asset);
    assert_eq!(reserved.vault, f.currency.vault);
    assert_eq!(reserved.amount, PRINCIPAL);
    assert!(reserved.expiresAt > receipt_timestamp(&url, &receipt));
    world.state.credis.as_mut().expect("fixture").reservation = reserved.id;
}

#[then("the reservation holds the requested liquidity for those actors")]
fn reserved(world: &mut World) {
    let state = snapshot(world);
    let f = world.state.credis.as_ref().expect("fixture");
    let (asset, amount, account, cca, vault, expiry) = state.reservation;
    assert_eq!(
        (asset, amount, account, cca, vault),
        (
            f.currency.asset,
            PRINCIPAL,
            f.account,
            f.cca,
            f.currency.vault
        )
    );
    assert!(
        expiry
            > world
                .rpc
                .latest_block_timestamp(world.validators.primary_port())
                .expect("head time")
    );
    assert_eq!(state.vault_stables, LIQUIDITY - PRINCIPAL);
    assert_eq!(state.shares, LIQUIDITY - PRINCIPAL);
    assert_eq!(state.router_stables, PRINCIPAL);
    assert_eq!(state.account_stables, INITIAL_STABLES);
    assert_eq!(state.cca_stables, U256::ZERO);
}

#[when("the user pledges Gratis")]
fn pledge(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let f = world.state.credis.as_ref().expect("fixture");
    let reservation = eth::read_call(
        &url,
        VAULT_ROUTER_ADDRESS,
        &eth::IVaultRouter::reservationOfCall { id: f.reservation },
    )
    .expect("reservation");
    let nonce = eth::read_call(
        &url,
        addresses::GRATIS_ADDR,
        &eth::IGratis::opNonceOfCall { account: f.user },
    )
    .expect("pledge nonce");
    let mac = outbe_tee_enclave::gratis::modify_mac(
        &f.keys.modify,
        f.user,
        GratisOp::Pledge,
        reservation.gratisMinor,
        nonce,
        chain_id_b256(world),
    );
    let receipt = send(
        &url,
        addresses::GRATIS_FACTORY_ADDR,
        DEPLOYER_KEY,
        &eth::IGratisFactory::createPledgeNoteCall {
            reservationId: f.reservation,
            auth: eth::IGratisFactory::ModifyAuth {
                mac: mac.into(),
                opNonce: nonce,
            },
        },
        None,
    );
    let pledged =
        event::<eth::IGratisFactory::PledgeNoteCreated>(&receipt, addresses::GRATIS_FACTORY_ADDR);
    assert_eq!(
        (pledged.reservationId, pledged.source, pledged.gratisMinor),
        (f.reservation, f.user, reservation.gratisMinor)
    );
    world.state.credis.as_mut().expect("fixture").gratis_minor = pledged.gratisMinor;
    let state = snapshot(world);
    assert_eq!(state.liquid, INITIAL_GRATIS - pledged.gratisMinor);
    assert_eq!(state.pledged, pledged.gratisMinor);
}

#[when("the CCA issues Credis against the pledge and reservation")]
fn issue(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let f = world.state.credis.as_ref().expect("fixture");
    let stake = outbe_primitives::units::checked_protocol_to_native(f.gratis_minor)
        .expect("fixture stake fits native units");
    let receipt = send(
        &url,
        CREDIS_FACTORY_ADDRESS,
        &f.cca_key,
        &ICredisFactory::issueCredisCall {
            reservationId: f.reservation,
        },
        Some(stake),
    );
    let block = receipt["blockNumber"]
        .as_str()
        .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
        .expect("issuance block number");
    let mut preimage = Vec::with_capacity(68);
    preimage.extend_from_slice(f.cca.as_slice());
    preimage.extend_from_slice(f.account.as_slice());
    preimage.extend_from_slice(f.currency.asset.as_slice());
    preimage.extend_from_slice(&block.to_be_bytes());
    let id = U256::from_be_bytes(keccak256(preimage).0);
    assert_receipt_event(
        &receipt,
        CREDIS_ADDRESS,
        &ICredis::PositionCreated {
            positionId: id,
            smartAccount: f.account,
            cca: f.cca,
            principalMinor: PRINCIPAL,
            gratisMinor: f.gratis_minor,
        },
    );
    assert_receipt_event(
        &receipt,
        CREDIS_FACTORY_ADDRESS,
        &ICredisFactory::CredisIssued {
            smartAccount: f.account,
            cca: f.cca,
            principalMinor: PRINCIPAL,
        },
    );
    assert_receipt_event(
        &receipt,
        addresses::GRATIS_FACTORY_ADDR,
        &eth::IGratisFactory::PledgeNoteSentToCredis {
            reservationId: f.reservation,
            positionId: id,
        },
    );
    world.state.credis.as_mut().expect("fixture").position_id = id;
}

#[then("the smart account owns the open Credis position")]
fn issued(world: &mut World) {
    let state = snapshot(world);
    let f = world.state.credis.as_ref().expect("fixture");
    let p = state.position.expect("issued position");
    assert_eq!(p.positionId, f.position_id);
    assert_eq!(
        (p.smartAccount, p.cca, p.asset),
        (f.account, f.cca, f.currency.asset)
    );
    assert_eq!((p.issuanceCurrency, p.referenceCurrency), (USD, USD));
    assert_eq!(
        (p.principalMinor, p.outstandingPrincipalMinor),
        (PRINCIPAL, PRINCIPAL)
    );
    assert_eq!(
        (p.gratisMinor, p.outstandingGratisMinor),
        (f.gratis_minor, f.gratis_minor)
    );
    assert_eq!(p.state, 0);
    assert_eq!(p.lastSettledAt, p.issuedAt);
    assert_eq!(p.entryPriceMinor, U256::from(1_000_000));
    assert_eq!(p.callAnchorPriceMinor, U256::from(1_000_000));
    assert_eq!(p.callPriceMinor, U256::from(1_640_000));
    assert!(p.policyRate > U256::ZERO);
    // Credis currently pins the issuance currency's official rate with a 1x multiplier.
    assert_eq!(p.policyRate, state.policy_rate);
    assert_eq!(p.source, f.user);
    assert_eq!(
        state.reservation,
        (
            Address::ZERO,
            U256::ZERO,
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
            0
        )
    );
    assert_eq!(state.account_stables, INITIAL_STABLES);
    assert_eq!(state.cca_stables, PRINCIPAL);
    assert_eq!(
        state.native,
        f.initial_native
            + outbe_primitives::units::checked_protocol_to_native(f.gratis_minor).expect("stake")
    );
    assert_eq!(state.vault_stables, LIQUIDITY - PRINCIPAL);
    assert_eq!(state.shares, LIQUIDITY - PRINCIPAL);
    assert_eq!(state.router_stables, U256::ZERO);
    assert_eq!(state.liquid, INITIAL_GRATIS - f.gratis_minor);
    assert_eq!(state.pledged, f.gratis_minor);
}

#[when("the user makes three daily payments through the smart account")]
fn repay(world: &mut World) {
    for payment_index in 0..3 {
        let before = snapshot(world);
        let p = before.position.as_ref().expect("position before payment");
        // An hour of slack keeps transaction inclusion well away from the next day boundary.
        let target = p.lastSettledAt + DAY + 3_600;
        let (_, _, _, pending) =
            crate::features::ocomp::restart_committee_at_logical_time(world, target);
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            let time_ready = world
                .rpc
                .latest_block_timestamp(world.validators.primary_port())
                .is_some_and(|time| time >= target);
            let price_ready = pending.as_ref().is_none_or(|pending| {
                crate::features::price_oracle::observe_pending_publication(world, pending)
            });
            if time_ready && price_ready {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Credis clock/price restart did not become ready"
            );
            sleep(Duration::from_secs(1));
        }
        let url = world.rpc.url(world.validators.primary_port());
        let f = world.state.credis.as_ref().expect("fixture");
        let now = world
            .rpc
            .latest_block_timestamp(world.validators.primary_port())
            .expect("payment time");
        let days = (now - p.lastSettledAt) / DAY;
        assert_eq!(days, 1, "each payment must accrue one new whole day");
        let interest = expected_interest(p, now);
        assert!(interest > U256::ZERO);
        assert_eq!(
            eth::read_call(
                &url,
                CREDIS_ADDRESS,
                &ICredis::interestAccruedMinorCall {
                    positionId: f.position_id
                }
            ),
            Some(interest)
        );
        let principal = if payment_index == 2 {
            p.outstandingPrincipalMinor
        } else {
            U256::from(100_000_000)
        };
        let amount = principal + interest;
        execute(
            &url,
            f.account,
            DEPLOYER_KEY,
            f.currency.asset,
            &IFixtureToken::approveCall {
                spender: CREDIS_FACTORY_ADDRESS,
                amount,
            },
        );
        let receipt = execute(
            &url,
            f.account,
            DEPLOYER_KEY,
            CREDIS_FACTORY_ADDRESS,
            &ICredisFactory::settleCredisCall {
                positionId: f.position_id,
                amountMinor: amount,
            },
        );
        let paid_at = receipt_timestamp(&url, &receipt);
        assert_eq!(
            expected_interest(p, paid_at),
            interest,
            "interest changed before inclusion"
        );
        let released = if principal == p.outstandingPrincipalMinor {
            p.outstandingGratisMinor
        } else {
            (p.gratisMinor * principal)
                .div_ceil(p.principalMinor)
                .min(p.outstandingGratisMinor)
        };
        assert_receipt_event(
            &receipt,
            CREDIS_ADDRESS,
            &ICredis::SettlementApplied {
                positionId: f.position_id,
                interestMinor: interest,
                principalPaidMinor: principal,
                gratisReturnedMinor: released,
                outstandingPrincipalMinor: p.outstandingPrincipalMinor - principal,
            },
        );
        if payment_index == 2 {
            assert_receipt_event(
                &receipt,
                CREDIS_ADDRESS,
                &ICredis::PositionSettled {
                    positionId: f.position_id,
                },
            );
        }
        let after = snapshot(world);
        let a = after.position.as_ref().expect("position after payment");
        assert_eq!(
            a.outstandingPrincipalMinor,
            p.outstandingPrincipalMinor - principal
        );
        assert_eq!(
            a.outstandingGratisMinor,
            p.outstandingGratisMinor - released
        );
        assert_eq!(a.lastSettledAt, p.lastSettledAt + DAY);
        assert_eq!(a.state, if payment_index == 2 { 2 } else { 0 });
        assert_eq!(
            (
                a.principalMinor,
                a.policyRate,
                a.entryPriceMinor,
                a.callAnchorPriceMinor
            ),
            (
                p.principalMinor,
                p.policyRate,
                p.entryPriceMinor,
                p.callAnchorPriceMinor
            )
        );
        assert_eq!(after.account_stables, before.account_stables - amount);
        assert_eq!(after.vault_stables, before.vault_stables + amount);
        assert_eq!(after.shares, before.shares + amount);
        assert_eq!(after.router_stables, U256::ZERO);
        assert_eq!(after.cca_stables, before.cca_stables);
        assert_eq!(after.native, before.native);
        assert_eq!(after.liquid, before.liquid + released);
        assert_eq!(after.pledged, before.pledged - released);
        assert_eq!(a.interestPaidMinor, f.interest_paid + interest);
        world.state.credis.as_mut().expect("fixture").interest_paid += interest;
    }
}

#[then("the principal and interest are paid and all collateral is released")]
fn fully_repaid(world: &mut World) {
    let state = snapshot(world);
    let f = world.state.credis.as_ref().expect("fixture");
    let position = state.position.expect("retained settled position");
    assert_eq!(
        (
            position.outstandingPrincipalMinor,
            position.outstandingGratisMinor
        ),
        (U256::ZERO, U256::ZERO)
    );
    assert_eq!(position.state, 2);
    assert!(f.interest_paid > U256::ZERO);
    assert_eq!(state.liquid, INITIAL_GRATIS);
    assert_eq!(state.pledged, U256::ZERO);
    assert_eq!(
        state.account_stables,
        INITIAL_STABLES - PRINCIPAL - f.interest_paid
    );
    assert_eq!(state.vault_stables, LIQUIDITY + f.interest_paid);
    assert_eq!(state.shares, LIQUIDITY + f.interest_paid);
    assert_eq!(state.cca_stables, PRINCIPAL);
    assert_eq!(state.router_stables, U256::ZERO);
}

fn expected_interest(position: &ICredis::Position, timestamp: u64) -> U256 {
    let days = (timestamp - position.lastSettledAt) / DAY;
    position.outstandingPrincipalMinor * position.policyRate * U256::from(days)
        / U256::from(365_000_000)
}

fn receipt_timestamp(url: &str, receipt: &serde_json::Value) -> u64 {
    let block = eth::raw_json_result(
        url,
        "eth_getBlockByNumber",
        serde_json::json!([receipt["blockNumber"], false]),
    )
    .expect("receipt block");
    u64::from_str_radix(
        block["timestamp"]
            .as_str()
            .expect("block time")
            .trim_start_matches("0x"),
        16,
    )
    .expect("timestamp fits u64")
}
