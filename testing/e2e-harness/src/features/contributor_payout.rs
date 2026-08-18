//! Steps for the certified contributor payout authority.
//!
//! Reads the two precompile views the payout path publishes: the contributor
//! authority Lysis installs, and the payout round proceeds open.

use std::fs;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::sol;
use cucumber::{then, when};

use outbe_intex::payout::{
    build_contributor_range_proof, decode_contributor_leaf, encode_contributor_leaf,
    ContributorLeafData, CONTRIBUTOR_CHUNK_CAPACITY, CONTRIBUTOR_LEAF_BYTES,
};

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

sol! {
    #[sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    interface IIntexFactoryPayout {
        struct ContributorLeaf {
            address owner;
            bytes32 sourceTributeDigest;
            bytes4 sourceTributeIndex;
            uint256 nominal;
        }
        function payContributorBatch(
            uint32 worldwideDay,
            uint32 startIndex,
            ContributorLeaf[] calldata leaves,
            bytes32[] calldata proof
        ) external;
    }
}

/// Reads the day's dense contributor list from validator 0's finalized payout
/// artifact, the same file the production payout sender consumes.
fn certified_leaves(world: &World) -> Vec<ContributorLeafData> {
    let jobs = world
        .ocomp
        .domain_root(0)
        .expect("validator 0 OCOMP domain root")
        .join("supervisor-v1")
        .join("jobs");
    let mut artifact: Option<PathBuf> = None;
    for entry in fs::read_dir(&jobs).expect("read OCOMP job roots") {
        let candidate = entry
            .expect("read one OCOMP job root")
            .path()
            .join("contributor-payout-v1.bin");
        if candidate.is_file() {
            artifact = Some(candidate);
        }
    }
    let artifact = artifact.expect("finalization wrote a contributor payout artifact");
    let bytes = fs::read(&artifact).expect("read the contributor payout artifact");
    assert!(
        !bytes.is_empty() && bytes.len() % CONTRIBUTOR_LEAF_BYTES == 0,
        "payout artifact {} is not a whole number of records",
        artifact.display()
    );
    bytes
        .chunks_exact(CONTRIBUTOR_LEAF_BYTES)
        .map(|record| decode_contributor_leaf(record.try_into().expect("record is one leaf wide")))
        .collect()
}

/// Drives the payout exactly as production does: the validator's OCOMP delegate
/// sends one chunk-aligned batch per call, each proved against the certified
/// root, and rides the zero-fee lane its role grants.
fn pay_every_chunk(world: &World, day: u32, sender: &str) {
    let leaves = certified_leaves(world);
    let count = u32::try_from(leaves.len()).expect("contributor count fits u32");
    let encoded: Vec<[u8; CONTRIBUTOR_LEAF_BYTES]> =
        leaves.iter().map(encode_contributor_leaf).collect();
    let url = world.rpc.url(world.validators.primary_port());
    let mut start = 0_u32;
    while start < count {
        let end = count.min(start + CONTRIBUTOR_CHUNK_CAPACITY);
        let proof = build_contributor_range_proof(count, start, encoded.iter())
            .expect("build the contributor range proof");
        let batch = leaves[start as usize..end as usize]
            .iter()
            .map(|leaf| IIntexFactoryPayout::ContributorLeaf {
                owner: leaf.owner,
                sourceTributeDigest: B256::from_slice(&leaf.source_tribute_id[..32]),
                sourceTributeIndex: leaf.source_tribute_id[32..]
                    .try_into()
                    .expect("four trailing index bytes"),
                nominal: leaf.nominal,
            })
            .collect::<Vec<_>>();
        eth::send_call(
            &url,
            INTEX_FACTORY_ADDR,
            sender,
            &IIntexFactoryPayout::payContributorBatchCall {
                worldwideDay: day,
                startIndex: start,
                leaves: batch,
                proof,
            },
            None,
        )
        .expect("pay one contributor chunk");
        start = end;
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

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IIntexFactoryProceeds {
        function armProceedsForTest(uint32 worldwideDay, uint32[] chains, uint64 deadline) external;
        function distribute(uint32 worldwideDay, uint32 srcChainId) external payable;
    }
}

/// Hardhat account #0 — the sender a throwaway build accepts as the proceeds
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
    eth::send_call(
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

    eth::send_call(
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
}

#[then("every certified contributor is paid their share")]
fn contributors_are_paid(world: &mut World) {
    let day = worldwide_day(world);
    let url = world.rpc.url(world.validators.primary_port());
    let delegate = world
        .ocomp
        .ocomp_delegate_private_key(0)
        .expect("validator 0 OCOMP delegate key");
    pay_every_chunk(world, day, &delegate);
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let round = eth::read_call(
            &url,
            INTEX_FACTORY_ADDR,
            &IIntexFactoryContributorRound::contributorPayoutRoundCall { worldwideDay: day },
        )
        .expect("read the payout round");
        assert!(
            round.contributorCount > 0,
            "day {day} never opened a payout round for its proceeds"
        );
        if round.paidLeafCount >= round.contributorCount {
            assert!(
                round.paidSoFar > U256::ZERO,
                "every leaf is marked paid but nothing left the pot"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "day {day} stalled at {} of {} contributors paid",
            round.paidLeafCount,
            round.contributorCount
        );
        sleep(Duration::from_secs(3));
    }
}
