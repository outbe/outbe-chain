//! Genesis V2 bootstrap regressions.
//!
//! These tests pin three independent contracts for the V2 greenfield genesis:
//!
//! 1. `outbe-genesis-cli` writes `ACCOUNTING_PROGRESS_ADDRESS = 0xEE04`
//!    with the canonical `0xef` marker bytecode and `slot 0 = 0`.
//! 2. The genesis seeder must not write any `ValidatorSet` storage entry at
//!    the direct slots 31..40. Those slots are reserved for the runtime
//!    `CommitteeSnapshotStore`, whose first writer is block 1's
//!    `BoundaryOutcome` system transaction.
//! 3. The genesis JSON that the seeder produces must not embed private DKG
//!    share material in any path. Private keys live exclusively in the
//!    per-validator directories under the operator filesystem.
//!
//! Tests `T-3..T-9` live in [`mod runtime`]. They exercise the begin-zone
//! system-transaction layout contract (`outbe_evm::system_tx`) for the
//! genesis bootstrap:
//!
//! - block 0 has no begin-zone txs
//! - block 1 mandatorily carries `BoundaryOutcome`
//! - block 2+ requires `CertifiedParentAccounting` before any user transaction
//!   reads `last_accounted_block_number`.

use alloy_primitives::Address;
use outbe_evm::system_tx::{expected_begin_block_kinds, SystemTxKind};
use outbe_gem::GemContract;
use outbe_intex::IntexContract;
use outbe_primitives::addresses::{GEM_ADDRESS, INTEX_ADDRESS};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_validatorset::contract::ValidatorSet;
use serde_json::Value;
use tempfile::TempDir;

const ACCOUNTING_PROGRESS_ADDRESS_HEX: &str = "000000000000000000000000000000000000ee04";
const VALIDATOR_SET_ADDRESS_HEX: &str = "000000000000000000000000000000000000ee00";

/// `0xef` marker bytecode, mirroring `MARKER_CODE` in
/// `bin/outbe-genesis-cli/src/seed_codec.rs`
/// and the executor's marker-bytecode allowlist in
/// `crates/blockchain/evm/src/executor.rs`.
const MARKER_BYTECODE_HEX: &str = "0xef";

const ZERO_WORD_HEX: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";

const FIXTURE_GENESIS: &str = r#"{
  "config": { "chainId": 512215, "epochLengthBlocks": 120 },
  "timestamp": "0x6800000",
  "alloc": {}
}"#;

const FIXTURE_SEED: &str = "{}";

// 96-hex-char (48-byte) placeholder BLS MinPk public keys. The length matches
// the length check in `outbe-genesis-cli`'s `seed_codec::pubkey_bytes`.
const FIXTURE_VALIDATORS_4_PUBLIC_ONLY: &str = r#"[
  { "address": "0x1111111111111111111111111111111111111111",
    "public_key": "111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111",
    "radicle_node_id": "1111111111111111111111111111111111111111111111111111111111111111" },
  { "address": "0x2222222222222222222222222222222222222222",
    "public_key": "222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222",
    "radicle_node_id": "2222222222222222222222222222222222222222222222222222222222222222" },
  { "address": "0x3333333333333333333333333333333333333333",
    "public_key": "333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333",
    "radicle_node_id": "3333333333333333333333333333333333333333333333333333333333333333" },
  { "address": "0x4444444444444444444444444444444444444444",
    "public_key": "444444444444444444444444444444444444444444444444444444444444444444444444444444444444444444444444",
    "radicle_node_id": "4444444444444444444444444444444444444444444444444444444444444444" }
]"#;

