use crate::world::rpc::*;

impl Rpc {
    /// Read and decode the four fixed result-vote slots at the finalized head.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_vote_accountability_on(
        &self,
        port: u16,
        job_id: B256,
    ) -> Option<OcompPublicVoteAccountabilityV1> {
        let finalized_height = self.finalized_result(port).ok()?;
        self.ocomp_vote_accountability_at_on(port, job_id, finalized_height)
            .ok()
    }

    /// Read OCOMP vote accountability at one exact already-finalized height.
    #[cfg(feature = "ocomp-integration")]
    pub fn ocomp_vote_accountability_at_on(
        &self,
        port: u16,
        job_id: B256,
        height: u64,
    ) -> Result<OcompPublicVoteAccountabilityV1> {
        let encoded = eth::read_call_at_result(
            &self.url(port),
            addresses::WWD_ADDR,
            &IMetadosis::getOffchainVoteAccountabilityCall { jobId: job_id },
            height,
        )
        .map_err(|error| eyre!(error))?;
        let accountability =
            OcompVoteAccountabilityV1::decode_canonical(encoded.as_ref(), &poc_schema_limits())
                .map_err(|error| {
                    eyre!("decode OCOMP vote accountability at h{height} on port {port}: {error}")
                })?;
        let quorum = accountability.quorum.as_ref();
        let closed = accountability.closed_summary.as_ref();
        Ok(OcompPublicVoteAccountabilityV1 {
            job_id: accountability.job_id,
            result_validator_set_epoch: accountability.result_validator_set_epoch,
            result_committee_set_hash: accountability.result_committee_set_hash,
            result_ocomp_binding_hash: accountability.result_ocomp_binding_hash,
            member_count: accountability.member_count,
            quorum_threshold: accountability.quorum_threshold,
            slot_validator_indexes: accountability
                .slots
                .iter()
                .flatten()
                .map(|slot| slot.validator_index)
                .collect(),
            slot_first_signatures: accountability
                .slots
                .iter()
                .flatten()
                .map(|slot| (slot.validator_index, slot.first_signature_rs.to_vec()))
                .collect(),
            quorum_result_digest: quorum.map(|value| value.result_digest),
            quorum_height: quorum.map(|value| value.quorum_height),
            quorum_signer_bitmap: quorum.map(|value| value.signer_bitmap.clone()),
            closed_height: closed.map(|value| value.closed_height),
            timely_bitmap: closed.map(|value| value.timely_bitmap.clone()),
            matching_bitmap: closed.map(|value| value.matching_bitmap.clone()),
            divergent_bitmap: closed.map(|value| value.divergent_bitmap.clone()),
            missing_bitmap: closed.map(|value| value.missing_bitmap.clone()),
            equivocation_bitmap: closed.map(|value| value.equivocation_bitmap.clone()),
        })
    }

    /// Enumerate canonical finalized public result-vote transactions from a
    /// bounded block range. This observes the real RPC -> txpool -> proposal ->
    /// import path rather than accepting transaction identities from a helper.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_result_vote_transactions_on(
        &self,
        port: u16,
        from_height: u64,
        to_height: u64,
    ) -> Option<Vec<OcompPublicResultVoteTransactionV1>> {
        if from_height > to_height {
            return None;
        }
        let rpc_url = self.url(port);
        let finalized_height = eth::finalized_number(&rpc_url)?;
        if to_height > finalized_height {
            return None;
        }
        const MAX_PUBLIC_VOTE_SCAN_BLOCKS: usize = 256;
        let selector = outbe_ocomp_protocol::abi::SUBMIT_LYSIS_RESULT_SELECTOR;
        let mut observed = Vec::new();
        let blocks = eth::blocks_with_transactions(
            &rpc_url,
            from_height,
            to_height,
            MAX_PUBLIC_VOTE_SCAN_BLOCKS,
        )?;
        for (height, block) in (from_height..=to_height).zip(blocks) {
            let block_hash = block.get("hash")?.as_str()?.parse::<B256>().ok()?;
            let rpc_block = serde_json::from_value::<alloy_rpc_types::Block>(block.clone()).ok()?;
            let consensus_block: alloy_consensus::Block<alloy_consensus::TxEnvelope> =
                rpc_block.into();
            let block_rlp_len = alloy_rlp::Encodable::length(&consensus_block);
            let transactions = block.get("transactions")?.as_array()?;
            for transaction in transactions {
                let to = transaction
                    .get("to")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| value.parse::<Address>().ok());
                if to != Some(addresses::WWD_ADDR) {
                    continue;
                }
                let calldata =
                    hex::decode(transaction.get("input")?.as_str()?.trim_start_matches("0x"))
                        .ok()?;
                if calldata.get(..4) != Some(selector.as_slice()) {
                    continue;
                }
                let transaction_hash = transaction.get("hash")?.as_str()?.parse::<B256>().ok()?;
                let signer = transaction.get("from")?.as_str()?.parse::<Address>().ok()?;
                let receipt = eth::receipt_json(&rpc_url, &format!("{transaction_hash:#x}"))?;
                let receipt_block = receipt.get("blockNumber")?.as_str().and_then(|value| {
                    u64::from_str_radix(value.trim_start_matches("0x"), 16).ok()
                })?;
                let receipt_hash = receipt.get("blockHash")?.as_str()?.parse::<B256>().ok()?;
                if receipt_block != height || receipt_hash != block_hash {
                    return None;
                }
                let raw_transaction_len = eth::raw_json_with_params(
                    &rpc_url,
                    "eth_getRawTransactionByHash",
                    serde_json::json!([format!("{transaction_hash:#x}")]),
                )
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .and_then(|value| hex::decode(value.trim_start_matches("0x")).ok())
                .map(|bytes| bytes.len())?;
                let gas_used = receipt.get("gasUsed")?.as_str().and_then(|value| {
                    u64::from_str_radix(value.trim_start_matches("0x"), 16).ok()
                })?;
                let success = receipt.get("status")?.as_str()? == "0x1";
                observed.push(OcompPublicResultVoteTransactionV1 {
                    transaction_hash,
                    signer,
                    block_number: height,
                    block_hash,
                    calldata_len: calldata.len(),
                    raw_transaction_len,
                    block_rlp_len,
                    gas_used,
                    success,
                });
            }
        }
        Some(observed)
    }

    /// Decode the exact canonical inner vote from one public transaction.
    #[cfg(feature = "ocomp-integration")]
    pub fn ocomp_result_vote_bytes_on(&self, port: u16, transaction_hash: B256) -> Option<Vec<u8>> {
        let transaction = eth::raw_json_with_params(
            &self.url(port),
            "eth_getTransactionByHash",
            serde_json::json!([format!("{transaction_hash:#x}")]),
        )?;
        let calldata =
            hex::decode(transaction.get("input")?.as_str()?.trim_start_matches("0x")).ok()?;
        if calldata.len() < 68
            || calldata.get(..4)
                != Some(outbe_ocomp_protocol::abi::SUBMIT_LYSIS_RESULT_SELECTOR.as_slice())
            || U256::from_be_slice(&calldata[4..36]) != U256::from(32)
        {
            return None;
        }
        let payload_len = usize::try_from(U256::from_be_slice(&calldata[36..68])).ok()?;
        let payload_end = 68_usize.checked_add(payload_len)?;
        let padded_end = 68_usize.checked_add(payload_len.checked_add(31)? & !31)?;
        if calldata.len() != padded_end
            || payload_end > calldata.len()
            || calldata[payload_end..].iter().any(|byte| *byte != 0)
        {
            return None;
        }
        Some(calldata[68..payload_end].to_vec())
    }

    /// Submit an adversarial or replayed canonical inner vote through a normal
    /// public validator transaction. The caller supplies only bytes previously
    /// observed from the public chain (possibly deliberately mutated); it
    /// cannot insert protocol state directly.
    #[cfg(feature = "ocomp-integration")]
    pub fn submit_ocomp_result_vote_bytes(
        &self,
        port: u16,
        signer_key: &str,
        vote_bytes: Vec<u8>,
    ) -> Result<String> {
        let calldata = IMetadosis::submitLysisResultCall {
            resultVoteV1: Bytes::from(vote_bytes),
        }
        .abi_encode();
        eth::send_calldata(
            &self.url(port),
            addresses::WWD_ADDR,
            signer_key,
            calldata,
            outbe_ocomp_protocol::system_carrier::OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
        )
    }
}

