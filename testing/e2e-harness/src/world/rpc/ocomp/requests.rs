use crate::world::rpc::*;

impl Rpc {
    /// Observe a finalized `OffchainJobRequested` log through the public RPC.
    ///
    /// This is intentionally read-only: the harness cannot create a job or
    /// provide any of its bindings. The returned block hash is checked against
    /// the canonical block independently read from the same validator.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_job_request(
        &self,
        from_height: u64,
    ) -> Result<Option<OcompPublicJobRequestV1>> {
        self.finalized_ocomp_job_request_on_url(&self.cfg.rpc0, from_height, None)
    }

    /// Observe the same finalized OCOMP request on one named validator.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_job_request_on(
        &self,
        port: u16,
        from_height: u64,
    ) -> Result<Option<OcompPublicJobRequestV1>> {
        self.finalized_ocomp_job_request_on_url(&self.url(port), from_height, None)
    }

    /// Observe the latest finalized OCOMP request for one exact WorldwideDay.
    /// Later events for another day cannot hide the requested job. A valid
    /// pending request returns `None` only on this positive-poll interface.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_job_request_for_worldwide_day_on(
        &self,
        port: u16,
        from_height: u64,
        worldwide_day: u32,
    ) -> Result<Option<OcompPublicJobRequestV1>> {
        self.finalized_ocomp_job_request_on_url(&self.url(port), from_height, Some(worldwide_day))
    }

    /// Observe one exact WorldwideDay while preserving transport, response
    /// shape, canonicality, and protocol-decoding failures. Negative assertions
    /// must use this method: an unavailable RPC is not proof that no request
    /// exists.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_job_request_for_worldwide_day_result_on(
        &self,
        port: u16,
        from_height: u64,
        worldwide_day: u32,
    ) -> Result<OcompRequestObservation> {
        self.finalized_ocomp_job_request_result_on_url(
            &self.url(port),
            from_height,
            Some(worldwide_day),
        )
        .wrap_err_with(|| {
            format!(
                "observe finalized OCOMP request for WWD {worldwide_day} from h{from_height} on RPC port {port}"
            )
        })
    }

    #[cfg(feature = "ocomp-integration")]
    fn finalized_ocomp_job_request_on_url(
        &self,
        rpc_url: &str,
        from_height: u64,
        worldwide_day: Option<u32>,
    ) -> Result<Option<OcompPublicJobRequestV1>> {
        self.finalized_ocomp_job_request_result_on_url(rpc_url, from_height, worldwide_day)
            .and_then(OcompRequestObservation::into_bound_request)
    }

    #[cfg(feature = "ocomp-integration")]
    fn finalized_ocomp_job_request_result_on_url(
        &self,
        rpc_url: &str,
        from_height: u64,
        worldwide_day: Option<u32>,
    ) -> Result<OcompRequestObservation> {
        const EVENT_SIGNATURE: &str = "OffchainJobRequested(bytes32,uint32,uint64,uint32,bytes32)";
        let finalized_block = eth::raw_json_result(
            rpc_url,
            "eth_getBlockByNumber",
            serde_json::json!(["finalized", false]),
        )
        .wrap_err("read finalized block")?;
        let finalized_number = finalized_block
            .get("number")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre!("finalized block omitted number"))?;
        let finalized_height =
            u64::from_str_radix(finalized_number.trim_start_matches("0x"), 16)
                .wrap_err_with(|| format!("decode finalized height {finalized_number}"))?;
        if finalized_height < from_height {
            return Ok(OcompRequestObservation::Absent);
        }
        let topic0 = keccak256(EVENT_SIGNATURE.as_bytes());
        let logs = eth::raw_json_result(
            rpc_url,
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::WWD_ADDR),
                "fromBlock": format!("0x{from_height:x}"),
                "toBlock": format!("0x{finalized_height:x}"),
                "topics": [format!("{topic0:#x}")]
            }]),
        )
        .wrap_err_with(|| {
            format!("read OCOMP request logs through finalized h{finalized_height}")
        })?;
        let logs = logs
            .as_array()
            .ok_or_else(|| eyre!("eth_getLogs returned a non-array response"))?;
        let Some(log) = select_ocomp_job_request_log_result(logs, worldwide_day)? else {
            return Ok(OcompRequestObservation::Absent);
        };
        let topics = log
            .get("topics")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| eyre!("OffchainJobRequested log omitted topics"))?;
        ensure!(
            topics.len() == 3,
            "OffchainJobRequested log has {} topics, expected 3",
            topics.len()
        );
        let observed_topic0 = topics[0]
            .as_str()
            .ok_or_else(|| eyre!("OffchainJobRequested topic0 is not a string"))?;
        ensure!(
            observed_topic0.eq_ignore_ascii_case(&format!("{topic0:#x}")),
            "OffchainJobRequested topic0 mismatch"
        );
        let intent_id = topics[1]
            .as_str()
            .ok_or_else(|| eyre!("OffchainJobRequested intent topic is not a string"))?
            .parse::<B256>()
            .wrap_err("decode OffchainJobRequested intent id")?;
        let worldwide_day = topics[2]
            .as_str()
            .and_then(parse_rpc_word)
            .and_then(|word| u32::try_from(word).ok())
            .ok_or_else(|| eyre!("decode OffchainJobRequested WorldwideDay"))?;
        let data_hex = log
            .get("data")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre!("OffchainJobRequested log omitted data"))?;
        let data = hex::decode(data_hex.trim_start_matches("0x"))
            .wrap_err("decode OffchainJobRequested data")?;
        ensure!(
            data.len() == 3 * 32,
            "OffchainJobRequested data has {} bytes, expected 96",
            data.len()
        );
        let pending_nonce = u64::try_from(U256::from_be_slice(&data[0..32]))
            .wrap_err("decode OffchainJobRequested pending nonce")?;
        let attempt = u32::try_from(U256::from_be_slice(&data[32..64]))
            .wrap_err("decode OffchainJobRequested attempt")?;
        let activation_preconditions_hash = B256::from_slice(&data[64..96]);
        let request_number = log
            .get("blockNumber")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre!("OffchainJobRequested log omitted blockNumber"))?;
        let request_height = u64::from_str_radix(request_number.trim_start_matches("0x"), 16)
            .wrap_err_with(|| format!("decode request block number {request_number}"))?;
        ensure!(
            request_height >= from_height && request_height <= finalized_height,
            "request h{request_height} is outside scanned finalized range h{from_height}..=h{finalized_height}"
        );
        let request_block_hash = log
            .get("blockHash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre!("OffchainJobRequested log omitted blockHash"))?
            .parse::<B256>()
            .wrap_err("decode OffchainJobRequested block hash")?;
        let (canonical_block_hash, canonical_state_root, _) =
            eth::block_commitment_result(rpc_url, request_height)
                .wrap_err_with(|| format!("read canonical request block h{request_height}"))?;
        ensure!(
            canonical_block_hash == request_block_hash,
            "request log block hash {request_block_hash:#x} is not canonical {canonical_block_hash:#x} at h{request_height}"
        );
        let encoded_record = eth::read_call_at_result(
            rpc_url,
            addresses::WWD_ADDR,
            &IMetadosis::getOffchainJobCall {
                intentId: intent_id,
            },
            finalized_height,
        )
        .map_err(|error| eyre!(error))
        .wrap_err_with(|| format!("read finalized OCOMP job {intent_id:#x}"))?;
        let limits = poc_schema_limits();
        let record = OcompJobRecordV1::decode_canonical(encoded_record.as_ref(), &limits)
            .wrap_err("decode finalized OCOMP job record")?;
        ensure!(
            record.intent.intent_id(&limits)? == intent_id,
            "job intent id mismatch"
        );
        ensure!(
            record.intent_height == request_height,
            "job request height mismatch"
        );
        ensure!(
            record.intent.wwd == worldwide_day,
            "job WorldwideDay mismatch"
        );
        ensure!(
            record.intent.pending_nonce == pending_nonce,
            "job pending nonce mismatch"
        );
        ensure!(record.intent.attempt == attempt, "job attempt mismatch");
        ensure!(
            record
                .intent
                .activation_preconditions
                .activation_preconditions_hash(&limits)?
                == activation_preconditions_hash,
            "job activation preconditions hash mismatch"
        );
        let transaction_hash = log
            .get("transactionHash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre!("OffchainJobRequested log omitted transactionHash"))?
            .parse::<B256>()
            .wrap_err("decode OffchainJobRequested transaction hash")?;
        // Validate every event/record binding before declaring any request
        // present, including the legitimate pre-binding interval.
        let Some(finalized) = record.finalized.as_ref() else {
            return Ok(if record.status == OcompJobStatus::AwaitingFinality {
                OcompRequestObservation::AwaitingFinality { intent_id }
            } else {
                OcompRequestObservation::TerminalWithoutBinding {
                    intent_id,
                    status: record.status,
                }
            });
        };
        ensure!(
            finalized.finalized_request_block_hash == canonical_block_hash
                && finalized.finalized_request_state_root == canonical_state_root,
            "finalized job request commitment mismatch"
        );
        ensure!(
            finalized.finality_recorded_height <= finalized_height,
            "job binding is ahead of sampled finalized state"
        );
        Ok(OcompRequestObservation::Bound(OcompPublicJobRequestV1 {
            intent_id,
            job_id: finalized.job_id,
            worldwide_day,
            pending_nonce,
            attempt,
            finality_recorded_height: finalized.finality_recorded_height,
            open_height: finalized.open_height,
            deadline_height: finalized.deadline_height,
            activation_preconditions_hash,
            request_height,
            request_block_hash,
            transaction_hash,
        }))
    }

    /// Read and decode the canonical finalized job record on one validator.
    #[cfg(feature = "ocomp-integration")]
    pub fn finalized_ocomp_job_record_on(
        &self,
        port: u16,
        intent_id: B256,
    ) -> Option<OcompJobRecordV1> {
        let finalized_height = self.finalized_result(port).ok()?;
        self.ocomp_job_record_at_on(port, intent_id, finalized_height)
            .ok()
    }

    /// Read and decode one OCOMP job record at an exact already-finalized height.
    #[cfg(feature = "ocomp-integration")]
    pub fn ocomp_job_record_at_on(
        &self,
        port: u16,
        intent_id: B256,
        height: u64,
    ) -> Result<OcompJobRecordV1> {
        let encoded = eth::read_call_at_result(
            &self.url(port),
            addresses::WWD_ADDR,
            &IMetadosis::getOffchainJobCall {
                intentId: intent_id,
            },
            height,
        )
        .map_err(|error| eyre!(error))?;
        OcompJobRecordV1::decode_canonical(encoded.as_ref(), &poc_schema_limits())
            .map_err(|error| eyre!("decode OCOMP job record at h{height} on port {port}: {error}"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompPublicJobRequestV1 {
    pub intent_id: B256,
    pub job_id: B256,
    pub worldwide_day: u32,
    pub pending_nonce: u64,
    pub attempt: u32,
    pub finality_recorded_height: u64,
    pub open_height: u64,
    pub deadline_height: u64,
    pub activation_preconditions_hash: B256,
    pub request_height: u64,
    pub request_block_hash: B256,
    pub transaction_hash: B256,
}

/// A request event can be finalized before the later canonical job binding.
/// Absence is deliberately distinct from a present request that is not ready.
#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OcompRequestObservation {
    Absent,
    AwaitingFinality {
        intent_id: B256,
    },
    TerminalWithoutBinding {
        intent_id: B256,
        status: OcompJobStatus,
    },
    Bound(OcompPublicJobRequestV1),
}

#[cfg(feature = "ocomp-integration")]
impl OcompRequestObservation {
    /// Negative assertions accept only a successfully observed absence.
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    /// Positive bounded polls may wait for absence/pending, but cannot wait
    /// forever for a request that has already terminated without a binding.
    pub fn into_bound_request(self) -> Result<Option<OcompPublicJobRequestV1>> {
        match self {
            Self::Absent | Self::AwaitingFinality { .. } => Ok(None),
            Self::Bound(request) => Ok(Some(request)),
            Self::TerminalWithoutBinding { intent_id, status } => Err(eyre!(
                "OCOMP request {intent_id:#x} terminated as {status:?} before job binding"
            )),
        }
    }
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::rpc) fn select_ocomp_job_request_log_result(
    logs: &[serde_json::Value],
    worldwide_day: Option<u32>,
) -> Result<Option<&serde_json::Value>> {
    let Some(expected) = worldwide_day else {
        return Ok(logs.last());
    };
    for log in logs.iter().rev() {
        let topics = log
            .get("topics")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| eyre!("OffchainJobRequested log omitted topics"))?;
        let encoded_day = topics
            .get(2)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre!("OffchainJobRequested log omitted WorldwideDay topic"))?;
        let observed = parse_rpc_word(encoded_day)
            .and_then(|word| u32::try_from(word).ok())
            .ok_or_else(|| eyre!("decode OffchainJobRequested WorldwideDay topic {encoded_day}"))?;
        if observed == expected {
            return Ok(Some(log));
        }
    }
    Ok(None)
}
