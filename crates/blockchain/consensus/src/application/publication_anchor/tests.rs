use super::*;
use outbe_primitives::{
    radicle_publication::PublicationBatch,
    reshare_artifact::{encode_outbe_block_artifacts, OutbeBlockArtifacts},
    OutbeBlock, OutbeHeader,
};
use reth_ethereum::primitives::SealedBlock;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Reader {
    finalized: BTreeMap<u64, ConsensusBlock>,
    calls: AtomicUsize,
}

impl FinalizedBlockReader for Reader {
    fn finalized_block(&self, height: u64) -> BlockLookupFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let block = self.finalized.get(&height).cloned();
        Box::pin(async move { block })
    }
}

fn block(number: u64, batch: Option<PublicationBatch>) -> ConsensusBlock {
    let artifacts = OutbeBlockArtifacts {
        radicle_publication_enabled: true,
        radicle_publication: batch,
        ..Default::default()
    };
    let header = alloy_consensus::Header {
        number,
        extra_data: encode_outbe_block_artifacts(&artifacts).unwrap(),
        ..Default::default()
    };
    ConsensusBlock::from_sealed(SealedBlock::seal_slow(OutbeBlock {
        header: OutbeHeader::new(header),
        body: Default::default(),
    }))
}

fn batch(height: u64, hash: B256) -> PublicationBatch {
    let mut batch = outbe_primitives::test_utils::sample_radicle_publication_batch(1);
    batch.body.chain_id = 2026;
    batch.body.genesis_hash = B256::repeat_byte(7);
    batch.body.finalized_anchor_height = height;
    batch.body.finalized_anchor_hash = hash;
    batch
}

#[tokio::test]
async fn publication_anchor_absence_and_exact_genesis_need_no_history_lookup() {
    let reader = Reader::default();
    let genesis = B256::repeat_byte(7);
    assert!(
        validate_publication_anchor(&block(1, None), 2026, genesis, &reader)
            .await
            .is_ok()
    );
    assert!(validate_publication_anchor(
        &block(1, Some(batch(0, genesis))),
        2026,
        genesis,
        &reader
    )
    .await
    .is_ok());
    assert!(matches!(
        validate_publication_anchor(
            &block(1, Some(batch(0, B256::repeat_byte(8)))),
            2026,
            genesis,
            &reader
        )
        .await,
        Err(PublicationAnchorError::GenesisHashMismatch)
    ));
    assert_eq!(reader.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn publication_anchor_preserves_opaque_headers_and_rejects_malformed_artifacts() {
    let reader = Reader::default();
    let genesis = B256::repeat_byte(7);
    for (extra_data, valid) in [
        (b"opaque builder header".as_slice(), true),
        (b"OART\x01\x00\x00".as_slice(), true),
        (b"OART\x01\x01\x00".as_slice(), true),
        (b"OART\x01\x02\x00".as_slice(), false),
        (b"OART\x0b\x00".as_slice(), false),
        (b"OART\x01\x01".as_slice(), false),
        (b"OART\x0c\x01".as_slice(), false),
    ] {
        let mut raw = block(1, None).into_inner().into_block();
        raw.header.inner.extra_data = extra_data.to_vec().into();
        let candidate = ConsensusBlock::from_sealed(SealedBlock::seal_slow(raw));
        let result = validate_publication_anchor(&candidate, 2026, genesis, &reader).await;
        if valid {
            assert!(result.is_ok());
        } else {
            assert!(matches!(
                result,
                Err(PublicationAnchorError::MalformedHeaderArtifacts(_))
            ));
            assert!(std::error::Error::source(&result.unwrap_err()).is_some());
        }
    }
    assert_eq!(reader.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn publication_anchor_requires_the_exact_local_finalized_block_with_one_lookup() {
    let genesis = B256::repeat_byte(7);
    let finalized = crate::test_fixtures::block_with_number(5);
    let mut reader = Reader::default();
    reader.finalized.insert(5, finalized.clone());
    assert!(validate_publication_anchor(
        &block(1000, Some(batch(5, finalized.block_hash()))),
        2026,
        genesis,
        &reader
    )
    .await
    .is_ok());
    assert_eq!(reader.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        validate_publication_anchor(
            &block(1000, Some(batch(5, B256::repeat_byte(9)))),
            2026,
            genesis,
            &reader
        )
        .await,
        Err(PublicationAnchorError::FinalizedHashMismatch)
    ));
    assert_eq!(reader.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn publication_anchor_missing_or_inconsistent_local_history_is_unavailable() {
    let genesis = B256::repeat_byte(7);
    let mut reader = Reader::default();
    let candidate = block(10, Some(batch(5, B256::repeat_byte(9))));
    assert!(matches!(
        validate_publication_anchor(&candidate, 2026, genesis, &reader).await,
        Err(PublicationAnchorError::Unavailable)
    ));
    reader
        .finalized
        .insert(5, crate::test_fixtures::block_with_number(6));
    assert!(matches!(
        validate_publication_anchor(&candidate, 2026, genesis, &reader).await,
        Err(PublicationAnchorError::Unavailable)
    ));
}

#[tokio::test]
async fn publication_anchor_rejects_foreign_chain_or_non_ancestor_height_before_lookup() {
    let genesis = B256::repeat_byte(7);
    let reader = Reader::default();
    for height in [10, 11] {
        assert!(matches!(
            validate_publication_anchor(
                &block(10, Some(batch(height, genesis))),
                2026,
                genesis,
                &reader
            )
            .await,
            Err(PublicationAnchorError::ChainOrHeightMismatch)
        ));
    }
    let mut foreign = batch(5, genesis);
    foreign.body.chain_id += 1;
    assert!(matches!(
        validate_publication_anchor(&block(10, Some(foreign)), 2026, genesis, &reader).await,
        Err(PublicationAnchorError::ChainOrHeightMismatch)
    ));
    let mut foreign = batch(5, genesis);
    foreign.body.genesis_hash = B256::repeat_byte(8);
    assert!(matches!(
        validate_publication_anchor(&block(10, Some(foreign)), 2026, genesis, &reader).await,
        Err(PublicationAnchorError::ChainOrHeightMismatch)
    ));
    assert_eq!(reader.calls.load(Ordering::SeqCst), 0);
}