/// One public `submitLysisResult(bytes)` transaction observed in a canonical
/// finalized block. The harness derives this only from public RPC block and
/// receipt data; Supervisor journals and chain storage are not test inputs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompPublicResultVoteTransactionV1 {
    pub transaction_hash: B256,
    pub signer: Address,
    pub block_number: u64,
    pub block_hash: B256,
    pub calldata_len: usize,
    pub raw_transaction_len: usize,
    pub block_rlp_len: usize,
    pub gas_used: u64,
    pub success: bool,
}

/// Public projection of the bounded accountability object. Keeping this
/// evidence shape independent from the optional protocol crate lets ordinary
/// harness builds retain scenario state without enabling OCOMP integration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompPublicVoteAccountabilityV1 {
    pub job_id: B256,
    pub result_validator_set_epoch: u64,
    pub result_committee_set_hash: B256,
    pub result_ocomp_binding_hash: B256,
    pub member_count: u16,
    pub quorum_threshold: u16,
    pub slot_validator_indexes: Vec<u16>,
    pub slot_first_signatures: Vec<(u16, Vec<u8>)>,
    pub quorum_result_digest: Option<B256>,
    pub quorum_height: Option<u64>,
    pub quorum_signer_bitmap: Option<Vec<u8>>,
    pub closed_height: Option<u64>,
    pub timely_bitmap: Option<Vec<u8>>,
    pub matching_bitmap: Option<Vec<u8>>,
    pub divergent_bitmap: Option<Vec<u8>>,
    pub missing_bitmap: Option<Vec<u8>>,
    pub equivocation_bitmap: Option<Vec<u8>>,
}
