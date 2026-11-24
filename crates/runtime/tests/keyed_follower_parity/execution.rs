use super::*;

pub(super) fn produce(prepared: &Prepared) -> Produced {
    let mut state = seeded_state(prepared);
    let mut parent = prepared.spec.sealed_genesis_header();
    let mut produced = Produced {
        seals: Vec::new(),
        blocks: Vec::new(),
        parents: Vec::new(),
    };
    for number in FIRST_BLOCK..=HEIGHTS + 1 {
        let input = HeightInput::new(number, &parent, prepared);
        let begin = input.begin(prepared);
        let users = &prepared.user_txs[(number - FIRST_BLOCK) as usize];
        let (block, seal) = execute_height(prepared, &mut state, input, (&begin, users));
        produced.parents.push(parent.hash());
        parent = SealedHeader::seal_slow(block.header.clone());
        assert_eq!(parent.hash(), seal.hash);
        assert_eq!(parent.header().inner.state_root, seal.state_root);
        produced.seals.push(seal);
        produced.blocks.push(block);
    }
    produced
}

struct HeightInput {
    number: u64,
    parent: B256,
    parent_artifact: Option<AccountedParentArtifact>,
    metadata: Option<CertifiedParentAccountingMetadata>,
    randao: B256,
    bootstrap: Option<super::bootstrap::Bootstrap>,
}

impl HeightInput {
    fn new(number: u64, parent: &SealedHeader<OutbeHeader>, prepared: &Prepared) -> Self {
        let (metadata, randao) = if number == 1 {
            (None, B256::ZERO)
        } else {
            let parent_height = number - 1;
            let hash = parent.hash();
            let (metadata, randao) = committee_finalization(prepared, parent_height, hash);
            (Some(metadata), randao)
        };
        let parent_artifact = (number > 1).then(|| {
            let summary = decode_outbe_block_artifacts(&parent.header().inner.extra_data)
                .unwrap()
                .execution_summary
                .expect("parent execution summary");
            AccountedParentArtifact {
                summary,
                timestamp: parent.header().inner.timestamp,
                state_root: Some(parent.header().inner.state_root),
            }
        });
        let bootstrap = (number == 1).then(|| super::bootstrap::Bootstrap::new(prepared));
        Self {
            number,
            parent: parent.hash(),
            parent_artifact,
            metadata,
            randao,
            bootstrap,
        }
    }

    fn begin(&self, prepared: &Prepared) -> Vec<Signed> {
        begin_system_txs_for_test(
            &prepared.config,
            BeginBlockFixture {
                block_number: self.number,
                parent_hash: self.parent,
                extra_data: &self
                    .bootstrap
                    .as_ref()
                    .map(|bootstrap| bootstrap.extra.clone())
                    .unwrap_or_default(),
                parent_consensus_metadata: self.metadata.clone(),
                proposer: prepared.proposer,
                bootstrap: self
                    .bootstrap
                    .as_ref()
                    .map(|bootstrap| BootstrapFixture::Payload(Box::new(bootstrap.payload.clone())))
                    .unwrap_or(BootstrapFixture::Absent),
            },
        )
    }
}

fn execute_height(
    prepared: &Prepared,
    state: &mut ExecState,
    input: HeightInput,
    txs: (&[Signed], &[Signed]),
) -> (outbe_primitives::OutbeBlock, HeightSeal) {
    let (begin, users) = txs;
    let number = input.number;
    let mut env = test_evm_env(number, REWARDS_ADDRESS);
    env.block.prevrandao = U256::from_be_bytes(input.randao.0);
    let evm = prepared.config.evm_with_env(&mut *state, env);
    let extra = input
        .bootstrap
        .as_ref()
        .map(|bootstrap| bootstrap.extra.clone())
        .unwrap_or_default();
    let count = Some(begin.len() + users.len());
    let mut ctx = if number == 1 {
        block_one_execution_ctx(count, extra)
    } else {
        block_two_execution_ctx(count, extra, prepared.proposer, input.parent).0
    };
    if let Some(bootstrap) = input.bootstrap {
        ctx.pending_tee_bootstrap = Some(bootstrap.payload);
    }
    ctx.inner.parent_hash = input.parent;
    ctx.inner.withdrawals = Some(std::borrow::Cow::Borrowed(&[]));
    ctx.parent_consensus_metadata = input.metadata;
    ctx.parent_artifact_hint = input.parent_artifact;
    let mut executor = prepared.config.create_executor(evm, ctx);
    executor.apply_pre_execution_changes().unwrap();
    for tx in begin.iter().chain(users).cloned() {
        executor
            .execute_transaction(tx)
            .unwrap_or_else(|error| panic!("tx at {number}: {error}"));
    }
    executor.apply_end_block_phase().unwrap();
    fixtures_execution::finalize_executor_header_artifacts(&mut executor, 0).unwrap();
    let end: Vec<_> = executor
        .take_staged_end_block_txs()
        .into_iter()
        .map(Recovered::into_inner)
        .collect();
    let extra = executor.final_extra_data().clone();
    let output = executor.finish().unwrap();
    state.commit_source(&BundleSource(&output.state));
    let result = output.result;
    let receipts = result.receipts;
    assert!(
        receipts
            .iter()
            .skip(begin.len())
            .take(users.len())
            .all(|receipt| receipt.status()),
        "user tx reverted at {number}"
    );
    if !users.is_empty() {
        assert_five_domain_witness(&extra, begin.len(), number);
    }
    seal_height(
        Header {
            number,
            parent_hash: input.parent,
            state_root: finalized::state_root(state),
            gas_used: result.gas_used,
            extra_data: extra,
            mix_hash: input.randao,
            ..Default::default()
        },
        begin
            .iter()
            .chain(users)
            .map(|tx| tx.inner().clone())
            .chain(end)
            .collect(),
        &receipts,
    )
}

