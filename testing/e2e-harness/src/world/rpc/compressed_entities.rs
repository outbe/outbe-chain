use crate::world::rpc::*;

fn wait_for_compressed_package(
    mut fetch: impl FnMut() -> Result<PointReadResultV1>,
    timeout: std::time::Duration,
) -> Result<PointReadResultV1> {
    let started = std::time::Instant::now();
    loop {
        let result = fetch()?;
        if !matches!(result, PointReadResultV1::Unavailable) {
            return Ok(result);
        }
        eyre::ensure!(
            started.elapsed() < timeout,
            "compressed-entity package remained unavailable for {timeout:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompressedEntityAtHeader {
    pub result: PointReadResultV1,
    pub header: SelectedHeaderV1,
}

impl CompressedEntityAtHeader {
    /// Hash the exact JSON transport package and extract its authenticated CE root.
    pub fn evidence_identity(&self) -> Result<(String, String)> {
        let proof_sha256 = sha256_hex(&serde_json::to_vec(&self.result)?);
        let artifacts = decode_outbe_block_artifacts(&self.header.extra_data)
            .map_err(|error| eyre!("decode compressed-entity header artifacts: {error}"))?;
        let ce_root = artifacts
            .compressed_entities_root
            .ok_or_else(|| eyre!("compressed-entity header has no CE root"))?
            .r_sealed
            .to_string();
        Ok((ce_root, proof_sha256))
    }
}

impl Rpc {
    /// Fetch one latest-finalized compressed-entity package and its exact header.
    pub fn compressed_entity(
        &self,
        port: u16,
        request: PointReadRequestV1,
    ) -> Result<CompressedEntityAtHeader> {
        // The finalized CE marker, canonical header and current body are read
        // separately by the service. It fails closed with Unavailable while
        // they advance; wait for a coherent package, never accept a partial one.
        let result = wait_for_compressed_package(
            || {
                let result = eth::raw_json_with_params(
                    &self.url(port),
                    "outbe_getCompressedEntity",
                    serde_json::json!([request]),
                )
                .ok_or_else(|| {
                    eyre!("outbe_getCompressedEntity returned no result on port {port}")
                })?;
                serde_json::from_value(result).wrap_err("decode compressed-entity package")
            },
            std::time::Duration::from_secs(30),
        )
        .wrap_err_with(|| format!("finalized compressed-entity read on port {port}"))?;
        let common = match &result {
            PointReadResultV1::Present { common, .. }
            | PointReadResultV1::Absent { common, .. } => common,
            PointReadResultV1::Unavailable => {
                return Err(eyre!(
                    "compressed-entity package is unavailable on port {port}"
                ));
            }
        };
        let block = eth::raw_json_with_params(
            &self.url(port),
            "eth_getBlockByHash",
            serde_json::json!([common.block_hash, false]),
        )
        .ok_or_else(|| eyre!("selected block {} is unavailable", common.block_hash))?;
        let returned_hash = block
            .get("hash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre!("selected block has no hash"))?;
        if !returned_hash.eq_ignore_ascii_case(&common.block_hash.to_string()) {
            return Err(eyre!("selected block hash does not match proof package"));
        }
        let returned_number = block
            .get("number")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
            .ok_or_else(|| eyre!("selected block has no canonical number"))?;
        if returned_number != common.block_number {
            return Err(eyre!("selected block number does not match proof package"));
        }
        let extra_data: Bytes = serde_json::from_value(
            block
                .get("extraData")
                .cloned()
                .ok_or_else(|| eyre!("selected block has no extraData"))?,
        )
        .wrap_err("decode selected block extraData")?;
        Ok(CompressedEntityAtHeader {
            header: SelectedHeaderV1 {
                block_number: common.block_number,
                block_hash: common.block_hash,
                extra_data: extra_data.to_vec(),
            },
            result,
        })
    }
}

#[cfg(test)]
mod polling_tests {
    use super::*;
    use outbe_compressed_entities::{
        AbsentEvidenceV1, CkbCompiledProofV1, PointProofCommonV1, WwdEntityId,
    };

    #[test]
    fn compressed_package_poll_preserves_the_available_response_after_unavailability() {
        // Polling must carry the transport package unchanged; cryptographic
        // verification remains the caller's separate responsibility.
        let available = PointReadResultV1::Absent {
            common: PointProofCommonV1 {
                proof_encoding_version: 1,
                chain_id: 54322345,
                block_number: 400,
                block_hash: B256::repeat_byte(1),
                domain_id: 2,
                raw_id: WwdEntityId::from_day_and_digest(
                    outbe_primitives::time::WorldwideDay::new(20260926),
                    B256::repeat_byte(2),
                ),
            },
            evidence: AbsentEvidenceV1::CollectionAbsent {
                root_catalog_proof: CkbCompiledProofV1::new(vec![1]).unwrap(),
            },
        };
        let mut responses = [PointReadResultV1::Unavailable, available.clone()].into_iter();
        let result = wait_for_compressed_package(
            || Ok(responses.next().expect("unexpected extra read")),
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(result, available);
        assert!(responses.next().is_none());
    }

    #[test]
    fn compressed_package_poll_does_not_hide_failure_or_invent_absence() {
        let error = wait_for_compressed_package(
            || Ok(PointReadResultV1::Unavailable),
            std::time::Duration::ZERO,
        )
        .unwrap_err();
        assert!(error.to_string().contains("remained unavailable"));
        let mut calls = 0;
        let error = wait_for_compressed_package(
            || {
                calls += 1;
                Err(eyre!("invalid RPC response"))
            },
            std::time::Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(calls, 1);
        assert_eq!(error.to_string(), "invalid RPC response");
    }
}
