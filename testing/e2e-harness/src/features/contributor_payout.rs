//! Steps for the certified contributor payout authority.
//!
//! Reads the two precompile views the payout path publishes: the contributor
//! authority Lysis installs, and the payout round proceeds open.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::sol;
use cucumber::{then, when};

use crate::internal::{economic_reference, eth};
use crate::world::ocomp::{OCOMP_PUBLIC_TRIBUTE_AMOUNT_ATTO, OCOMP_PUBLIC_TRIBUTE_AMOUNT_BASE};
use crate::world::rpc::FinalizedCheckpoint;
use crate::world::state::{
    ContributorPayoutEvidenceV1, ContributorPayoutSnapshotV1, ExpectedContributorV1,
};
use crate::world::World;

/// Intex precompile - certified contributor authority.
const INTEX_ADDR: Address = address!("0x0000000000000000000000000000000000001014");
/// IntexFactory precompile - contributor payout round.
const INTEX_FACTORY_ADDR: Address = address!("0x0000000000000000000000000000000000001015");

sol! {
    #[sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    interface IIntexContributorAuthority {
        struct CertifiedContributorGeneration {
            uint64 seriesVersion;
            bytes32 contributorRoot;
            uint32 contributorCount;
            uint256 eligibleNominalTotal;
        }
        function certifiedContributorGeneration(uint32 worldwideDay)
            external view returns (CertifiedContributorGeneration memory);
    }
}

sol! {
    #[sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    interface IIntexFactoryContributorRound {
        struct ContributorRound {
            uint256 amount;
            uint32 contributorCount;
            uint256 paidSoFar;
            uint32 paidLeafCount;
        }
        function contributorPayoutRound(uint32 worldwideDay)
            external view returns (ContributorRound memory);
    }
}

/// The worldwide day the preceding OCOMP steps drove, as they resolve it.
fn worldwide_day(world: &World) -> u32 {
    world
        .state
        .wwd
        .as_deref()
        .expect("scenario recorded a WorldwideDay")
        .parse::<u32>()
        .expect("numeric WorldwideDay")
}

fn authority_on(world: &World, port: u16, day: u32, height: u64) -> (u64, B256, u32, U256) {
    let g = eth::read_call_at(
        &world.rpc.url(port),
        INTEX_ADDR,
        &IIntexContributorAuthority::certifiedContributorGenerationCall { worldwideDay: day },
        height,
    )
    .unwrap_or_else(|| panic!("read certified contributor generation on port {port}"));
    (
        g.seriesVersion,
        g.contributorRoot,
        g.contributorCount,
        g.eligibleNominalTotal,
    )
}

/// These scenarios submit included USD offers: capacity uses the declared
/// input amount; the reward-bearing single offer explicitly submits 100 USD.
/// Normalize each amount with its independently read execution-time pricing.
/// No owner, weight or population is taken from the certified result.
fn expected_contributors(world: &World) -> Vec<ExpectedContributorV1> {
    let count = world.state.ocomp_capacity_tribute_tx_hashes.len();
    let (owners, amount, heights) = if count > 0 {
        let keys = world
            .state
            .ocomp_capacity_tribute_private_keys
            .get(..count)
            .expect("every submitted capacity offer has its fixture key");
        let owners = keys
            .iter()
            .map(|key| eth::address_of(key).expect("capacity owner key"))
            .collect::<Vec<_>>();
        let amount = super::tribute_expectations::amount_minor(
            OCOMP_PUBLIC_TRIBUTE_AMOUNT_BASE,
            OCOMP_PUBLIC_TRIBUTE_AMOUNT_ATTO,
        );
        let heights = world
            .state
            .ocomp_capacity_tribute_tx_hashes
            .iter()
            .map(|tx| super::tribute_expectations::offer_height(world, tx))
            .collect::<Vec<_>>();
        (owners, amount, heights)
    } else {
        assert!(
            world
                .state
                .ocomp_agent_reward
                .as_ref()
                .is_some_and(|reward| reward.offer_execution_block_number.is_some()),
            "expected the executed singleton reward-bearing offer fixture"
        );
        let key = world
            .validators
            .get(0)
            .evm_key()
            .expect("singleton offer owner key");
        (
            vec![eth::address_of(&key).expect("singleton offer owner")],
            U256::from(100_000_000),
            vec![world
                .state
                .ocomp_agent_reward
                .as_ref()
                .expect("reward offer")
                .offer_execution_block_number
                .expect("executed reward offer")],
        )
    };
    let unique: std::collections::BTreeSet<_> = owners.iter().copied().collect();
    assert_eq!(
        unique.len(),
        owners.len(),
        "fixture submits one offer per distinct owner"
    );
    assert_eq!(owners.len(), heights.len());
    let nominal = heights
        .into_iter()
        .map(|height| {
            super::tribute_expectations::usd_offer_terms_at(
                world,
                worldwide_day(world),
                height,
                amount,
            )
            .0
        })
        .collect::<Vec<_>>();
    let (shares, _) = economic_reference::contributor_shares(U256::from(PROCEEDS_UNITS), &nominal);
    owners
        .into_iter()
        .zip(nominal)
        .zip(shares)
        .map(
            |((owner, nominal_minor), share_coen_units)| ExpectedContributorV1 {
                owner,
                nominal_minor,
                share_coen_units,
            },
        )
        .collect()
}