/// Run `outbe-genesis-cli` seeder against the supplied fixtures inside a fresh
/// temp directory and return the parsed JSON plus the raw text. The function
/// keeps the text verbatim so DKG-share scans operate on the on-disk
/// representation, not the serde-roundtripped one.
fn run_seed_genesis(
    genesis_json: &str,
    seed_json: &str,
    validators_json: &str,
) -> (TempDir, Value, String) {
    let tmp = TempDir::new().expect("tempdir");
    let genesis_path = tmp.path().join("genesis.json");
    let seed_path = tmp.path().join("seed.json");
    let validators_path = tmp.path().join("validators.json");
    let out_path = tmp.path().join("out.json");
    std::fs::write(&genesis_path, genesis_json).expect("write genesis fixture");
    std::fs::write(&seed_path, seed_json).expect("write seed fixture");
    std::fs::write(&validators_path, validators_json).expect("write validators fixture");

    outbe_genesis_cli::create_genesis::run_seed_cli(
        &outbe_genesis_cli::create_genesis::SeedCliArgs {
            genesis: genesis_path,
            seed: seed_path,
            validators: Some(validators_path),
            output: out_path.clone(),
            worldwide_day: None,
            fresh_metadosis: false,
            contracts_dir: None,
            canon_dir: None,
        },
    )
    .expect("run_seed_cli");

    let raw = std::fs::read_to_string(&out_path).expect("read seeded genesis");
    let parsed: Value = serde_json::from_str(&raw).expect("seeded genesis is valid JSON");
    (tmp, parsed, raw)
}

fn alloc_entry<'a>(genesis: &'a Value, addr_hex: &str) -> &'a Value {
    let alloc = genesis
        .get("alloc")
        .and_then(Value::as_object)
        .expect("alloc object present in seeded genesis");
    alloc
        .get(addr_hex)
        .unwrap_or_else(|| panic!("alloc entry for 0x{addr_hex} missing from seeded genesis"))
}

fn storage_keys(entry: &Value) -> Vec<String> {
    entry
        .get("storage")
        .and_then(Value::as_object)
        .map(|m| m.keys().map(|s| s.to_lowercase()).collect())
        .unwrap_or_default()
}

/// The genesis seeder writes `ACCOUNTING_PROGRESS_ADDRESS` with
/// marker bytecode `0xef` and explicit `slot 0 = 0`.
#[test]
fn accounting_progress_address_seeded_with_marker_and_zero_slot0() {
    let (_tmp, genesis, _raw) = run_seed_genesis(
        FIXTURE_GENESIS,
        FIXTURE_SEED,
        FIXTURE_VALIDATORS_4_PUBLIC_ONLY,
    );
    let entry = alloc_entry(&genesis, ACCOUNTING_PROGRESS_ADDRESS_HEX);

    let code = entry
        .get("code")
        .and_then(Value::as_str)
        .expect("EE04 entry has `code` field");
    assert_eq!(
        code, MARKER_BYTECODE_HEX,
        "EE04 must carry the `{MARKER_BYTECODE_HEX}` marker bytecode so slot 0 \
         survives EIP-161 cleanup (executor allowlist guarantees no dispatch)"
    );

    let storage = entry
        .get("storage")
        .and_then(Value::as_object)
        .expect("EE04 entry has `storage` map");
    let slot0 = storage
        .get(ZERO_WORD_HEX)
        .and_then(Value::as_str)
        .expect("EE04 storage maps slot 0 to a value");
    assert_eq!(
        slot0, ZERO_WORD_HEX,
        "EE04 slot 0 (last_accounted_block_number) must be zero at genesis"
    );
}

/// `outbe-genesis-cli` does not write any direct-slot
/// storage entry at `ValidatorSet` slots 31..40. This matches the Rust schema in
/// `crates/system/validatorset/src/schema.rs`. In that schema, slots 31..39 are
/// mappings with no base-slot value. Slot 40 is a reserved `Slot<B256>` that
/// must be zero at genesis.
///
/// This protects the invariant that genesis carries no committee-snapshot
/// material. Block 1's `BoundaryOutcome` system tx is the first writer.
#[test]
fn seed_genesis_writes_committee_snapshot_slots_31_to_40_matching_rust_schema() {
    // 1. Rust-schema sanity: the slot indices on the contract facade must be
    //    exactly 31..40. If schema drift moves them, this test fails loudly.
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        let vs = ValidatorSet::new(storage);
        outbe_validatorset::test_support::assert_committee_snapshot_schema_slots(&vs);
    });

    // 2. Seeder-output check: for each slot in 31..=40, the corresponding
    //    direct slot key (`hex32(N)`) must be absent from the seeded
    //    `VALIDATOR_SET_ADDRESS` storage. Mappings (slots 31..39) write at
    //    keccak-derived keys and never the base slot itself. The reserved
    //    `Slot<B256>` at 40 must be untouched.
    let (_tmp, genesis, _raw) = run_seed_genesis(
        FIXTURE_GENESIS,
        FIXTURE_SEED,
        FIXTURE_VALIDATORS_4_PUBLIC_ONLY,
    );
    let entry = alloc_entry(&genesis, VALIDATOR_SET_ADDRESS_HEX);
    let keys = storage_keys(entry);
    for slot in 31u64..=40 {
        let hex = format!("0x{:064x}", slot);
        assert!(
            !keys.iter().any(|k| k == &hex),
            "outbe-genesis-cli wrote direct-slot entry at ValidatorSet slot {slot} \
             ({hex}); slots 31..40 are reserved for runtime CommitteeSnapshotStore"
        );
    }
}

