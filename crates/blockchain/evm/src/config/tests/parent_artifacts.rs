use super::*;

#[derive(Clone, Default)]
struct TestHeaderProvider {
    sealed: Option<SealedHeader<OutbeHeader>>,
}

impl HeaderProvider for TestHeaderProvider {
    type Header = OutbeHeader;

    fn header(&self, block_hash: B256) -> ProviderResult<Option<Self::Header>> {
        Ok(self
            .sealed
            .as_ref()
            .filter(|sealed| sealed.hash() == block_hash)
            .map(|sealed| sealed.header().clone()))
    }

    fn header_by_number(&self, num: u64) -> ProviderResult<Option<Self::Header>> {
        Ok(self
            .sealed
            .as_ref()
            .filter(|sealed| sealed.header().inner.number == num)
            .map(|sealed| sealed.header().clone()))
    }

    fn headers_range(&self, _range: impl RangeBounds<u64>) -> ProviderResult<Vec<Self::Header>> {
        Ok(Vec::new())
    }

    fn sealed_header(&self, number: u64) -> ProviderResult<Option<SealedHeader<Self::Header>>> {
        Ok(self
            .sealed
            .as_ref()
            .filter(|sealed| sealed.header().inner.number == number)
            .cloned())
    }

    fn sealed_headers_while(
        &self,
        _range: impl RangeBounds<u64>,
        _predicate: impl FnMut(&SealedHeader<Self::Header>) -> bool,
    ) -> ProviderResult<Vec<SealedHeader<Self::Header>>> {
        Ok(Vec::new())
    }
}

/// cache-first lookup. When the bridge cache holds an entry
/// keyed by exact `(block_number, block_hash)`, the provider must return
/// it even if no Reth header is reachable yet. This is the proposer/
/// validator fast-path before the import pipeline has indexed the parent.
#[test]
fn accounted_parent_artifact_provider_uses_cache_when_provider_header_is_not_visible_yet() {
    let bridge = ConsensusExecutionBridge::new();
    let summary = test_summary();
    let block_hash = B256::repeat_byte(0x42);
    let state_root = B256::repeat_byte(0x43);
    bridge.record_execution_summary_with_state_root(7, block_hash, summary, 123, state_root);
    let provider = RethAccountedParentArtifactProvider::new(
        TestHeaderProvider::default(),
        Some(bridge.clone()),
    );

    let resolved = provider
        .execution_summary_by_hash(7, block_hash)
        .expect("provider read must not fail")
        .expect("cache must bridge provider visibility race");

    assert_eq!(resolved.summary, summary);
    assert_eq!(resolved.timestamp, 123);
    assert_eq!(resolved.state_root, Some(state_root));
}
