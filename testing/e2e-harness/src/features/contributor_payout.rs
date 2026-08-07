//! Steps for the certified contributor payout authority.
//!
//! Reads the two precompile views the payout path publishes: the contributor
//! authority Lysis installs, and the payout round proceeds open.

use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::sol;
use cucumber::then;

use crate::internal::eth;
use crate::world::World;

/// Intex precompile — certified contributor authority.
const INTEX_ADDR: Address = address!("0x0000000000000000000000000000000000001014");
/// IntexFactory precompile — contributor payout round.
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

fn authority_on(world: &World, port: u16, day: u32) -> (u64, B256, u32, U256) {
    let g = eth::read_call(
        &world.rpc.url(port),
        INTEX_ADDR,
        &IIntexContributorAuthority::certifiedContributorGenerationCall { worldwideDay: day },
    )
    .unwrap_or_else(|| panic!("read certified contributor generation on port {port}"));
    (
        g.seriesVersion,
        g.contributorRoot,
        g.contributorCount,
        g.eligibleNominalTotal,
    )
}

#[then("the certified contributor authority for that day is identical on every validator")]
fn certified_authority_is_installed(world: &mut World) {
    let day = worldwide_day(world);
    let ports = world.validators.committee_ports();
    let primary = authority_on(world, ports[0], day);

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
    assert!(
        primary.2 > 0,
        "certified contributor population for day {day} must not be empty"
    );
    assert!(
        !primary.3.is_zero(),
        "eligible nominal total for day {day} must not be zero"
    );

    // Quorum installed one authority, so every validator must expose that exact
    // record — a divergence here is a consensus fault, not a read race.
    for port in ports.iter().skip(1) {
        assert_eq!(
            authority_on(world, *port, day),
            primary,
            "validator on port {port} disagrees about the certified contributor authority"
        );
    }
}

#[then("that day has no open contributor payout round before proceeds arrive")]
fn no_payout_round_before_proceeds(world: &mut World) {
    let day = worldwide_day(world);
    let port = world.validators.primary_port();
    let round = eth::read_call(
        &world.rpc.url(port),
        INTEX_FACTORY_ADDR,
        &IIntexFactoryContributorRound::contributorPayoutRoundCall { worldwideDay: day },
    )
    .unwrap_or_else(|| panic!("read contributor payout round on port {port}"));

    assert!(
        round.amount.is_zero() && round.contributorCount == 0 && round.paidLeafCount == 0,
        "day {day} opened a payout round without proceeds: {round:?}"
    );
}
