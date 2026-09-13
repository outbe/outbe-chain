use super::*;

#[derive(Clone)]
struct FinalityAnchorProvider {
    finalized: BlockNumHash,
    canonical: B256,
}

impl BlockHashReader for FinalityAnchorProvider {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        Ok((number == 7).then_some(self.canonical))
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        Ok((start..end)
            .filter_map(|number| (number == 7).then_some(self.canonical))
            .collect())
    }
}

impl BlockNumReader for FinalityAnchorProvider {
    fn chain_info(&self) -> ProviderResult<ChainInfo> {
        Ok(ChainInfo {
            best_hash: self.finalized.hash,
            best_number: self.finalized.number,
        })
    }

    fn best_block_number(&self) -> ProviderResult<u64> {
        Ok(self.finalized.number)
    }

    fn last_block_number(&self) -> ProviderResult<u64> {
        Ok(self.finalized.number)
    }

    fn block_number(&self, hash: B256) -> ProviderResult<Option<u64>> {
        Ok((hash == self.canonical).then_some(7))
    }
}

impl BlockIdReader for FinalityAnchorProvider {
    fn pending_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(None)
    }

    fn safe_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.finalized))
    }

    fn finalized_block_num_hash(&self) -> ProviderResult<Option<BlockNumHash>> {
        Ok(Some(self.finalized))
    }
}

struct CountingFinality {
    calls: Arc<AtomicUsize>,
}

impl OcompFinalizedIntentAuthority for CountingFinality {
    fn verify(
        &self,
        _proof: &FinalizedIntentProofV1,
        _expected: ExpectedFinalizedIntentBindingV1,
        _limits: &SchemaLimits,
    ) -> Result<VerifiedFinalizedIntentV1, OcompFinalityAuthorityError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Err(FinalizedIntentVerificationError::WrongChain.into())
    }
}

fn anchor_proof(block_number: u64, block_hash: B256) -> FinalizedIntentProofV1 {
    FinalizedIntentProofV1 {
        chain_id: 1,
        genesis_hash: B256::ZERO,
        fork_id: B256::ZERO,
        protocol_bundle_hash: B256::ZERO,
        canonical_request_header_rlp: ProofBytes(Vec::new()),
        parent_accounting: CertifiedParentAccountingMetadataV2 {
            finalized_block_number: block_number,
            finalized_block_hash: block_hash,
            finalized_epoch: 0,
            finalized_view: 0,
            parent_view: 0,
            ordered_committee: Vec::new(),
            signer_bitmap: BoundedBytes(Vec::new()),
            canonical_commonware_finalization_proof: ProofBytes(Vec::new()),
            committee_set_hash: B256::ZERO,
            vrf_material_version: 0,
            vrf_group_public_key_hash: B256::ZERO,
            proof_kind: ParentProofKind::Finalization,
            missed_proposers: Vec::new(),
        },
        historical_committee_membership_proof: ProofBytes(Vec::new()),
        canonical_job_intent: BoundedBytes(Vec::new()),
        intent_account_proof: ProofBytes(Vec::new()),
        intent_storage_proof: ProofBytes(Vec::new()),
    }
}

#[test]
fn production_finality_wrapper_requires_node_owned_canonical_finalized_anchor() {
    let canonical = B256::repeat_byte(0x77);
    let calls = Arc::new(AtomicUsize::new(0));
    let authority = ProviderAnchoredOcompFinalityAuthority::new(
        FinalityAnchorProvider {
            finalized: BlockNumHash::new(10, B256::repeat_byte(0xAA)),
            canonical,
        },
        Arc::new(CountingFinality {
            calls: calls.clone(),
        }),
    );
    let expected = ExpectedFinalizedIntentBindingV1 {
        chain_id: 1,
        genesis_hash: B256::ZERO,
        fork_id: B256::ZERO,
        protocol_bundle_hash: B256::ZERO,
    };
    let limits = outbe_metadosis::config::poc_schema_limits();

    let side_chain = authority
        .verify(&anchor_proof(7, B256::repeat_byte(0x88)), expected, &limits)
        .unwrap_err();
    assert!(matches!(
        side_chain,
        OcompFinalityAuthorityError::InvalidProof(
            FinalizedIntentVerificationError::FinalizedHeaderMetadataMismatch
        )
    ));
    assert_eq!(calls.load(Ordering::Relaxed), 0);

    let above_finalized = authority
        .verify(&anchor_proof(11, canonical), expected, &limits)
        .unwrap_err();
    assert!(matches!(
        above_finalized,
        OcompFinalityAuthorityError::LocalAuthority(_)
    ));
    assert_eq!(calls.load(Ordering::Relaxed), 0);

    let delegated = authority
        .verify(&anchor_proof(7, canonical), expected, &limits)
        .unwrap_err();
    assert!(matches!(
        delegated,
        OcompFinalityAuthorityError::InvalidProof(FinalizedIntentVerificationError::WrongChain)
    ));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}
