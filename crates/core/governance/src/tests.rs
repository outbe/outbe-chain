use alloy_primitives::{address, keccak256, Address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::GOVERNANCE_ADDRESS;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use outbe_primitives::error::PrecompileError;
use outbe_validatorset::contract::ValidatorSet;

use crate::errors::GovernanceError;
use crate::precompile::IGovernance;
use crate::state::{author_index_key, ProposalMeta};
use crate::status;
use crate::GovernanceContract;

const AUTH: Address = address!("0x1111111111111111111111111111111111111111");
const OTHER: Address = address!("0x2222222222222222222222222222222222222222");
const AUTHOR: Address = address!("0x3333333333333333333333333333333333333333");
const LATE: Address = address!("0x4444444444444444444444444444444444444444");

/// Registers `addr` and moves it to ACTIVE in the live `ValidatorSet`.
fn activate_validator(storage: StorageHandle, addr: Address, seed: u8) {
    let owner = address!("0xffffffffffffffffffffffffffffffffffffffff");
    let mut vs = ValidatorSet::new(storage);
    vs.config_owner.write(owner).unwrap();
    vs.set_config_max_validators(100).unwrap();
    let mut pk = [0u8; 48];
    pk[0] = seed;
    vs.register_validator(owner, addr, &pk).unwrap();
    vs.activate_validator_via_boundary_for_test(addr).unwrap();
}

fn with_governance<R>(f: impl FnOnce(&mut GovernanceContract) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        activate_validator(storage.clone(), AUTH, 1);
        let mut gov = GovernanceContract::new(storage.clone());
        f(&mut gov)
    })
}

fn assert_not_authorized<T: std::fmt::Debug>(result: outbe_primitives::error::Result<T>) {
    let expected = GovernanceError::NotAuthorized.to_string();
    assert!(
        matches!(&result, Err(PrecompileError::Revert(msg)) if *msg == expected),
        "expected NotAuthorized, got {result:?}"
    );
}

// ---------------------------------------------------------------- canon ---

#[test]
fn canon_roundtrip_and_versioning() {
    with_governance(|gov| {
        // empty initial state
        let (text, ver, hash) = gov.get_canon().unwrap();
        assert_eq!(text, "");
        assert_eq!(ver, 0);
        assert!(hash.is_zero());

        // first update -> version 1
        let v = gov.update_canon(AUTH, "gain takes time").unwrap();
        assert_eq!(v, 1);
        let (text, ver, hash) = gov.get_canon().unwrap();
        assert_eq!(text, "gain takes time");
        assert_eq!(ver, 1);
        assert_eq!(hash, keccak256(b"gain takes time"));
        assert_eq!(gov.canon_revision_hash(1).unwrap(), hash);

        // second update -> version 2, revision map keeps both
        let v = gov.update_canon(AUTH, "gain takes more time").unwrap();
        assert_eq!(v, 2);
        assert_eq!(
            gov.canon_revision_hash(1).unwrap(),
            keccak256(b"gain takes time")
        );
        assert_eq!(
            gov.canon_revision_hash(2).unwrap(),
            keccak256(b"gain takes more time")
        );
    });
}

#[test]
fn meta_canon_independent_from_canon() {
    with_governance(|gov| {
        gov.update_canon(AUTH, "canon v1").unwrap();
        gov.update_meta_canon(AUTH, "meta v1").unwrap();
        assert_eq!(gov.get_canon().unwrap().1, 1);
        assert_eq!(gov.get_meta_canon().unwrap().1, 1);
        assert_eq!(gov.get_canon().unwrap().0, "canon v1");
        assert_eq!(gov.get_meta_canon().unwrap().0, "meta v1");
    });
}

#[test]
fn canon_update_requires_authority() {
    with_governance(|gov| {
        assert!(gov.update_canon(OTHER, "sneaky").is_err());
        assert!(gov.update_meta_canon(OTHER, "sneaky").is_err());
        // still empty
        assert_eq!(gov.get_canon().unwrap().1, 0);
    });
}