fn checkpoint(world: &World, min_height: u64) -> FinalizedCheckpoint {
    world
        .rpc
        .wait_finalized_checkpoint(&world.validators.committee_ports(), min_height, 120)
        .expect("all validators reached the contributor checkpoint")
}

fn reverify(world: &World, checkpoint: FinalizedCheckpoint) {
    for port in world.validators.committee_ports() {
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, checkpoint.height)
                .expect("contributor checkpoint"),
            checkpoint
        );
    }
}

#[then("the certified contributor authority for that day is identical on every validator")]
fn certified_authority_is_installed(world: &mut World) {
    let day = worldwide_day(world);
    let ports = world.validators.committee_ports();
    let checkpoint = checkpoint(world, 1);
    let expected = expected_contributors(world);
    let nominal_total: U256 = expected.iter().map(|leaf| leaf.nominal_minor).sum();
    let primary = authority_on(world, ports[0], day, checkpoint.height);

    // Installed at all: a zero root means the activation frame never ran.
    assert_ne!(
        primary.1,
        B256::ZERO,
        "Lysis activation left no certified contributor root for day {day}"
    );
    assert!(
        primary.0 > 0,
        "certified contributor series version must advance past zero for day {day}"
    );
    assert_eq!(
        usize::try_from(primary.2).expect("contributor count"),
        expected.len(),
        "certified population differs from submitted offers"
    );
    assert_eq!(
        primary.3, nominal_total,
        "certified nominal total differs from independently normalized offers"
    );

    // Quorum installed one authority, so every validator must expose that exact
    // record - a divergence here is a consensus fault, not a read race.
    for port in ports.iter().skip(1) {
        assert_eq!(
            authority_on(world, *port, day, checkpoint.height),
            primary,
            "validator on port {port} disagrees about the certified contributor authority"
        );
    }
    reverify(world, checkpoint);
}

#[then("that day has no open contributor payout round before proceeds arrive")]
fn no_payout_round_before_proceeds(world: &mut World) {
    let day = worldwide_day(world);
    let checkpoint = checkpoint(world, 1);
    for port in world.validators.committee_ports() {
        let round = round_on(world, port, day, checkpoint.height);
        assert!(
            round.amount.is_zero()
                && round.contributorCount == 0
                && round.paidLeafCount == 0
                && round.paidSoFar.is_zero(),
            "day {day} opened a payout round without proceeds on port {port}: {round:?}"
        );
    }
    reverify(world, checkpoint);
}

fn round_on(
    world: &World,
    port: u16,
    day: u32,
    height: u64,
) -> IIntexFactoryContributorRound::ContributorRound {
    eth::read_call_at(
        &world.rpc.url(port),
        INTEX_FACTORY_ADDR,
        &IIntexFactoryContributorRound::contributorPayoutRoundCall { worldwideDay: day },
        height,
    )
    .unwrap_or_else(|| panic!("read contributor payout round on port {port} at {height}"))
}

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IIntexFactoryProceeds {
        function armProceedsForTest(uint32 worldwideDay, uint32[] chains, uint64 deadline) external;
        function distribute(uint32 worldwideDay, uint32 srcChainId) external payable;
    }
}