/// Cross-checks all sources of the EIP-161 preservation contract. Every
/// stateful dispatch-registered precompile must survive state-root computation
/// through either the executor's runtime `0xEF` marker list or genesis `0xEF`
/// bytecode seeded by `outbe-genesis-cli`. This binds route registration,
/// runtime preservation, and genesis seeding so an uncovered precompile fails
/// loudly instead of losing its storage.
///
/// This test skips the stateless verifier: they own no EVM storage to
/// preserve, matching the `MARKER_EXEMPT` rationale in the executor's
/// `marker_list_covers_stateful_precompiles` unit test.
#[test]
fn every_stateful_precompile_preserved_by_marker_or_genesis() {
    use outbe_evm::executor::marker_addresses::OUTBE_RUNTIME_MARKER_ADDRESSES;
    use outbe_primitives::addresses::ZKPROOF_VERIFY_ADDRESS;

    let (_tmp, genesis, _raw) = run_seed_genesis(
        FIXTURE_GENESIS,
        FIXTURE_SEED,
        FIXTURE_VALIDATORS_4_PUBLIC_ONLY,
    );

    // These routes own no storage, so neither runtime markers nor genesis code
    // need to cover them.
    let storage_free = [ZKPROOF_VERIFY_ADDRESS];

    let mut checked = 0usize;
    for addr in outbe_evm::precompiles::outbe_precompile_addresses() {
        if storage_free.contains(addr) {
            continue;
        }
        checked += 1;

        let in_marker = OUTBE_RUNTIME_MARKER_ADDRESSES.contains(addr);

        // Match the alloc key format used by `alloc_entry` / `outbe-genesis-cli`:
        // lowercase 40-hex chars, NO `0x` prefix (`address_bytes(addr).hex()`).
        let hex = format!("{:x}", addr);
        let in_genesis = genesis["alloc"][&hex]["code"].as_str() == Some(MARKER_BYTECODE_HEX);

        assert!(
            in_marker || in_genesis,
            "stateful precompile {addr} is EIP-161-preserved by neither the executor marker \
             list nor genesis 0xEF bytecode - its storage would be pruned at state-root"
        );
    }

    assert!(
        checked > 0,
        "no stateful precompiles were checked - the iteration covered nothing, \
         which would make this test vacuously pass"
    );
}

/// `ZEROFEE_ADDRESS` is exempt from the runtime marker list because genesis is
/// responsible for preserving it. This test verifies that the seeder actually
/// writes `0xEF` code at that address. Otherwise the exemption would silently
/// discard its storage.
#[test]
fn zerofee_precompile_is_genesis_seeded_with_marker_bytecode() {
    use outbe_primitives::addresses::ZEROFEE_ADDRESS;

    let (_tmp, genesis, _raw) = run_seed_genesis(
        FIXTURE_GENESIS,
        FIXTURE_SEED,
        FIXTURE_VALIDATORS_4_PUBLIC_ONLY,
    );

    let hex = format!("{:x}", ZEROFEE_ADDRESS);
    let entry = alloc_entry(&genesis, &hex);
    let code = entry
        .get("code")
        .and_then(Value::as_str)
        .expect("ZEROFEE_ADDRESS entry has `code` field in seeded genesis");
    assert_eq!(
        code, MARKER_BYTECODE_HEX,
        "ZEROFEE_ADDRESS must carry `{MARKER_BYTECODE_HEX}` genesis bytecode - it is the \
         marker-list exemption in marker_list_covers_stateful_precompiles, so its EIP-161 \
         preservation depends entirely on this genesis seed"
    );
}

