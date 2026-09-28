use crate::world::rpc::*;

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

pub const COMPRESSED_ENTITY_TIMEOUT_SECS: u64 = 120;

impl Rpc {
    /// Fetch one latest-finalized compressed-entity package and its exact header.
    pub fn compressed_entity(
        &self,
        port: u16,
        request: PointReadRequestV1,
    ) -> Result<CompressedEntityAtHeader> {
        self.point_read(port, request)?
            .ok_or_else(|| eyre!("compressed-entity package is unavailable on port {port}"))
    }

    /// The CE marker, block finality and the off-chain bodies advance independently, so a
    /// point read answers `Unavailable` for a moment even once its height is finalized.
    pub fn compressed_entity_ready(
        &self,
        port: u16,
        request: PointReadRequestV1,
    ) -> Result<CompressedEntityAtHeader> {
        let deadline = Instant::now() + Duration::from_secs(COMPRESSED_ENTITY_TIMEOUT_SECS);
        loop {
            if let Some(package) = self.point_read(port, request)? {
                return Ok(package);
            }
            if Instant::now() >= deadline {
                return Err(eyre!(
                    "compressed-entity package stayed unavailable on port {port} for {COMPRESSED_ENTITY_TIMEOUT_SECS}s"
                ));
            }
            sleep(Duration::from_millis(250));
        }
    }

    fn point_read(
        &self,
        port: u16,
        request: PointReadRequestV1,
    ) -> Result<Option<CompressedEntityAtHeader>> {
        let result = eth::raw_json_with_params(
            &self.url(port),
            "outbe_getCompressedEntity",
            serde_json::json!([request]),
        )
        .ok_or_else(|| eyre!("outbe_getCompressedEntity returned no result on port {port}"))?;
        let result: PointReadResultV1 =
            serde_json::from_value(result).wrap_err("decode compressed-entity package")?;
        let common = match &result {
            PointReadResultV1::Present { common, .. }
            | PointReadResultV1::Absent { common, .. } => common,
            PointReadResultV1::Unavailable => return Ok(None),
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
        Ok(Some(CompressedEntityAtHeader {
            header: SelectedHeaderV1 {
                block_number: common.block_number,
                block_hash: common.block_hash,
                extra_data: extra_data.to_vec(),
            },
            result,
        }))
    }
}
