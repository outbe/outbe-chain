//! Restart readiness must come from one DB-only snapshot, never the memory tree.

use alloy_primitives::B256;
use eyre::{Result, WrapErr};
use outbe_primitives::OutbeHeader;
use reth_provider::providers::{BlockchainProvider, ProviderNodeTypes};
use reth_provider::{
    ChainStateBlockReader, DatabaseProviderFactory, HeaderProvider, StageCheckpointReader,
    StateProviderBox,
};
use reth_stages_types::StageId;

pub(crate) trait DurableTeeStateProvider: DatabaseProviderFactory {
    fn state_from_durable_snapshot(&self, snapshot: Self::Provider, hash: B256)
        -> StateProviderBox;
}
impl<N: ProviderNodeTypes> DurableTeeStateProvider for BlockchainProvider<N> {
    fn state_from_durable_snapshot(
        &self,
        snapshot: Self::Provider,
        hash: B256,
    ) -> StateProviderBox {
        self.state_provider_from_database(snapshot, hash)
    }
}

/// Return the exact persisted anchor's timestamp and state once both finality
/// and execution are committed. Absence/lag means retry; corruption is an error.
pub(super) fn durable_anchor_state<P>(
    provider: &P,
    height: u64,
    expected_hash: B256,
) -> Result<Option<(u64, StateProviderBox)>>
where
    P: DurableTeeStateProvider,
    P::Provider:
        ChainStateBlockReader + HeaderProvider<Header = OutbeHeader> + StageCheckpointReader,
{
    let durable = provider
        .database_provider_ro()
        .wrap_err("open durable TEE recovery snapshot")?;
    if !durable
        .last_finalized_block_number()?
        .is_some_and(|finalized| finalized >= height)
    {
        return Ok(None);
    }
    let Some(header) = durable.sealed_header(height)? else {
        return Ok(None);
    };
    eyre::ensure!(
        header.hash() == expected_hash,
        "durable TEE recovery anchor hash mismatch at height {height}: expected {expected_hash}, local {}",
        header.hash(),
    );
    let timestamp = header.header().inner.timestamp;
    if !durable
        .get_stage_checkpoint(StageId::Finish)?
        .is_some_and(|stage| stage.block_number >= height)
    {
        return Ok(None);
    }
    let state = provider.state_from_durable_snapshot(durable, expected_hash);
    Ok(Some((timestamp, state)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_node::OutbeNode;
    use reth_provider::{
        test_utils::create_test_provider_factory_with_node_types, ChainStateBlockWriter,
        StageCheckpointWriter, StaticFileProviderFactory, StaticFileSegment, StaticFileWriter,
    };
    use std::sync::Arc;

    #[test]
    fn finality_header_and_executed_state_must_all_be_committed() {
        let spec = Arc::new(
            reth_chainspec::MAINNET
                .as_ref()
                .clone()
                .map_header(OutbeHeader::new),
        );
        let factory = create_test_provider_factory_with_node_types::<OutbeNode>(spec);
        let provider = BlockchainProvider::with_latest(
            factory.clone(),
            reth_primitives_traits::SealedHeader::seal_slow(OutbeHeader::new(Default::default())),
        )
        .unwrap();
        let expected = B256::repeat_byte(0x55);
        assert!(durable_anchor_state(&provider, 5, expected)
            .unwrap()
            .is_none());

        let write = factory.provider_rw().unwrap();
        write.save_finalized_block_number(5).unwrap();
        // An uncommitted transaction must not make the separate RO view ready.
        assert!(durable_anchor_state(&provider, 5, expected)
            .unwrap()
            .is_none());
        write.commit().unwrap();
        assert!(durable_anchor_state(&provider, 5, expected)
            .unwrap()
            .is_none());

        {
            let files = factory.static_file_provider();
            let mut headers = files.latest_writer(StaticFileSegment::Headers).unwrap();
            for number in 0..=5 {
                let header = OutbeHeader::new(alloy_consensus::Header {
                    number,
                    timestamp: 100 + number,
                    ..Default::default()
                });
                headers.append_header(&header, &expected).unwrap();
            }
            headers.commit().unwrap();
        }
        // Finality and header visibility do not certify execution persistence.
        assert!(durable_anchor_state(&provider, 5, expected)
            .unwrap()
            .is_none());
        let write = factory.provider_rw().unwrap();
        write.update_pipeline_stages(5, false).unwrap();
        assert!(durable_anchor_state(&provider, 5, expected)
            .unwrap()
            .is_none());
        write.commit().unwrap();
        let (timestamp, state) = durable_anchor_state(&provider, 5, expected)
            .unwrap()
            .unwrap();
        assert_eq!(timestamp, 105);
        assert_eq!(
            state
                .storage(alloy_primitives::Address::ZERO, B256::ZERO)
                .unwrap(),
            None
        );
        assert!(durable_anchor_state(&provider, 6, expected)
            .unwrap()
            .is_none());

        let error = durable_anchor_state(&provider, 5, B256::repeat_byte(0x66))
            .err()
            .unwrap();
        assert!(error.to_string().contains("anchor hash mismatch"));
    }
}