#[test]
fn canon_overwrite_shorter_is_clean() {
    with_governance(|gov| {
        let long = "x".repeat(200);
        gov.update_canon(AUTH, &long).unwrap();
        gov.update_canon(AUTH, "short").unwrap();
        let (text, _, hash) = gov.get_canon().unwrap();
        assert_eq!(text, "short");
        assert_eq!(hash, keccak256(b"short"));
    });
}

#[test]
fn canon_64k_roundtrip() {
    with_governance(|gov| {
        let big = "a".repeat(64 * 1024);
        gov.update_canon(AUTH, &big).unwrap();
        assert_eq!(gov.get_canon().unwrap().0.len(), 64 * 1024);
    });
}

#[test]
fn canon_rejects_empty_and_oversize() {
    with_governance(|gov| {
        assert!(gov.update_canon(AUTH, "").is_err());
        let too_big = "a".repeat(crate::runtime::MAX_TEXT_BYTES + 1);
        assert!(gov.update_canon(AUTH, &too_big).is_err());
    });
}

// ------------------------------------------------------------ proposals ---

#[test]
fn oip_gip_independent_id_sequences() {
    with_governance(|gov| {
        assert_eq!(gov.submit_oip(AUTHOR, "oip one").unwrap(), U256::from(1));
        assert_eq!(gov.submit_oip(AUTHOR, "oip two").unwrap(), U256::from(2));
        assert_eq!(gov.submit_gip(AUTHOR, "gip one").unwrap(), U256::from(1));
        assert_eq!(gov.oip_count().unwrap(), 2);
        assert_eq!(gov.gip_count().unwrap(), 1);
    });
}

#[test]
fn oip_submit_get_roundtrip_text_in_record() {
    with_governance(|gov| {
        let id = gov.submit_oip(AUTHOR, "proposal body text").unwrap();
        let o = gov.get_oip(id).unwrap().unwrap();
        assert_eq!(o.author, AUTHOR);
        assert_eq!(o.status, status::DRAFT);
        assert_eq!(o.text, "proposal body text");
        assert_eq!(o.text_hash, keccak256(b"proposal body text"));
    });
}

#[test]
fn oip_text_update_author_only_and_status_gated() {
    with_governance(|gov| {
        let id = gov.submit_oip(AUTHOR, "v1").unwrap();

        // non-author cannot edit
        assert!(gov.update_oip_text(OTHER, id, "hijack").is_err());

        // author edits in Draft
        gov.update_oip_text(AUTHOR, id, "v2 longer text").unwrap();
        let o = gov.get_oip(id).unwrap().unwrap();
        assert_eq!(o.text, "v2 longer text");
        assert_eq!(o.text_hash, keccak256(b"v2 longer text"));

        // Move to Approved. After this, the text is not editable.
        gov.set_oip_status(AUTH, id, status::APPROVED).unwrap();
        assert!(gov.update_oip_text(AUTHOR, id, "v3").is_err());
    });
}

#[test]
fn oip_text_update_shrink_keeps_hash_consistent() {
    with_governance(|gov| {
        let id = gov.submit_oip(AUTHOR, &"z".repeat(300)).unwrap();
        gov.update_oip_text(AUTHOR, id, "tiny").unwrap();
        let o = gov.get_oip(id).unwrap().unwrap();
        assert_eq!(o.text, "tiny");
        assert_eq!(o.text_hash, keccak256(b"tiny"));
    });
}

#[test]
fn status_change_preserves_text() {
    with_governance(|gov| {
        let id = gov.submit_oip(AUTHOR, "keep me intact").unwrap();
        gov.set_oip_status(AUTH, id, status::APPROVED).unwrap();
        gov.set_oip_status(AUTH, id, status::IMPLEMENTED).unwrap();
        let o = gov.get_oip(id).unwrap().unwrap();
        assert_eq!(o.text, "keep me intact");
        assert_eq!(o.status, status::IMPLEMENTED);
    });
}