/// the seeded `genesis.json` must not contain any
/// substring that would identify a private DKG share, polynomial scalar, or
/// dealer secret. Public keys (BLS MinPk pubkeys, hex-encoded) and validator
/// EVM addresses are permitted because they are explicitly public.
#[test]
fn genesis_json_does_not_contain_private_dkg_shares() {
    let (_tmp, _genesis, raw) = run_seed_genesis(
        FIXTURE_GENESIS,
        FIXTURE_SEED,
        FIXTURE_VALIDATORS_4_PUBLIC_ONLY,
    );

    let haystack = raw.to_ascii_lowercase();
    let forbidden_substrings: &[&str] = &[
        "share",
        "signing_share",
        "private_key",
        "private-key",
        "privatekey",
        "secret",
        "dkg_share",
        "dealer_secret",
        "polynomial_secret",
        "evm_key",
        "consensus_share",
        "signing-key",
    ];
    for needle in forbidden_substrings {
        assert!(
            !haystack.contains(needle),
            "seeded genesis.json must not contain `{needle}` - DKG / private \
             key material is the runtime's responsibility, not genesis"
        );
    }
}

// ---------------------------------------------------------------------------
// Runtime begin-zone ordering invariants.
//
// These tests pin the V2 system-transaction layout contract surfaced through
// `outbe_evm::system_tx::expected_begin_block_kinds`. The contract is the
// single source of truth for which kinds appear in which order at each
// block height. The executor and the block builder both consume it. Pinning
// it here keeps the genesis-bootstrap requirements (block 1 BoundaryOutcome
// before block 2 CertifiedParentAccounting) visible alongside the genesis
// seeder regressions above.
// ---------------------------------------------------------------------------

// T-3 / used to live here as a source-text grep of
// `engine/src/stack.rs::validate_recovered_vrf_material`. Source-grep tests
// drift the moment anyone renames a local, so the behavioural assertion
// belongs next to the function. `outbe-engine`'s stack tests
// (`stack::tests::*_dkg_*` family) and the localnet restart smoke harness
// exercise the DKG-share / VRF-group-key mismatch rejection end-to-end.