fn seal_height(
    mut header: Header,
    transactions: Vec<TransactionSigned>,
    receipts: &[Receipt],
) -> (outbe_primitives::OutbeBlock, HeightSeal) {
    let blooms: Vec<_> = receipts
        .iter()
        .map(|receipt| receipt.with_bloom_ref())
        .collect();
    header.transactions_root = alloy_consensus::proofs::calculate_transaction_root(&transactions);
    header.receipts_root = alloy_consensus::proofs::calculate_receipt_root(&blooms);
    header.withdrawals_root = Some(alloy_trie::EMPTY_ROOT_HASH);
    header.logs_bloom = logs_bloom(receipts.iter().flat_map(|receipt| receipt.logs.iter()));
    header.gas_limit = outbe_primitives::system_tx::protocol_block_gas_limit(header.number);
    header.timestamp = TEST_BLOCK_TIMESTAMP_BASE + header.number;
    header.base_fee_per_gas = Some(MIN_PROTOCOL_BASE_FEE);
    header.beneficiary = REWARDS_ADDRESS;
    let seal = HeightSeal {
        hash: header.hash_slow(),
        state_root: header.state_root,
        extra: header.extra_data.clone(),
    };
    let block = outbe_primitives::OutbeBlock {
        header: OutbeHeader::new(header),
        body: outbe_primitives::OutbeBlockBody {
            transactions,
            withdrawals: Some(Default::default()),
            ..Default::default()
        },
    };
    (block, seal)
}

pub(super) fn committee_snapshot(prepared: &Prepared) -> outbe_validatorset::CommitteeSnapshot {
    let addresses = super::bootstrap::VALIDATOR_SEEDS.map(|seed| {
        OutbeEvmSigner::from_secret_bytes([seed; 32])
            .unwrap()
            .address()
    });
    prepared.committee.snapshot(addresses)
}

pub(super) fn committee_finalization(
    prepared: &Prepared,
    height: u64,
    hash: B256,
) -> (CertifiedParentAccountingMetadata, B256) {
    let snapshot = committee_snapshot(prepared);
    let proof = prepared.committee.certify(hash.into(), height);
    let metadata = CertifiedParentAccountingMetadata {
        finalized_block_number: height,
        finalized_block_hash: hash,
        finalized_epoch: 0,
        finalized_view: height + 1,
        parent_view: height,
        ordered_committee: snapshot
            .committee
            .iter()
            .map(|entry| entry.address)
            .collect(),
        signer_bitmap: vec![1; snapshot.committee.len()],
        committee_set_hash: snapshot.committee_set_hash_v2(0),
        vrf_material_version: snapshot.vrf_material_version,
        vrf_group_public_key_hash: keccak256(&snapshot.vrf_group_public_key_bytes),
        proof: proof.into(),
        ..Default::default()
    };
    let verified =
        outbe_consensus::proof::verify_v2_proof(&metadata, &snapshot, &metadata.proof, hash)
            .unwrap();
    (metadata, verified.prev_randao)
}

pub(super) fn assert_linked(produced: &Produced) {
    assert_eq!(produced.seals.len(), (HEIGHTS + 1) as usize);
    for (entry, (last, next)) in produced.parents[1..]
        .iter()
        .zip(produced.seals.iter().zip(&produced.seals[1..]))
    {
        assert_eq!(*entry, last.hash);
        assert_ne!(last.state_root, next.state_root);
    }
}