#[test]
fn status_transitions_enforced() {
    with_governance(|gov| {
        let id = gov.submit_oip(AUTHOR, "body").unwrap();
        // invalid: Draft -> Implemented
        assert!(gov.set_oip_status(AUTH, id, status::IMPLEMENTED).is_err());
        // valid: Draft -> Approved -> Implemented
        gov.set_oip_status(AUTH, id, status::APPROVED).unwrap();
        assert!(gov.set_oip_status(AUTH, id, status::REJECTED).is_err()); // Approved -> Rejected invalid
        gov.set_oip_status(AUTH, id, status::IMPLEMENTED).unwrap();
        // Implemented is terminal
        assert!(gov.set_oip_status(AUTH, id, status::DRAFT).is_err());
    });
}

#[test]
fn status_change_requires_authority_except_author_resubmit() {
    with_governance(|gov| {
        let id = gov.submit_oip(AUTHOR, "body").unwrap();
        // non-authority cannot approve
        assert!(gov.set_oip_status(OTHER, id, status::APPROVED).is_err());

        // authority sends to Rework
        gov.set_oip_status(AUTH, id, status::REWORK).unwrap();

        // author may resubmit (Rework -> Draft) despite not being an authority
        gov.set_oip_status(AUTHOR, id, status::DRAFT).unwrap();
        assert_eq!(gov.get_oip(id).unwrap().unwrap().status, status::DRAFT);

        // a random non-author non-authority cannot do the same
        gov.set_oip_status(AUTH, id, status::REWORK).unwrap();
        assert!(gov.set_oip_status(OTHER, id, status::DRAFT).is_err());
    });
}

// ------------------------------------------- authority = live ValidatorSet ---

#[test]
fn authority_follows_live_validator_set() {
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_block_number(1); // jail records a non-zero height
    StorageHandle::enter(&mut storage, |storage| {
        activate_validator(storage.clone(), AUTH, 1);
        let mut gov = GovernanceContract::new(storage.clone());
        assert!(gov.is_authority(AUTH).unwrap());
        // An unknown address is not an authority and the check does not fail.
        assert!(!gov.is_authority(OTHER).unwrap());
        gov.update_canon(AUTH, "v1").unwrap();

        // A validator activated after genesis can write.
        assert!(!gov.is_authority(LATE).unwrap());
        activate_validator(storage.clone(), LATE, 2);
        assert!(gov.is_authority(LATE).unwrap());
        gov.update_canon(LATE, "v2").unwrap();

        // A validator that leaves ACTIVE loses write rights at once.
        let id = gov.submit_oip(AUTHOR, "body").unwrap();
        ValidatorSet::new(storage.clone())
            .jail_validator(AUTH)
            .unwrap();
        assert!(!gov.is_authority(AUTH).unwrap());
        assert_not_authorized(gov.update_canon(AUTH, "v3"));
        assert_not_authorized(gov.update_meta_canon(AUTH, "m"));
        assert_not_authorized(gov.set_oip_status(AUTH, id, status::APPROVED));
        assert_eq!(gov.get_canon().unwrap().1, 2);
    });
}

#[test]
fn gip_mirrors_oip_behavior() {
    with_governance(|gov| {
        let id = gov.submit_gip(AUTHOR, "gip body").unwrap();
        let g = gov.get_gip(id).unwrap().unwrap();
        assert_eq!(g.text, "gip body");
        gov.set_gip_status(AUTH, id, status::APPROVED).unwrap();
        assert_eq!(gov.get_gip(id).unwrap().unwrap().status, status::APPROVED);
    });
}