/// T-6 / block 1 executes `BoundaryOutcome` and persists the epoch-0
/// `CommitteeSnapshot` in `CommitteeSnapshotStore` before block 2's Phase 1
/// (`CertifiedParentAccounting`) reads it.
#[test]
fn block1_boundary_outcome_writes_epoch0_snapshot_before_block2() {
    use alloy_primitives::{keccak256, Bytes, B256, U256};
    use outbe_evm::{begin_block_precompile::dispatch, system_tx::SystemTxInputV2};
    use outbe_primitives::{
        addresses::SYSTEM_ADDRESS,
        consensus::{DkgBoundaryArtifact, ReshareResult},
    };

    let validator = Address::repeat_byte(0x11);
    let owner = Address::repeat_byte(0xAA);
    let consensus_pubkey = [7u8; 48];
    let vrf_group_public_key_bytes = vec![0x42u8; 96];
    let expected_snapshot = outbe_validatorset::CommitteeSnapshot {
        committee: vec![outbe_validatorset::CommitteeEntry {
            address: validator,
            consensus_pubkey,
        }],
        vrf_material_version: 0,
        vrf_group_public_key_bytes: vrf_group_public_key_bytes.clone(),
        vrf_public_polynomial_hash: B256::ZERO,
    };
    let committee_set_hash = outbe_validatorset::committee_set_hash_v2(0, &expected_snapshot);
    let snapshot_key = outbe_validatorset::committee_snapshot_key(0, committee_set_hash);

    let mut active_set_bytes = Vec::with_capacity(28);
    active_set_bytes.extend_from_slice(&1u64.to_be_bytes());
    active_set_bytes.extend_from_slice(validator.as_slice());
    let active_set_hash = keccak256(active_set_bytes);

    let mut provider = HashMapStorageProvider::new(512_215);
    provider.set_block_number(1);
    provider.set_timestamp(U256::from(1_700_000_001u64));
    provider.set_beneficiary(validator);
    StorageHandle::enter(&mut provider, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(owner).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_epoch_length_blocks.write(60).unwrap();
        let minimum = U256::from(1_000u64);
        vs.test_seed_authoritative_min_stake(minimum).unwrap();
        vs.register_validator(owner, validator, &consensus_pubkey)
            .unwrap();
        vs.test_seed_authoritative_bonded(validator, minimum)
            .unwrap();
        vs.mark_pending(validator).unwrap();
        vs.confirm_validator_ready(validator).unwrap();
        vs.activate_validator_via_boundary_for_test(validator)
            .unwrap();
        assert!(
            outbe_validatorset::read_committee_snapshot(storage.clone(), snapshot_key)
                .unwrap()
                .is_none(),
            "epoch-0 snapshot must not exist before block 1 BoundaryOutcome runs"
        );

        let artifact = DkgBoundaryArtifact {
            epoch: 0,
            dkg_cycle: 0,
            freeze_height: 0,
            planned_activation_height: 0,
            target_set_hash: active_set_hash,
            vrf_material_version: 0,
            vrf_group_public_key: keccak256(&vrf_group_public_key_bytes),
            vrf_group_public_key_bytes: Bytes::from(vrf_group_public_key_bytes.clone()),
            committee_set_hash,
            is_validator_set_change: false,
            outcome: Bytes::new(),
            is_full_dkg: true,
            tee_expired_target_exclusions: Vec::new(),
            tee_expired_target_exclusions_hash: B256::ZERO,
            reshare: ReshareResult {
                new_active_set: vec![validator],
                active_set_hash,
            },
        };
        let input = SystemTxInputV2::BoundaryOutcome { artifact }
            .encode()
            .unwrap();
        dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO)
            .expect("block 1 genesis DKG BoundaryOutcome must commit epoch-0 snapshot");
    });

    // Advance to block 2 and verify the persisted epoch-0 snapshot is readable
    // at the exact key that block 2's CertifiedParentAccounting preflight queries.
    provider.set_block_number(2);
    provider.set_timestamp(U256::from(1_700_000_002u64));
    StorageHandle::enter(&mut provider, |storage| {
        let persisted = outbe_validatorset::read_committee_snapshot(storage, snapshot_key)
            .unwrap()
            .expect("epoch-0 committee snapshot written at block 1 must be readable at block 2");
        assert_eq!(persisted, expected_snapshot);
    });
}

/// T-7 / Begin-zone layout contract for block 1 (genesis DKG `BoundaryOutcome`,
/// no `CertifiedParentAccounting`) vs block 2 (`CertifiedParentAccounting` first).
#[test]
fn runtime_genesis_dkg_boundary_seeds_epoch0_vrf_snapshot_before_block2() {
    let block1 = expected_begin_block_kinds(1, true, false);
    let expected_block1 = vec![
        SystemTxKind::CycleTick,
        SystemTxKind::BoundaryOutcome,
        SystemTxKind::OracleSlashWindow,
        SystemTxKind::HookEvents,
    ];
    assert_eq!(
        block1, expected_block1,
        "block 1 V2 layout pins to [CycleTick, \
         BoundaryOutcome, OracleSlashWindow, HookEvents]; BoundaryOutcome is \
         the genesis DKG VRF snapshot writer and CertifiedParentAccounting is absent"
    );

    let block2 = expected_begin_block_kinds(2, false, false);
    assert_eq!(
        block2.first(),
        Some(&SystemTxKind::CertifiedParentAccounting),
        "block 2's first begin-zone tx must be CertifiedParentAccounting, the \
         first reader of the snapshot seeded at block 1"
    );
}

/// T-8 / block N >= 2 mandatorily carries
/// `CertifiedParentAccounting` as its first begin-zone tx. This is the V2
/// replacement for the legacy exact-parent finalization invariant per Epic
#[test]
fn block2_requires_v2_parent_accounting() {
    let block2_without_boundary = expected_begin_block_kinds(2, false, false);
    let block2_with_boundary = expected_begin_block_kinds(2, true, false);
    assert_eq!(
        block2_without_boundary.first(),
        Some(&SystemTxKind::CertifiedParentAccounting)
    );
    assert_eq!(
        block2_with_boundary.first(),
        Some(&SystemTxKind::CertifiedParentAccounting)
    );

    // Higher blocks (e.g. 100) keep the same requirement. The V2 contract is
    // not localized to block 2.
    let block100 = expected_begin_block_kinds(100, false, false);
    assert_eq!(
        block100.first(),
        Some(&SystemTxKind::CertifiedParentAccounting),
        "every block N >= 2 must run CertifiedParentAccounting first"
    );
}

