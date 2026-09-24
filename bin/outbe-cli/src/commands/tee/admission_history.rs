//! History discovery is untrusted; only certified headers authorize admission.
use super::{json_hex_array, json_hex_bytes, json_hex_u64_field};
use crate::rpc::Rpc;
use eyre::{Result, WrapErr};
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS,
    reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact},
};
use outbe_tee::finalized_admission::CertifiedHeaderV1;

/// Retry only the RPC's historical-proof-window error. Every returned opening
/// remains tied to its exact finalized height; callers must rebuild the header
/// witness for that height and the enclave rechecks all recipient claims.
pub(super) async fn registry_opening(
    rpc: &(impl Rpc + Sync),
    mut height: u64,
    slots: &[String],
) -> Result<(u64, serde_json::Value)> {
    for attempt in 0..4 {
        match rpc.eth_get_proof(TEE_REGISTRY_ADDRESS, slots, height).await {
            Ok(opening) => return Ok((height, opening)),
            Err(error) => {
                let expired = error.chain().any(|cause| {
                    cause
                        .to_string()
                        .contains("distance to target block exceeds maximum proof window")
                });
                if !expired || attempt == 3 {
                    return Err(error).wrap_err("read exact finalized TeeRegistry MPT opening");
                }
                let fresh = rpc.eth_get_finalized_block().await?;
                let next = json_hex_u64_field(&fresh, "number")?;
                eyre::ensure!(
                    next >= height && next > 0,
                    "finalized proof height regressed"
                );
                height = next;
                eprintln!("TeeRegistry proof window expired; retrying finalized height {height}");
            }
        }
    }
    unreachable!("bounded proof retry returns on its final attempt")
}

pub(super) async fn history_artifact(
    rpc: &(impl Rpc + Sync),
    height: u64,
) -> Result<Option<ConsensusHeaderArtifact>> {
    let block = rpc
        .eth_get_block_by_number(height)
        .await
        .wrap_err_with(|| format!("read finalized history header at height {height}"))?;
    eyre::ensure!(
        json_hex_u64_field(&block, "number")? == height,
        "history header height mismatch at {height}"
    );
    let extra = json_hex_bytes(&block, "extraData")?;
    Ok(decode_outbe_block_artifacts(&extra)
        .map_err(|error| eyre::eyre!("decode history header {height}: {error:?}"))?
        .consensus_header_artifact)
}

/// Marshal can finalize ancestors without storing a direct certificate for
/// every height. Admission needs certificates only for committee transitions
/// and its exact state root. Ordinary history headers are discovery hints:
/// omitting/forging a transition cannot pass the enclave's committee verifier.
pub(super) async fn admission_public(
    rpc: &(impl Rpc + Sync),
    height: u64,
    finalized_height: u64,
    next_transition_epoch: u64,
) -> Result<Option<serde_json::Value>> {
    eyre::ensure!(
        height > 0 && height <= finalized_height,
        "invalid admission history height"
    );
    if height < finalized_height
        && !matches!(
            history_artifact(rpc, height).await?,
            Some(ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, .. }) if epoch == next_transition_epoch
        )
    {
        return Ok(None);
    }
    rpc.outbe_get_finality_proof(height)
        .await
        .wrap_err_with(|| format!("read required finalization at height {height}"))
        .map(Some)
}