#[test]
fn proposal_kinds_keep_text_metadata_and_events_separate() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(10);
    StorageHandle::enter(&mut provider, |storage| {
        activate_validator(storage.clone(), AUTH, 1);
        let mut gov = GovernanceContract::new(storage);
        assert_eq!(
            gov.submit_oip(AUTHOR, "original OIP").unwrap(),
            U256::from(1)
        );
        assert_eq!(gov.submit_gip(OTHER, "g").unwrap(), U256::from(1));
    });

    provider.set_block_number(20);
    let id = U256::from(1);
    let oip_hash = keccak256(b"o");
    let gip_hash = keccak256(b"longer GIP replacement");
    StorageHandle::enter(&mut provider, |storage| {
        let mut gov = GovernanceContract::new(storage);
        gov.update_oip_text(AUTHOR, id, "o").unwrap();
        gov.update_gip_text(OTHER, id, "longer GIP replacement")
            .unwrap();
        gov.set_oip_status(AUTH, id, status::APPROVED).unwrap();
        gov.set_gip_status(AUTH, id, status::REJECTED).unwrap();
        assert_eq!(gov.get_oip(id).unwrap().unwrap().text, "o");
        assert_eq!(
            gov.get_gip(id).unwrap().unwrap().text,
            "longer GIP replacement"
        );
        assert_eq!(
            gov.oips_by_author(AUTHOR, U256::ZERO, U256::from(10))
                .unwrap(),
            vec![ProposalMeta {
                id,
                author: AUTHOR,
                status: status::APPROVED,
                created_block: 10,
                updated_block: 20,
                text_hash: oip_hash,
            }]
        );
        assert_eq!(
            gov.gips_by_status(status::REJECTED, U256::ZERO, U256::from(10))
                .unwrap(),
            vec![ProposalMeta {
                id,
                author: OTHER,
                status: status::REJECTED,
                created_block: 10,
                updated_block: 20,
                text_hash: gip_hash,
            }]
        );
    });

    assert_eq!(
        provider.get_events(GOVERNANCE_ADDRESS),
        &vec![
            IGovernance::OipSubmitted {
                id,
                author: AUTHOR,
                textHash: keccak256(b"original OIP"),
            }
            .encode_log_data(),
            IGovernance::GipSubmitted {
                id,
                author: OTHER,
                textHash: keccak256(b"g"),
            }
            .encode_log_data(),
            IGovernance::OipTextUpdated {
                id,
                textHash: oip_hash
            }
            .encode_log_data(),
            IGovernance::GipTextUpdated {
                id,
                textHash: gip_hash
            }
            .encode_log_data(),
            IGovernance::OipStatusChanged {
                id,
                oldStatus: status::DRAFT,
                newStatus: status::APPROVED,
            }
            .encode_log_data(),
            IGovernance::GipStatusChanged {
                id,
                oldStatus: status::DRAFT,
                newStatus: status::REJECTED,
            }
            .encode_log_data(),
        ]
    );
}

#[test]
fn missing_proposal_errors() {
    with_governance(|gov| {
        assert!(gov.get_oip(U256::from(99)).unwrap().is_none());
        assert!(gov
            .set_oip_status(AUTH, U256::from(99), status::APPROVED)
            .is_err());
        assert!(gov.update_oip_text(AUTHOR, U256::from(99), "x").is_err());
    });
}

#[test]
fn proposal_rejects_empty_and_oversize_text() {
    with_governance(|gov| {
        assert!(gov.submit_oip(AUTHOR, "").is_err());
        let too_big = "a".repeat(crate::runtime::MAX_TEXT_BYTES + 1);
        assert!(gov.submit_oip(AUTHOR, &too_big).is_err());
    });
}

// ------------------------------------------------------- storage layout ---

