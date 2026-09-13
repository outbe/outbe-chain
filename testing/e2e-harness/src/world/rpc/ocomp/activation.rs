use crate::world::rpc::*;

impl Rpc {
    #[cfg(feature = "ocomp-integration")]
    pub fn active_ocomp_protocol_bundle_hash_on(&self, port: u16) -> Option<B256> {
        eth::read_call(
            &self.url(port),
            addresses::OCOMP_REGISTRY_ADDR,
            &IOcompRegistry::activeProtocolBundleHashCall {},
        )
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn retiring_ocomp_protocol_bundle_hash_on(&self, port: u16) -> Option<B256> {
        eth::read_call(
            &self.url(port),
            addresses::OCOMP_REGISTRY_ADDR,
            &IOcompRegistry::retiringProtocolBundleHashCall {},
        )
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn ocomp_live_lineage_count_on(&self, port: u16, bundle_hash: B256) -> Option<u32> {
        eth::read_call(
            &self.url(port),
            addresses::OCOMP_REGISTRY_ADDR,
            &IOcompRegistry::liveLineageCountCall {
                protocolBundleHash: bundle_hash,
            },
        )
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn ocomp_retention_until_on(&self, port: u16, bundle_hash: B256) -> Option<u64> {
        eth::read_call(
            &self.url(port),
            addresses::OCOMP_REGISTRY_ADDR,
            &IOcompRegistry::retentionUntilCall {
                protocolBundleHash: bundle_hash,
            },
        )
    }

    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn active_ocomp_protocol_bundle_hash_at_on(
        &self,
        port: u16,
        height: u64,
    ) -> Result<B256> {
        eth::read_call_at_result(
            &self.url(port),
            addresses::OCOMP_REGISTRY_ADDR,
            &IOcompRegistry::activeProtocolBundleHashCall {},
            height,
        )
        .map_err(|error| eyre!("read active OCOMP bundle at h{height}: {error}"))
    }

    /// Observe the predecessor retention state at one exact finalized checkpoint.
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn ocomp_retention_state_at_on(
        &self,
        port: u16,
        bundle_hash: B256,
        height: u64,
    ) -> Result<(B256, u32, u64)> {
        let url = self.url(port);
        let retiring = eth::read_call_at_result(
            &url,
            addresses::OCOMP_REGISTRY_ADDR,
            &IOcompRegistry::retiringProtocolBundleHashCall {},
            height,
        )
        .map_err(|error| eyre!("read retiring bundle at h{height}: {error}"))?;
        let live = eth::read_call_at_result(
            &url,
            addresses::OCOMP_REGISTRY_ADDR,
            &IOcompRegistry::liveLineageCountCall {
                protocolBundleHash: bundle_hash,
            },
            height,
        )
        .map_err(|error| eyre!("read live lineage at h{height}: {error}"))?;
        let until = eth::read_call_at_result(
            &url,
            addresses::OCOMP_REGISTRY_ADDR,
            &IOcompRegistry::retentionUntilCall {
                protocolBundleHash: bundle_hash,
            },
            height,
        )
        .map_err(|error| eyre!("read retention deadline at h{height}: {error}"))?;
        Ok((retiring, live, until))
    }

    /// Observe the finalized public `LysisActivated` result on one validator.
    pub fn finalized_ocomp_activation_on(
        &self,
        port: u16,
        from_height: u64,
        expected_intent_id: B256,
    ) -> Option<OcompPublicActivationV1> {
        let rpc_url = self.url(port);
        let finalized_height = eth::finalized_number(&rpc_url)?;
        if finalized_height < from_height {
            return None;
        }
        let topic0 = keccak256(b"LysisActivated(bytes32,bytes32,bytes32,bytes32,bytes32,uint32)");
        let logs = eth::raw_json_with_params(
            &rpc_url,
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::WWD_ADDR),
                "fromBlock": format!("0x{from_height:x}"),
                "toBlock": format!("0x{finalized_height:x}"),
                "topics": [
                    format!("{topic0:#x}"),
                    format!("{expected_intent_id:#x}")
                ]
            }]),
        )?;
        let log = logs.as_array()?.last()?;
        let topics = log.get("topics")?.as_array()?;
        if topics.len() != 3
            || topics[0].as_str()? != format!("{topic0:#x}")
            || topics[1].as_str()? != format!("{expected_intent_id:#x}")
        {
            return None;
        }
        let intent_id = topics[1].as_str()?.parse::<B256>().ok()?;
        let job_id = topics[2].as_str()?.parse::<B256>().ok()?;
        let data = hex::decode(log.get("data")?.as_str()?.trim_start_matches("0x")).ok()?;
        if data.len() != 4 * 32 {
            return None;
        }
        let activation_call_id = B256::from_slice(&data[0..32]);
        let result_digest = B256::from_slice(&data[32..64]);
        let terminal_receipt_hash = B256::from_slice(&data[64..96]);
        let worldwide_day = u32::try_from(U256::from_be_slice(&data[96..128])).ok()?;
        let block_number = u64::from_str_radix(
            log.get("blockNumber")?.as_str()?.trim_start_matches("0x"),
            16,
        )
        .ok()?;
        if block_number < from_height || block_number > finalized_height {
            return None;
        }
        let block_hash = log.get("blockHash")?.as_str()?.parse::<B256>().ok()?;
        if eth::block_hash(&rpc_url, block_number)?
            .parse::<B256>()
            .ok()?
            != block_hash
        {
            return None;
        }
        Some(OcompPublicActivationV1 {
            intent_id,
            job_id,
            activation_call_id,
            result_digest,
            terminal_receipt_hash,
            worldwide_day,
            block_number,
            block_hash,
            transaction_hash: log.get("transactionHash")?.as_str()?.parse::<B256>().ok()?,
        })
    }

    /// Prove that no finalized `LysisActivated` event exists for one intent.
    ///
    /// An unreadable finalized height, RPC failure, or malformed log response
    /// is not evidence of absence and therefore remains an error.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_activation_absent_on(
        &self,
        port: u16,
        from_height: u64,
        expected_intent_id: B256,
    ) -> Result<bool> {
        let finalized_height = self.finalized_result(port)?;
        ensure!(
            finalized_height >= from_height,
            "RPC {port} has only finalized {finalized_height}, below OCOMP request {from_height}"
        );
        let topic0 = keccak256(b"LysisActivated(bytes32,bytes32,bytes32,bytes32,bytes32,uint32)");
        let value = eth::raw_json_result(
            &self.url(port),
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::WWD_ADDR),
                "fromBlock": format!("0x{from_height:x}"),
                "toBlock": format!("0x{finalized_height:x}"),
                "topics": [
                    format!("{topic0:#x}"),
                    format!("{expected_intent_id:#x}")
                ]
            }]),
        )
        .wrap_err_with(|| format!("read finalized LysisActivated events from RPC {port}"))?;
        let logs = value
            .as_array()
            .ok_or_else(|| eyre!("eth_getLogs on RPC {port} returned a non-array result"))?;
        Ok(logs.is_empty())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompPublicActivationV1 {
    pub intent_id: B256,
    pub job_id: B256,
    pub activation_call_id: B256,
    pub result_digest: B256,
    pub terminal_receipt_hash: B256,
    pub worldwide_day: u32,
    pub block_number: u64,
    pub block_hash: B256,
    pub transaction_hash: B256,
}