/// Convert RPC transport into the compact proof checked inside the enclave.
/// Hash-link checks here catch transport corruption early; the enclave repeats
/// them and authenticates the final signature against its committee chain.
pub(super) fn compact_header(
    public: &serde_json::Value,
    height: u64,
) -> Result<(outbe_consensus::block::ConsensusBlock, CertifiedHeaderV1)> {
    let finalization = json_hex_bytes(public, "finalizationHex")?;
    let encoded = json_hex_bytes(public, "blockHex")?;
    let certified =
        outbe_consensus::follow::decode_public_finalized_block(&finalization, &encoded, 256)?;
    eyre::ensure!(
        certified.finalization.proposal.payload == certified.block.digest(),
        "finality certificate payload mismatch"
    );
    let ancestors = match public.get("ancestorBlocksHex") {
        None => Vec::new(),
        Some(_) => json_hex_array(public, "ancestorBlocksHex")?,
    };
    eyre::ensure!(
        ancestors.len() <= outbe_tee::finalized_admission::MAX_FINALITY_DESCENDANTS,
        "too many finality ancestors"
    );
    let mut blocks = Vec::with_capacity(ancestors.len() + 1);
    for bytes in &ancestors {
        let block = outbe_consensus::follow::upstream::decode_public_block(bytes)?;
        blocks.push(block);
    }
    blocks.push(certified.block);
    eyre::ensure!(
        blocks[0].number() == height,
        "finality proof height mismatch at {height}"
    );
    for pair in blocks.windows(2) {
        eyre::ensure!(
            pair[0].number().checked_add(1) == Some(pair[1].number())
                && pair[1].parent_digest() == pair[0].digest(),
            "invalid finality ancestor chain"
        );
    }
    let target = blocks[0].clone();
    Ok((
        target.clone(),
        CertifiedHeaderV1 {
            finalization,
            header: alloy_rlp::encode(target.header()).to_vec(),
            descendants: blocks
                .iter()
                .skip(1)
                .map(|block| alloy_rlp::encode(block.header()).to_vec())
                .collect(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::mock::MockRpc;
    use outbe_primitives::reshare_artifact::{encode_outbe_block_artifacts, OutbeBlockArtifacts};

    #[tokio::test]
    async fn expired_proof_refreshes_exact_finalized_height() {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let rpc = MockRpc {
            finalized_block: Ok(serde_json::json!({"number":"0x90"})),
            proof_fn: Some(Box::new(move |height| {
                recorded.lock().unwrap().push(height);
                if height == 10 {
                    eyre::bail!("distance to target block exceeds maximum proof window")
                }
                Ok(serde_json::json!({"height":height}))
            })),
            ..Default::default()
        };
        let (height, proof) = registry_opening(&rpc, 10, &[]).await.unwrap();
        assert_eq!(height, 144);
        assert_eq!(proof["height"], 144);
        assert_eq!(*calls.lock().unwrap(), vec![10, 144]);
    }

    #[tokio::test]
    async fn proof_retries_are_bounded_and_only_for_window_expiry() {
        for (message, expected_calls) in [
            ("distance to target block exceeds maximum proof window", 4),
            ("invalid proof request", 1),
        ] {
            let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let recorded = calls.clone();
            let rpc = MockRpc {
                finalized_block: Ok(serde_json::json!({"number":"0x90"})),
                proof_fn: Some(Box::new(move |_| {
                    recorded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err(eyre::eyre!(message))
                })),
                ..Default::default()
            };
            assert!(registry_opening(&rpc, 10, &[]).await.is_err());
            assert_eq!(
                calls.load(std::sync::atomic::Ordering::SeqCst),
                expected_calls
            );
        }
    }

    #[tokio::test]
    async fn proof_retry_rejects_regressing_finalized_height() {
        let rpc = MockRpc {
            finalized_block: Ok(serde_json::json!({"number":"0x9"})),
            proof_fn: Some(Box::new(|_| {
                eyre::bail!("distance to target block exceeds maximum proof window")
            })),
            ..Default::default()
        };
        assert!(registry_opening(&rpc, 10, &[])
            .await
            .unwrap_err()
            .to_string()
            .contains("regressed"));
    }

    fn history_rpc(height: u64, artifact: Option<ConsensusHeaderArtifact>) -> MockRpc {
        let extra = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            consensus_header_artifact: artifact,
            ..Default::default()
        })
        .unwrap();
        let mut rpc = MockRpc::default();
        rpc.block_by_number = Ok(serde_json::json!({
            "number": format!("0x{height:x}"), "extraData": format!("0x{}", hex::encode(extra))
        }));
        rpc
    }

    #[tokio::test]
    async fn ordinary_ancestor_does_not_require_a_direct_certificate() {
        // MockRpc deliberately has no finalization endpoint. This was the
        // migration failure for an indirectly finalized ordinary block 43.
        let rpc = history_rpc(43, None);
        assert!(admission_public(&rpc, 43, 48, 1).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn missing_admission_or_transition_certificate_still_fails_closed() {
        let rpc = history_rpc(
            43,
            Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
                epoch: 1,
                outcome: vec![1].into(),
            }),
        );
        let error = admission_public(&rpc, 43, 48, 1).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("required finalization at height 43"));
        let rpc = history_rpc(48, None);
        let error = admission_public(&rpc, 48, 48, 1).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("required finalization at height 48"));
    }

    #[tokio::test]
    async fn wrong_history_height_is_rejected() {
        let rpc = history_rpc(44, None);
        assert!(admission_public(&rpc, 43, 48, 1)
            .await
            .unwrap_err()
            .to_string()
            .contains("height mismatch"));
    }
}