/// Pins the contract's slot layout, which `scripts/seed_genesis.py`
/// (`seed_governance`) hardcodes. If someone reorders or inserts a field, this
/// test fails. That failure signals that the seeder must change in lockstep.
#[test]
fn storage_layout_matches_seeder() {
    with_governance(|gov| {
        assert_eq!(gov.meta_canon_version.slot(), U256::from(1));
        assert_eq!(gov.meta_canon_hash.slot(), U256::from(2));
        assert_eq!(gov.meta_canon_revisions.base_slot(), U256::from(3));
        assert_eq!(gov.canon_version.slot(), U256::from(5));
        assert_eq!(gov.canon_hash.slot(), U256::from(6));
        assert_eq!(gov.canon_revisions.base_slot(), U256::from(7));
        assert_eq!(gov.next_oip_id.slot(), U256::from(8));
        assert_eq!(gov.next_gip_id.slot(), U256::from(9));
        assert_eq!(gov.reserved_authorities.base_slot(), U256::from(10));
        // Record maps come last so growing Oip/Gip never shifts a seeded slot.
        assert_eq!(gov.oips.base_slot(), U256::from(11));
        assert_eq!(gov.gips.base_slot(), U256::from(17)); // 11 + Oip::SLOTS(6)
                                                          // Indexes are appended after gips (gips ends at 22): they never touch the
                                                          // seeded region and thus don't affect seed_genesis.py.
        assert_eq!(gov.oip_author_count.base_slot(), U256::from(23));
        assert_eq!(gov.oip_author_ids.base_slot(), U256::from(24));
        assert_eq!(gov.oip_by_status.base_slot(), U256::from(25));
        assert_eq!(gov.gip_author_count.base_slot(), U256::from(26));
        assert_eq!(gov.gip_author_ids.base_slot(), U256::from(27));
        assert_eq!(gov.gip_by_status.base_slot(), U256::from(28));
        // meta_canon text is at slot 0, canon text at slot 4 (adjacency: the
        // one-slot version field sits immediately after each text field).
    });
}

// ------------------------------------ cross-impl: seeder writes, we read ---

/// End-to-end proof that `scripts/seed_genesis.py::seed_governance` and the Rust
/// contract agree on the storage layout. These `(slot, value)` pairs are the
/// verbatim output of the Python seeder for canon="gain takes time",
/// meta="meta rules". We load them raw and read them back
/// through `GovernanceContract`. Python computed the slots (keccak/StorageBytes).
/// Rust resolves them independently. Agreement proves the seam.
#[test]
fn reads_python_seeder_output() {
    use outbe_primitives::addresses::GOVERNANCE_ADDRESS;

    // AUTO-GENERATED fixture from scripts/seed_genesis.py seed_governance()
    let seeded: &[(&str, &str)] = &[
        (
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            "0x6d6574612072756c657300000000000000000000000000000000000000000014",
        ),
        (
            "0x0000000000000000000000000000000000000000000000000000000000000001",
            "0x0000000000000000000000000000000000000000000000000000000000000001",
        ),
        (
            "0x0000000000000000000000000000000000000000000000000000000000000002",
            "0xa540c01c4bd1ce33fc1cb92fd109c95ec7068e456ed7e0533fd1adb7e98158c6",
        ),
        (
            "0x0000000000000000000000000000000000000000000000000000000000000004",
            "0x6761696e2074616b65732074696d65000000000000000000000000000000001e",
        ),
        (
            "0x0000000000000000000000000000000000000000000000000000000000000005",
            "0x0000000000000000000000000000000000000000000000000000000000000001",
        ),
        (
            "0x0000000000000000000000000000000000000000000000000000000000000006",
            "0x93758306b69d86c6e2158df53b14c3847d00947f2e6ae98e7f1a6d65ee2e7afb",
        ),
        (
            "0xa15bc60c955c405d20d9149c709e2460f1c2d9a497496a7f46004d1772c3054c",
            "0xa540c01c4bd1ce33fc1cb92fd109c95ec7068e456ed7e0533fd1adb7e98158c6",
        ),
        (
            "0xb39221ace053465ec3453ce2b36430bd138b997ecea25c1043da0c366812b828",
            "0x93758306b69d86c6e2158df53b14c3847d00947f2e6ae98e7f1a6d65ee2e7afb",
        ),
    ];
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        for (slot, val) in seeded {
            let slot = slot.parse::<U256>().unwrap();
            let val = val.parse::<U256>().unwrap();
            storage.sstore(GOVERNANCE_ADDRESS, slot, val).unwrap();
        }
        let gov = GovernanceContract::new(storage.clone());

        let (canon, cver, chash) = gov.get_canon().unwrap();
        assert_eq!(canon, "gain takes time");
        assert_eq!(cver, 1);
        assert_eq!(chash, keccak256(b"gain takes time"));

        let (meta, mver, mhash) = gov.get_meta_canon().unwrap();
        assert_eq!(meta, "meta rules");
        assert_eq!(mver, 1);
        assert_eq!(mhash, keccak256(b"meta rules"));

        assert_eq!(
            gov.canon_revision_hash(1).unwrap(),
            keccak256(b"gain takes time")
        );
    });
}