/// Hardhat account #0 - the sender a throwaway build accepts as the proceeds
/// source, since the production one is a contract nobody can sign for.
const PROCEEDS_SENDER_KEY: &str =
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const PROCEEDS_SENDER: Address = address!("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
/// Comfortably above the contributor count, so no leaf floors to nothing.
const PROCEEDS_UNITS: u128 = 1_000_000;
const PROCEEDS_GAS_UNITS: u128 = 10_000_000;

#[when("the day's auction proceeds arrive from one chain")]
fn proceeds_arrive(world: &mut World) {
    let day = worldwide_day(world);
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let chain_id = u32::try_from(world.rpc.chain_id(port).expect("committee chain id"))
        .expect("chain id fits u32");
    let funder = world
        .state
        .ocomp_capacity_tribute_private_keys
        .first()
        .cloned()
        .expect("capacity fixture funded its owners");

    // Genesis funds only validators, so the accepted sender starts empty.
    eth::send_value(
        &url,
        PROCEEDS_SENDER,
        &funder,
        U256::from(PROCEEDS_GAS_UNITS + PROCEEDS_UNITS),
    )
    .expect("fund the proceeds sender");

    // Issuance arms this in production; a payout scenario runs no auction.
    let deadline = u64::MAX;
    let arm_hash = eth::send_call(
        &url,
        INTEX_FACTORY_ADDR,
        &funder,
        &IIntexFactoryProceeds::armProceedsForTestCall {
            worldwideDay: day,
            chains: vec![chain_id],
            deadline,
        },
        None,
    )
    .expect("arm the day's proceeds fan-in");

    let arm_height = world
        .rpc
        .receipt_block_number(&arm_hash, port)
        .expect("mined arm receipt");
    let before_checkpoint = checkpoint(world, arm_height);
    let contributors = expected_contributors(world);
    let before = balances_at(world, before_checkpoint, &contributors);
    let expected_paid: U256 = contributors.iter().map(|leaf| leaf.share_coen_units).sum();
    world.state.ocomp_contributor_payout = Some(ContributorPayoutEvidenceV1 {
        worldwide_day: day,
        amount: U256::from(PROCEEDS_UNITS),
        expected_paid,
        expected_burned: U256::from(PROCEEDS_UNITS)
            .checked_sub(expected_paid)
            .expect("shares fit pot"),
        contributors,
        before,
        after: None,
        validator_ports: world.validators.committee_ports(),
    });

    let proceeds_hash = eth::send_call(
        &url,
        INTEX_FACTORY_ADDR,
        PROCEEDS_SENDER_KEY,
        &IIntexFactoryProceeds::distributeCall {
            worldwideDay: day,
            srcChainId: chain_id,
        },
        Some(U256::from(PROCEEDS_UNITS)),
    )
    .expect("credit the day's proceeds");
    let proceeds_height = world
        .rpc
        .receipt_block_number(&proceeds_hash, port)
        .expect("mined proceeds receipt");
    checkpoint(world, proceeds_height);
}

#[then("every certified contributor is paid their share")]
fn contributors_are_paid(world: &mut World) {
    let day = worldwide_day(world);
    let expected = world
        .state
        .ocomp_contributor_payout
        .as_ref()
        .expect("pre-proceeds snapshot");
    assert_eq!(day, expected.worldwide_day);
    let deadline = Instant::now() + Duration::from_secs(600);
    let complete_checkpoint = loop {
        let checkpoint = checkpoint(world, expected.before.height);
        let mut complete = true;
        for port in &expected.validator_ports {
            let round = round_on(world, *port, day, checkpoint.height);
            assert_eq!(
                round.amount, expected.amount,
                "proceeds round amount on port {port}"
            );
            assert_eq!(
                usize::try_from(round.contributorCount).expect("contributor count"),
                expected.contributors.len(),
                "proceeds round population on port {port}"
            );
            assert!(
                round.paidLeafCount <= round.contributorCount,
                "paid leaf count exceeds population"
            );
            assert!(
                round.paidSoFar <= expected.expected_paid,
                "round overpaid contributors"
            );
            if round.paidLeafCount == round.contributorCount {
                assert_eq!(
                    round.paidSoFar, expected.expected_paid,
                    "completed round paid amount on port {port}"
                );
            } else {
                complete = false;
            }
        }
        reverify(world, checkpoint);
        if complete {
            break checkpoint;
        }
        assert!(
            Instant::now() < deadline,
            "day {day} did not finish paying every expected contributor"
        );
        sleep(Duration::from_secs(3));
    };
    let after = balances_at(world, complete_checkpoint, &expected.contributors);
    for (index, leaf) in expected.contributors.iter().enumerate() {
        assert_eq!(
            after.owner_balances[index],
            expected.before.owner_balances[index]
                .checked_add(leaf.share_coen_units)
                .expect("expected recipient balance fits U256"),
            "contributor {} did not receive its exact share",
            leaf.owner
        );
    }
    // Every input unit left this round, either as a recipient share or as the
    // explicitly calculated dust burn. Other days' factory funds are conserved.
    assert_eq!(
        after.factory_balance, expected.before.factory_balance,
        "completed round stranded or spent unrelated proceeds"
    );
    world
        .state
        .ocomp_contributor_payout
        .as_mut()
        .expect("payout evidence")
        .after = Some(after);
}

fn balances_at(
    world: &World,
    checkpoint: FinalizedCheckpoint,
    contributors: &[ExpectedContributorV1],
) -> ContributorPayoutSnapshotV1 {
    reverify(world, checkpoint);
    let mut common = None;
    for port in world.validators.committee_ports() {
        let native = |account: Address| {
            let value = eth::raw_json_with_params(
                &world.rpc.url(port),
                "eth_getBalance",
                serde_json::json!([
                    format!("{account:#x}"),
                    format!("0x{:x}", checkpoint.height)
                ]),
            )
            .expect("native balance at contributor checkpoint");
            U256::from_str_radix(
                value
                    .as_str()
                    .expect("balance hex")
                    .trim_start_matches("0x"),
                16,
            )
            .expect("native contributor balance fits U256")
        };
        let balances = (
            native(INTEX_FACTORY_ADDR),
            contributors
                .iter()
                .map(|leaf| native(leaf.owner))
                .collect::<Vec<_>>(),
        );
        if let Some(common) = &common {
            assert_eq!(
                &balances, common,
                "contributor balances disagree on port {port}"
            );
        } else {
            common = Some(balances);
        }
    }
    reverify(world, checkpoint);
    let (factory_balance, owner_balances) = common.expect("nonempty contributor validator cohort");
    ContributorPayoutSnapshotV1 {
        height: checkpoint.height,
        block_hash: checkpoint.block_hash,
        state_root: checkpoint.state_root,
        factory_balance,
        owner_balances,
    }
}