/// T-9 / blocks 0 and 1 must not include `CertifiedParentAccounting`.
/// Block 0 is genesis (no parent). Block 1's parent is genesis (no Phase 1
/// state to import). The ignored builder test
/// `locally_built_genesis_block_reexecutes_with_same_state_root` exists
/// because of this exact invariant.
#[test]
fn block0_and_block1_do_not_require_parent_accounting() {
    let block0_no_boundary = expected_begin_block_kinds(0, false, false);
    let block0_with_boundary = expected_begin_block_kinds(0, true, false);
    let block1_with_boundary = expected_begin_block_kinds(1, true, false);

    assert!(
        block0_no_boundary.is_empty(),
        "block 0 must have no begin-zone txs; got {block0_no_boundary:?}"
    );
    assert!(
        block0_with_boundary.is_empty(),
        "block 0 with header artifact still has no begin-zone txs (Phase 3 \
         requires block_number > 0); got {block0_with_boundary:?}"
    );
    assert!(
        !block1_with_boundary.contains(&SystemTxKind::CertifiedParentAccounting),
        "block 1 must not run CertifiedParentAccounting; the V2 contract \
         starts Phase 1 at block N >= 2"
    );
}

fn load_seeded_storage(provider: &mut HashMapStorageProvider, genesis: &Value, address: Address) {
    let entry = alloc_entry(genesis, &alloy_primitives::hex::encode(address));
    for (key, value) in entry["storage"].as_object().expect("seeded storage") {
        let value = value.as_str().expect("storage value is a hex string");
        provider.storage.insert(
            (address, key.parse().expect("storage key")),
            value.parse().expect("storage value"),
        );
    }
}

/// The seeder writes raw slots. This test reads them back through the schemas that own
/// them. Genesis does not seed Gem bodies: the Gem account carries only its profile
/// and an empty supply.
#[test]
fn seeded_profiles_and_gems_read_back_through_their_schemas() {
    const SEED: &str = r#"{
      "intex_profile": { "profile": "dev" },
      "gem_profile": { "profile": "prod" },
      "nod_profile": { "profile": "prod" },
      "credis_profile": { "profile": "dev" }
    }"#;
    let (_tmp, genesis, _raw) =
        run_seed_genesis(FIXTURE_GENESIS, SEED, FIXTURE_VALIDATORS_4_PUBLIC_ONLY);
    let mut provider = HashMapStorageProvider::new(512_215);
    load_seeded_storage(&mut provider, &genesis, GEM_ADDRESS);
    load_seeded_storage(&mut provider, &genesis, INTEX_ADDRESS);
    load_seeded_storage(
        &mut provider,
        &genesis,
        outbe_primitives::addresses::NOD_ADDRESS,
    );
    load_seeded_storage(
        &mut provider,
        &genesis,
        outbe_primitives::addresses::CREDIS_ADDRESS,
    );

    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            outbe_credis::CredisContract::new(storage.clone())
                .config_profile
                .read()
                .unwrap(),
            outbe_credis::config::PROFILE_DEV
        );
        assert_eq!(
            IntexContract::new(storage.clone())
                .config_profile
                .read()
                .unwrap(),
            outbe_intex::config::PROFILE_DEV
        );
        assert_eq!(
            outbe_nod::NodContract::new(storage.clone())
                .config_profile
                .read()
                .unwrap(),
            outbe_nod::config::PROFILE_PROD
        );
        let gem = GemContract::new(storage);
        assert_eq!(
            gem.config_profile.read().unwrap(),
            outbe_gem::config::PROFILE_PROD
        );
        assert_eq!(
            gem.total_supply().unwrap(),
            0,
            "genesis carries no Gem bodies"
        );
    });
}