// ------------------------------------------------- indexes: by author ---

#[test]
fn author_index_lists_own_proposals() {
    with_governance(|gov| {
        gov.submit_oip(AUTHOR, "a").unwrap(); // id 1
        gov.submit_oip(AUTHOR, "b").unwrap(); // id 2
        gov.submit_oip(OTHER, "c").unwrap(); // id 3

        let mine = gov
            .oips_by_author(AUTHOR, U256::ZERO, U256::from(1000))
            .unwrap();
        assert_eq!(mine.len(), 2);
        assert_eq!(
            mine.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![U256::from(1), U256::from(2)]
        );
        assert!(mine.iter().all(|m| m.author == AUTHOR));

        let theirs = gov
            .oips_by_author(OTHER, U256::ZERO, U256::from(1000))
            .unwrap();
        assert_eq!(theirs.len(), 1);
        assert_eq!(theirs[0].id, U256::from(3));

        // unknown author -> empty
        assert!(gov
            .oips_by_author(AUTH, U256::ZERO, U256::from(1000))
            .unwrap()
            .is_empty());
    });
}

#[test]
fn author_index_is_per_kind() {
    with_governance(|gov| {
        gov.submit_oip(AUTHOR, "oip").unwrap();
        gov.submit_gip(AUTHOR, "gip").unwrap();
        assert_eq!(
            gov.oips_by_author(AUTHOR, U256::ZERO, U256::from(1000))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            gov.gips_by_author(AUTHOR, U256::ZERO, U256::from(1000))
                .unwrap()
                .len(),
            1
        );
    });
}

fn assert_author_index_full<T: std::fmt::Debug>(result: outbe_primitives::error::Result<T>) {
    let expected = GovernanceError::AuthorIndexFull.to_string();
    assert!(
        matches!(&result, Err(PrecompileError::Revert(msg)) if *msg == expected),
        "expected AuthorIndexFull, got {result:?}"
    );
}

#[test]
fn oip_submit_refuses_full_author_index() {
    with_governance(|gov| {
        let first = gov.submit_oip(AUTHOR, "a").unwrap();
        gov.oip_author_count.write(&AUTHOR, u32::MAX).unwrap();

        assert_author_index_full(gov.submit_oip(AUTHOR, "b"));

        // Refusing the submission must not change any part of the existing index.
        assert_eq!(gov.oip_author_count.read(&AUTHOR).unwrap(), u32::MAX);
        let slot0 = gov.oip_author_ids.read(&author_index_key(AUTHOR, 0));
        assert_eq!(slot0.unwrap(), first);
        assert_eq!(gov.oip_count().unwrap(), 1);
    });
}

#[test]
fn gip_submit_refuses_full_author_index() {
    with_governance(|gov| {
        let first = gov.submit_gip(AUTHOR, "a").unwrap();
        gov.gip_author_count.write(&AUTHOR, u32::MAX).unwrap();

        assert_author_index_full(gov.submit_gip(AUTHOR, "b"));

        assert_eq!(gov.gip_author_count.read(&AUTHOR).unwrap(), u32::MAX);
        let slot0 = gov.gip_author_ids.read(&author_index_key(AUTHOR, 0));
        assert_eq!(slot0.unwrap(), first);
        assert_eq!(gov.gip_count().unwrap(), 1);
    });
}

