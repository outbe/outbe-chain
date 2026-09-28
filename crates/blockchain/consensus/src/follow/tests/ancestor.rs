use super::*;

#[test]
fn child_accounting_recovers_only_exact_parent_finalization() {
    use alloy_consensus::SignableTransaction as _;
    use outbe_primitives::{
        consensus_metadata::{CertifiedParentAccountingMetadata, ParentParticipationProof},
        system_tx::{build_unsigned_system_tx, SystemTxInputV2, SystemTxKind},
    };
    use reth_ethereum::{primitives::SealedBlock, Block};
    let signer = committee(150);
    let parent = certified_block(&signer, Epoch::new(0), 43, Vec::new());
    let metadata = CertifiedParentAccountingMetadata {
        finalized_block_number: 43,
        finalized_block_hash: parent.block.block_hash(),
        finalized_epoch: 0,
        proof: parent.finalization.encode().to_vec().into(),
        ..Default::default()
    };
    let child = |metadata: CertifiedParentAccountingMetadata| {
        let data = SystemTxInputV2::CertifiedParentAccounting { metadata }
            .encode()
            .unwrap();
        let transaction =
            build_unsigned_system_tx(SystemTxKind::CertifiedParentAccounting, 0, 44, 1, data)
                .unwrap()
                .into_signed(alloy_primitives::Signature::test_signature())
                .into();
        let mut raw = Block::default();
        raw.header.number = 44;
        raw.header.parent_hash = parent.block.block_hash();
        raw.body.transactions.push(transaction);
        crate::block::ConsensusBlock::from_sealed(SealedBlock::seal_slow(
            raw.map_header(outbe_primitives::OutbeHeader::new),
        ))
    };
    let recovered =
        upstream::parent_finalization_from_child(&parent.block, &child(metadata.clone())).unwrap();
    assert_eq!(recovered.proposal, parent.finalization.proposal);
    let mut notarized = metadata.clone();
    notarized.proof_kind = ParentParticipationProof::CertifiedNotarization;
    assert!(upstream::parent_finalization_from_child(&parent.block, &child(notarized)).is_none());
    let mut wrong = metadata;
    wrong.finalized_block_hash = B256::ZERO;
    assert!(upstream::parent_finalization_from_child(&parent.block, &child(wrong)).is_none());
}

#[derive(Clone)]
struct GappedFinalitySource {
    records: Arc<BTreeMap<u64, CertifiedFinalizedBlock>>,
}

impl FinalizedSource for GappedFinalitySource {
    async fn get_finalization(&self, height: Height) -> Option<CertifiedFinalizedBlock> {
        if [2, 4].contains(&height.get()) {
            None
        } else {
            self.records.get(&height.get()).cloned()
        }
    }
    async fn get_block(&self, height: Height) -> Option<crate::block::ConsensusBlock> {
        self.records
            .get(&height.get())
            .map(|value| value.block.clone())
    }
    async fn get_finality_proof(&self, height: Height) -> Option<upstream::AncestorFinalityProof> {
        let indirect = [2, 4].contains(&height.get());
        Some(upstream::AncestorFinalityProof {
            certified: self
                .records
                .get(&(height.get() + u64::from(indirect)))?
                .clone(),
            ancestors: if indirect {
                vec![self.records.get(&height.get())?.block.clone()]
            } else {
                Vec::new()
            },
        })
    }
}

#[test]
fn missing_transition_and_boundary_certificates_survive_restart_via_ancestry() {
    use reth_ethereum::{primitives::SealedBlock, Block};
    let c0 = committee(110);
    let c1 = committee(130);
    let e0 = Epoch::new(0);
    let e1 = Epoch::new(1);
    let mut records = BTreeMap::new();
    let mut parent = B256::ZERO;
    for height in 1..=6 {
        let signer = if height <= 3 { &c0 } else { &c1 };
        let epoch = if height <= 3 { e0 } else { e1 };
        let extra = match height {
            1 => c0.boundary_block_extra_data(e0),
            2 => c1.preannounce_block_extra_data(e1),
            4 => c1.boundary_block_extra_data(e1),
            _ => Vec::new(),
        };
        let mut raw = Block::default();
        raw.header.number = height;
        raw.header.parent_hash = parent;
        raw.header.extra_data = Bytes::from(extra);
        let block = crate::block::ConsensusBlock::from_sealed(SealedBlock::seal_slow(
            raw.map_header(outbe_primitives::OutbeHeader::new),
        ));
        parent = block.block_hash();
        let finalization = signer.finalization_for(epoch, block.digest());
        records.insert(
            height,
            CertifiedFinalizedBlock {
                block,
                finalization,
            },
        );
    }
    let source = GappedFinalitySource {
        records: Arc::new(records),
    };
    let certificates = MemoryCertificates::default();
    let blocks = MemoryBlocks::default();
    for (lower, upper) in [(1, 2), (2, 6), (5, 6)] {
        let chain = SharedCommitteeChain::new(CommitteeChain::new(e0, c0.participants.clone()));
        let epocher = FollowerEpocher::new(3, 0);
        futures::executor::block_on(engine::authenticate_and_reconcile_replay_suffix(
            &chain,
            &source,
            &epocher,
            e0,
            Height::new(lower),
            Height::new(upper),
            certificates.clone(),
            blocks.clone(),
        ))
        .unwrap();
        assert_eq!(chain.lock().highest_registered(), Some(e1));
        if upper >= 4 {
            assert_eq!(epocher.activation_height(e1), Some(Height::new(4)));
        } else {
            // Crash cut: only the ancestor was executed, but its direct
            // certificate correctly remains indexed at descendant height 3.
            assert_eq!(epocher.activation_height(e1), None);
            assert!(blocks.by_height.lock().unwrap().contains_key(&2));
            assert!(certificates.by_height.lock().unwrap().contains_key(&3));
            assert!(!certificates.by_height.lock().unwrap().contains_key(&2));
        }
    }
    assert_eq!(blocks.by_height.lock().unwrap().len(), 6);
    assert!(!certificates.by_height.lock().unwrap().contains_key(&2));
    assert!(!certificates.by_height.lock().unwrap().contains_key(&4));
    assert!(certificates.by_height.lock().unwrap().contains_key(&3));
    assert!(certificates.by_height.lock().unwrap().contains_key(&5));
    let mut tampered =
        futures::executor::block_on(source.get_finality_proof(Height::new(2))).unwrap();
    tampered.ancestors[0] = source.records[&1].block.clone();
    assert!(tampered.validate_envelope(Height::new(2)).is_err());
    let mut omitted =
        futures::executor::block_on(source.get_finality_proof(Height::new(2))).unwrap();
    omitted.ancestors.clear();
    assert!(omitted.validate_envelope(Height::new(2)).is_err());
}
