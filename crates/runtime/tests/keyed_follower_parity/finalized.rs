//! Independent genesis allocation and production follower finalization readback.
use super::*;
use commonware_runtime::Runner as _;
use outbe_primitives::consensus::ConsensusOperatingMode;
use std::path::Path;

pub(super) const KEYLESS_CHILD_DIR: &str = "OUTBE_TEST_KEYED_PARITY_KEYLESS_CHILD_DIR";

// Witness policy is installed once per process, including on Reth worker threads.
#[derive(serde::Serialize, serde::Deserialize)]
struct KeylessCase {
    genesis: alloy_genesis::Genesis,
    genesis_hash: B256,
    blocks: Vec<Bytes>,
}

pub(super) fn replay_keyless_process(
    spec: &ChainSpec<OutbeHeader>,
    blocks: &[outbe_primitives::OutbeBlock],
    datadir: &Path,
    test_name: &str,
) -> Vec<HeightSeal> {
    use commonware_codec::Encode as _;
    use outbe_consensus::block::ConsensusBlock;
    use reth_primitives_traits::SealedBlock;

    let input = KeylessCase {
        genesis: spec.genesis.clone(),
        genesis_hash: spec.genesis_hash(),
        blocks: blocks
            .iter()
            .cloned()
            .map(|block| {
                Bytes::from(
                    ConsensusBlock::from_sealed(SealedBlock::seal_slow(block))
                        .encode()
                        .to_vec(),
                )
            })
            .collect(),
    };
    std::fs::create_dir(datadir).unwrap();
    std::fs::write(
        datadir.join("input.json"),
        serde_json::to_vec(&input).unwrap(),
    )
    .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture"])
        .env(KEYLESS_CHILD_DIR, datadir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "keyless follower failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    serde_json::from_slice(&std::fs::read(datadir.join("finalized.json")).unwrap()).unwrap()
}

pub(super) fn run_keyless_child(datadir: &Path) {
    let input: KeylessCase =
        serde_json::from_slice(&std::fs::read(datadir.join("input.json")).unwrap()).unwrap();
    outbe_chain_constants::initialize(None).unwrap();
    outbe_consensus::proof::init_consensus_chain_id(CHAIN_ID).unwrap();
    outbe_tee::witness::install_witness_policy(outbe_tee::witness::WitnessPolicy::ReplayOnly)
        .unwrap();
    assert!(!outbe_tee::is_enclave_configured());
    let spec = Arc::new(spec_from_genesis(input.genesis));
    assert_eq!(spec.genesis_hash(), input.genesis_hash);
    let blocks: Vec<_> = input
        .blocks
        .iter()
        .map(|encoded| {
            outbe_consensus::committee_chain::decode_public_block(encoded)
                .unwrap()
                .into_inner()
                .unseal()
        })
        .collect();
    let seals = replay(&spec, &blocks, &datadir.join("node"), ReplayRole::Keyless);
    std::fs::write(
        datadir.join("finalized.json"),
        serde_json::to_vec(&seals).unwrap(),
    )
    .unwrap();
}

pub(super) fn chain_spec(state: &ExecState) -> Arc<ChainSpec<OutbeHeader>> {
    use alloy_genesis::{Genesis, GenesisAccount};
    use outbe_primitives::tee_test_utils::{
        no_attestation_dev_policy_v1, tee_attestation_v1_extra_field,
    };

    let genesis = Genesis {
        timestamp: TEST_BLOCK_TIMESTAMP_BASE,
        gas_limit: outbe_primitives::system_tx::protocol_block_gas_limit(0),
        base_fee_per_gas: Some(u128::from(MIN_PROTOCOL_BASE_FEE)),
        alloc: complete_accounts(state)
            .filter_map(|(address, account)| {
                let info = account.info()?;
                let storage = account
                    .storage
                    .into_iter()
                    .filter(|(_, value)| !value.is_zero())
                    .map(|(slot, value)| (B256::from(slot), B256::from(value)))
                    .collect::<std::collections::BTreeMap<_, _>>();
                if info.is_empty() && storage.is_empty() {
                    return None;
                }
                let code = info
                    .code
                    .or_else(|| state.database.cache.contracts.get(&info.code_hash).cloned());
                Some((
                    address,
                    GenesisAccount {
                        nonce: Some(info.nonce),
                        balance: info.balance,
                        code: code.map(|code| code.original_bytes()),
                        storage: Some(storage),
                        ..Default::default()
                    },
                ))
            })
            .collect(),
        ..Default::default()
    };
    let mut spec = spec_from_genesis(genesis);
    let policy = no_attestation_dev_policy_v1(CHAIN_ID, spec.genesis_hash()).unwrap();
    spec.genesis.config.chain_id = CHAIN_ID;
    spec.genesis.config.extra_fields.insert(
        "teeAttestationV1".into(),
        tee_attestation_v1_extra_field(&policy).unwrap(),
    );
    assert_eq!(spec.genesis_header().inner.state_root, state_root(state));
    Arc::new(spec)
}

fn spec_from_genesis(genesis: alloy_genesis::Genesis) -> ChainSpec<OutbeHeader> {
    reth_chainspec::ChainSpecBuilder::default()
        .chain(CHAIN_ID.into())
        .genesis(genesis)
        .shanghai_activated()
        .build()
        .map_header(OutbeHeader::new)
}

pub(super) enum ReplayRole<'a> {
    CommitteeVerifier,
    Keyless,
    Keyed(&'a Arc<outbe_tee::in_process::InProcessTeeContext>),
}

pub(super) fn replay(
    spec: &Arc<ChainSpec<OutbeHeader>>,
    blocks: &[outbe_primitives::OutbeBlock],
    datadir: &Path,
    role: ReplayRole<'_>,
) -> Vec<HeightSeal> {
    let (seed, keyed) = match role {
        ReplayRole::CommitteeVerifier => (Some(9), None),
        ReplayRole::Keyless => (None, None),
        ReplayRole::Keyed(context) => (Some(8), Some(context)),
    };
    let signer = seed.map(|seed| Arc::new(OutbeEvmSigner::from_secret_bytes([seed; 32]).unwrap()));
    let config =
        commonware_runtime::tokio::Config::new().with_storage_directory(datadir.join("marshal"));
    commonware_runtime::tokio::Runner::new(config).start(|ctx| async move {
        let node = if matches!(role, ReplayRole::CommitteeVerifier) {
            follower_import::launch_profile(
                Arc::clone(spec),
                datadir,
                (ConsensusOperatingMode::Verifier, signer.clone()),
                |_| Ok(()),
            )
            .await
        } else {
            follower_import::launch_with_seed(Arc::clone(spec), datadir, signer.clone(), |_| Ok(()))
                .await
        }
        .expect("launch real Outbe execution node");
        let provider = node.provider.clone();
        let genesis = spec.sealed_genesis_header();
        let mut follower = follower_import::FollowerImport::start(
            ctx,
            node,
            outbe_primitives::OutbeBlock {
                header: genesis.header().clone(),
                body: outbe_primitives::OutbeBlockBody {
                    withdrawals: Some(Default::default()),
                    ..Default::default()
                },
            },
            &genesis,
        )
        .await;
        let owner = keyed.zip(signer.as_deref()).map(|(resident, signer)| {
            keyed_owner(resident, signer, blocks[0].header.inner.hash_slow())
        });
        let protocol = outbe_runtime::OutbeRuntime::new();
        let chain = outbe_execution::tee_remote_session::RegistryChainIdentity {
            chain_id: CHAIN_ID,
            genesis_hash: spec.genesis_hash(),
        };
        let mut seals = Vec::new();
        for block in blocks {
            let number = block.header.inner.number;
            let expected_hash = block.header.inner.hash_slow();
            follower.finalize(block.clone()).await;
            let header =
                follower_import::assert_finalized(&provider, &provider, number, expected_hash);
            if let Some((identity, owner)) = &owner {
                assert_eq!(
                    owner.refresh(&provider, &protocol, &chain).unwrap(),
                    outbe_tee::TeeAuthorityState::Armed {
                        enclave_id: identity.expected_enclave_id.unwrap(),
                        valid_until: u64::MAX,
                    },
                );
                assert_eq!(owner.identity().unwrap(), *identity);
            }
            seals.push(HeightSeal {
                hash: header.hash(),
                state_root: header.header().inner.state_root,
                extra: header.header().inner.extra_data.clone(),
            });
        }
        seals
    })
}

fn keyed_owner(
    context: &Arc<outbe_tee::in_process::InProcessTeeContext>,
    signer: &OutbeEvmSigner,
    anchor: B256,
) -> (
    outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
    outbe_runtime::LifecycleAuthorityProbe,
) {
    use clap::Parser as _;
    use outbe_consensus::{args::ConsensusArgs, validators::LocalTeeRuntimeIdentityV1};
    #[derive(clap::Parser)]
    struct Launch {
        #[command(flatten)]
        consensus: ConsensusArgs,
    }

    let args = Launch::try_parse_from([
        "follower",
        "--upstream",
        "127.0.0.1:8545",
        "--validator.evm-key",
        "operator.hex",
    ])
    .unwrap()
    .consensus;
    assert!(!args.is_validator);
    let base = LocalTeeRuntimeIdentityV1 {
        validator: Some(signer.address()),
        reth_p2p_public: context.identity().reth_p2p_public,
        expected_enclave_id: Some(context.manifest().enclave_id().unwrap()),
    };
    let (mode, keyless, identity) =
        outbe_runtime::tee_launch_profile_for_test(&args, Some(signer), base);
    assert_eq!(mode, ConsensusOperatingMode::Follower);
    assert!(!keyless);
    assert!(identity.validator.is_none());
    let owner = outbe_runtime::LifecycleAuthorityProbe::new(
        Arc::clone(context),
        identity,
        (FIRST_BLOCK, anchor),
        outbe_tee::TeeAuthorityChannel::default(),
    )
    .unwrap();
    (identity, owner)
}

fn complete_accounts(
    state: &ExecState,
) -> impl Iterator<Item = (Address, revm::database::DbAccount)> {
    let mut post = state.database.cache.accounts.clone();
    for (&address, changes) in state.bundle_state.state() {
        let Some(info) = &changes.info else {
            post.insert(address, revm::database::DbAccount::new_not_existing());
            continue;
        };
        let account = post.entry(address).or_default();
        if changes.status.was_destroyed() {
            account.storage.clear();
        }
        account.info = info.clone();
        // A cache miss can already exist as NotExisting before this block creates it.
        account.account_state = revm::database::AccountState::Touched;
        for (&slot, change) in &changes.storage {
            account.storage.insert(slot, change.present_value);
        }
    }
    post.into_iter()
}

pub(super) fn state_root(state: &ExecState) -> B256 {
    use alloy_trie::root::{state_root_unhashed, storage_root_unhashed};
    state_root_unhashed(complete_accounts(state).filter_map(|(address, account)| {
        let info = account.info()?;
        let storage = account.storage.into_iter().filter_map(|(slot, value)| {
            (!value.is_zero()).then_some((B256::from(slot.to_be_bytes::<32>()), value))
        });
        let storage_root = storage_root_unhashed(storage);
        if info.is_empty() && storage_root == alloy_trie::EMPTY_ROOT_HASH {
            return None;
        }
        Some((
            address,
            alloy_trie::TrieAccount {
                nonce: info.nonce,
                balance: info.balance,
                code_hash: info.code_hash,
                storage_root,
            },
        ))
    }))
}