fn ids(rows: Vec<crate::state::ProposalMeta>) -> Vec<U256> {
    rows.into_iter().map(|m| m.id).collect()
}

#[test]
fn author_listing_skips_empty_index_slot() {
    with_governance(|gov| {
        // Each count includes a slot that was never populated.
        let oip = gov.submit_oip(AUTHOR, "o").unwrap();
        gov.oip_author_count.write(&AUTHOR, 2).unwrap();
        let gip = gov.submit_gip(AUTHOR, "g").unwrap();
        gov.gip_author_count.write(&AUTHOR, 2).unwrap();

        let page = U256::from(1000);
        let oips = gov.oips_by_author(AUTHOR, U256::ZERO, page).unwrap();
        assert_eq!(ids(oips), vec![oip]);
        let gips = gov.gips_by_author(AUTHOR, U256::ZERO, page).unwrap();
        assert_eq!(ids(gips), vec![gip]);
        assert!(!oip.is_zero() && !gip.is_zero());
    });
}

// -------------------------------------------------- indexes: by status ---

fn all(g: &GovernanceContract, status: u8) -> Vec<U256> {
    g.oips_by_status(status, U256::ZERO, U256::from(1000))
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect()
}

#[test]
fn author_and_draft_listings_agree_for_single_author() {
    with_governance(|gov| {
        for i in 0..3u32 {
            gov.submit_oip(AUTHOR, &format!("p{i}")).unwrap();
        }
        let by_author = gov
            .oips_by_author(AUTHOR, U256::ZERO, U256::from(1000))
            .unwrap();
        let mut author_ids = ids(by_author);
        let mut draft_ids = all(gov, status::DRAFT);
        author_ids.sort();
        draft_ids.sort();
        assert_eq!(author_ids.len(), 3);
        assert_eq!(author_ids, draft_ids);
    });
}

#[test]
fn status_move_relocates_id_between_buckets() {
    with_governance(|gov| {
        let a = gov.submit_oip(AUTHOR, "a").unwrap();
        let b = gov.submit_oip(AUTHOR, "b").unwrap();
        let c = gov.submit_oip(AUTHOR, "c").unwrap(); // stays Draft

        gov.set_oip_status(AUTH, a, status::APPROVED).unwrap();
        gov.set_oip_status(AUTH, a, status::IMPLEMENTED).unwrap(); // Approved -> Implemented
        gov.set_oip_status(AUTH, b, status::REJECTED).unwrap();

        // each id sits in exactly its current status bucket, nowhere else
        assert_eq!(all(gov, status::DRAFT), vec![c]);
        assert!(all(gov, status::APPROVED).is_empty()); // a moved on to Implemented
        assert_eq!(all(gov, status::IMPLEMENTED), vec![a]);
        assert_eq!(all(gov, status::REJECTED), vec![b]);

        // counts, and the invariant sum = total
        assert_eq!(gov.oip_count_by_status(status::DRAFT).unwrap(), 1);
        assert_eq!(gov.oip_count_by_status(status::IMPLEMENTED).unwrap(), 1);
        assert_eq!(gov.oip_count_by_status(status::REJECTED).unwrap(), 1);
        let total: u32 = (0..=4).map(|s| gov.oip_count_by_status(s).unwrap()).sum();
        assert_eq!(total as u64, gov.oip_count().unwrap());
    });
}

#[test]
fn rework_then_approve_lands_in_approved_only() {
    with_governance(|gov| {
        let a = gov.submit_oip(AUTHOR, "a").unwrap();
        gov.set_oip_status(AUTH, a, status::REWORK).unwrap();
        gov.set_oip_status(AUTHOR, a, status::DRAFT).unwrap(); // author resubmit
        gov.set_oip_status(AUTH, a, status::APPROVED).unwrap();

        assert_eq!(all(gov, status::APPROVED), vec![a]);
        assert!(all(gov, status::DRAFT).is_empty());
        assert!(all(gov, status::REWORK).is_empty());
    });
}

#[test]
fn status_buckets_are_per_kind() {
    with_governance(|gov| {
        let o = gov.submit_oip(AUTHOR, "o").unwrap();
        let g = gov.submit_gip(AUTHOR, "g").unwrap();
        gov.set_oip_status(AUTH, o, status::APPROVED).unwrap();
        gov.set_gip_status(AUTH, g, status::REJECTED).unwrap();

        assert_eq!(gov.oip_count_by_status(status::APPROVED).unwrap(), 1);
        assert_eq!(gov.gip_count_by_status(status::APPROVED).unwrap(), 0);
        assert_eq!(gov.oip_count_by_status(status::REJECTED).unwrap(), 0);
        assert_eq!(gov.gip_count_by_status(status::REJECTED).unwrap(), 1);
    });
}

#[test]
fn out_of_range_status_query_errors() {
    with_governance(|gov| {
        assert!(gov.oip_count_by_status(5).is_err());
        assert!(gov.oips_by_status(9, U256::ZERO, U256::from(10)).is_err());
    });
}

// ------------------------------------------------------------ pagination ---

#[test]
fn author_listing_is_paginated() {
    with_governance(|gov| {
        for i in 0..5u32 {
            gov.submit_oip(AUTHOR, &format!("p{i}")).unwrap(); // ids 1..=5
        }
        assert_eq!(gov.oip_count_by_author(AUTHOR).unwrap(), 5);

        // first page of 2
        let p0 = gov
            .oips_by_author(AUTHOR, U256::ZERO, U256::from(2))
            .unwrap();
        assert_eq!(
            p0.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![U256::from(1), U256::from(2)]
        );

        // middle page
        let p1 = gov
            .oips_by_author(AUTHOR, U256::from(2), U256::from(2))
            .unwrap();
        assert_eq!(
            p1.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![U256::from(3), U256::from(4)]
        );

        // last partial page (only 1 left)
        let p2 = gov
            .oips_by_author(AUTHOR, U256::from(4), U256::from(2))
            .unwrap();
        assert_eq!(
            p2.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![U256::from(5)]
        );

        // offset past the end -> empty
        assert!(gov
            .oips_by_author(AUTHOR, U256::from(99), U256::from(2))
            .unwrap()
            .is_empty());
    });
}

#[test]
fn status_listing_is_paginated() {
    with_governance(|gov| {
        for _ in 0..4 {
            let id = gov.submit_oip(AUTHOR, "x").unwrap();
            gov.set_oip_status(AUTH, id, status::APPROVED).unwrap();
        }
        assert_eq!(gov.oip_count_by_status(status::APPROVED).unwrap(), 4);

        let page = gov
            .oips_by_status(status::APPROVED, U256::from(1), U256::from(2))
            .unwrap();
        assert_eq!(page.len(), 2); // items 2 and 3 of the bucket
        let last = gov
            .oips_by_status(status::APPROVED, U256::from(3), U256::from(10))
            .unwrap();
        assert_eq!(last.len(), 1); // clamped to what's left
    });
}

// ----------------------------------------------------------------- diff ---

#[test]
fn gip_diff_against_canon_shows_edits() {
    with_governance(|gov| {
        gov.update_canon(AUTH, "line a\nline b\nline c\n").unwrap();
        let id = gov.submit_gip(AUTHOR, "line a\nline B\nline c\n").unwrap();
        let g = gov.get_gip(id).unwrap().unwrap();
        let (canon, _, _) = gov.get_canon().unwrap();
        let d = crate::diff::unified(&canon, &g.text);
        assert!(d.contains("-line b"));
        assert!(d.contains("+line B"));
    });
}

mod vote_dispatch;
