use crate::world::rpc::*;

impl Rpc {
    #[cfg(feature = "ocomp-integration")]
    pub fn assert_certified_nod_mining_blocked(
        &self,
        port: u16,
        private_key: &str,
        generation: &OcompCertifiedGenerationV1,
    ) -> Result<()> {
        let head = self
            .nod_materialization_head_on(port)
            .ok_or_else(|| eyre!("certified NOD materialization head is absent"))?;
        if head.worldwide_day != generation.worldwide_day
            || head.generation != generation.generation
            || head.next_nod_ordinal >= head.nod_count
        {
            return Err(eyre!(
                "certified materialization head does not match the incomplete generation"
            ));
        }
        let owner = self
            .address_of(private_key)
            .ok_or_else(|| eyre!("derive first capacity owner"))?
            .parse::<Address>()
            .wrap_err("parse first capacity owner")?;
        let nod_id = outbe_nod::NodContract::generate_nod_id(
            owner,
            WorldwideDay::new(generation.worldwide_day),
        )?;
        let call = INodFactory::mineGratisCall {
            nodId: nod_id.to_u256(),
            nonce: 0,
            mac: B256::ZERO,
            opNonce: 0,
        };
        // This is an intentional negative transaction. Supplying an explicit
        // bounded gas limit prevents the RPC client from replacing the actual
        // status=0 receipt with an eth_estimateGas error.
        let transaction_hash = eth::send_calldata(
            &self.url(port),
            addresses::NOD_FACTORY_ADDR,
            private_key,
            call.abi_encode(),
            300_000,
        )?;
        if eth::receipt_success(&self.url(port), &transaction_hash) != Some(false) {
            return Err(eyre!(
                "mineGratis succeeded before materialization completion"
            ));
        }
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn wait_for_completed_nod_materialization(
        &self,
        port: u16,
        generation: &OcompCertifiedGenerationV1,
        timeout_seconds: u64,
    ) -> Option<NodMaterializationObservationV1> {
        let stall = Duration::from_secs(timeout_seconds);
        let mut deadline = MaterializationStallDeadline::new(Instant::now(), stall);
        loop {
            if let Some(completed) = self.completed_nod_materialization(port, generation) {
                return Some(completed);
            }
            let head_progress = self
                .nod_materialization_head_on(port)
                .filter(|head| {
                    head.worldwide_day == generation.worldwide_day
                        && head.generation == generation.generation
                })
                .map_or(deadline.last_progress(), |head| head.next_nod_ordinal);
            let finalized_progress = if head_progress > deadline.last_progress() {
                self.materialization_progress_on(
                    port,
                    generation.worldwide_day,
                    generation.generation,
                )
                .and_then(|events| events.into_iter().map(|event| event.next_nod_ordinal).max())
                .unwrap_or(deadline.last_progress())
            } else {
                deadline.last_progress()
            };
            if deadline.observe(Instant::now(), finalized_progress) {
                eprintln!(
                    "NOD materialization made no finalized progress for {timeout_seconds}s: \
                     worldwide_day={} generation={} cursor={}/{}",
                    generation.worldwide_day,
                    generation.generation,
                    deadline.last_progress(),
                    generation.nod_count,
                );
                return None;
            }
            sleep(Duration::from_millis(250));
        }
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn completed_nod_materialization(
        &self,
        port: u16,
        expected: &OcompCertifiedGenerationV1,
    ) -> Option<NodMaterializationObservationV1> {
        let pending_projection = eth::read_call(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::certifiedGenerationCall {
                worldwideDay: expected.worldwide_day,
            },
        )?;
        if pending_projection.exists || self.nod_materialization_head_on(port).is_some() {
            return None;
        }
        let progress =
            self.materialization_progress_on(port, expected.worldwide_day, expected.generation)?;
        let completed = progress
            .iter()
            .find(|event| event.completed && event.next_nod_ordinal == expected.nod_count)?;
        Some(NodMaterializationObservationV1 {
            worldwide_day: expected.worldwide_day,
            generation: expected.generation,
            nod_count: expected.nod_count,
            next_nod_ordinal: completed.next_nod_ordinal,
            successful_batch_transactions: u32::try_from(progress.len()).ok()?,
            completion_block_number: completed.block_number,
        })
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn assert_one_materialized_nod_for_owner(
        &self,
        port: u16,
        owner: Address,
        completion_block_number: u64,
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let last_observation = match self.materialized_nod_for_owner(port, owner) {
                Ok(Some((nod_id, body))) => {
                    if body.owner != owner || body.nodId != U256::from_be_slice(&nod_id) {
                        return Err(eyre!("owner enumeration and nodData disagree"));
                    }
                    return Ok(());
                }
                Ok(None) => "balanceOf returned zero".to_owned(),
                Err(error) => error,
            };
            if Instant::now() >= deadline {
                return Err(eyre!(
                    "materialized owner read did not become available: owner={owner:#x} completion_block={completion_block_number} head={:?} finalized={:?} last_observation={last_observation}",
                    self.head(port),
                    self.finalized(port),
                ));
            }
            sleep(Duration::from_millis(250));
        }
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn mine_first_materialized_capacity_nod(
        &self,
        port: u16,
        private_key: &str,
        pay_note_proof: &[u8],
    ) -> Result<()> {
        let owner = self
            .address_of(private_key)
            .ok_or_else(|| eyre!("derive capacity owner"))?
            .parse::<Address>()
            .wrap_err("parse capacity owner")?;
        let nod_id = self
            .nod_id_of_owner_by_index_on(port, owner, 0)
            .map_err(|error| eyre!("capacity owner NOD read failed: {error}"))?
            .ok_or_else(|| eyre!("capacity owner NOD is unavailable"))?;
        let body = self
            .nod_data_on(port, &nod_id)
            .map_err(|error| eyre!("capacity owner NOD body read failed: {error}"))?;
        let settlement_hash = eth::send_call(
            &self.url(port),
            addresses::NOD_FACTORY_ADDR,
            private_key,
            &INodFactory::settleNodCall {
                nodId: U256::from_be_slice(&nod_id),
                payNoteProof: Bytes::copy_from_slice(pay_note_proof),
            },
            None,
        )?;
        if eth::receipt_success(&self.url(port), &settlement_hash) != Some(true) {
            return Err(eyre!("post-completion settleNod transaction failed"));
        }
        let entity = outbe_compressed_entities::WwdEntityId::try_from(nod_id.as_slice())?;
        let nonce = (0_u64..100_000)
            .find(|nonce| outbe_nodfactory::runtime::validate_pow(entity, *nonce).is_ok())
            .ok_or_else(|| eyre!("find bounded mineGratis nonce"))?;
        let keys = eth::derive_account_keys(
            &self.url(port),
            private_key,
            outbe_tee::protocol::Ledger::Gratis,
        )?;
        let op_nonce = eth::query_gratis(&self.url(port), owner, &keys)?.next_nonce;
        let modify_key = keys.modify;
        let chain_id = B256::from(U256::from(
            self.chain_id(port)
                .ok_or_else(|| eyre!("read chain ID for mineGratis"))?,
        ));
        let mac = outbe_tee_enclave::gratis::modify_mac(
            &modify_key,
            owner,
            outbe_tee::protocol::GratisOp::Mint,
            body.gratisLoadMinor,
            op_nonce,
            chain_id,
        );
        let transaction_hash = eth::send_call(
            &self.url(port),
            addresses::NOD_FACTORY_ADDR,
            private_key,
            &INodFactory::mineGratisCall {
                nodId: U256::from_be_slice(&nod_id),
                nonce,
                mac: B256::from(mac),
                opNonce: op_nonce,
            },
            None,
        )?;
        if eth::receipt_success(&self.url(port), &transaction_hash) != Some(true) {
            return Err(eyre!("post-completion mineGratis transaction failed"));
        }
        Ok(())
    }

    #[cfg(feature = "ocomp-integration")]
    fn nod_materialization_head_on(&self, port: u16) -> Option<NodMaterializationHeadV1> {
        let head = eth::read_call(
            &self.url(port),
            addresses::NOD_FACTORY_ADDR,
            &INodFactory::materializationHeadCall {},
        )?;
        head.exists.then(|| {
            NodMaterializationHeadV1::decode_canonical(
                head.canonicalHead.as_ref(),
                &poc_schema_limits(),
            )
            .ok()
        })?
    }

    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn materialized_nod_for_owner(
        &self,
        port: u16,
        owner: Address,
    ) -> std::result::Result<Option<(Vec<u8>, crate::internal::eth::INod::NodData)>, String> {
        let balance = eth::read_call_result(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::balanceOfCall { owner },
        )?;
        if balance.is_zero() {
            return Ok(None);
        }
        if balance != U256::from(1) {
            return Err(format!(
                "balanceOf returned {balance}, expected exactly one"
            ));
        }
        let nod_id = self
            .nod_id_of_owner_by_index_on(port, owner, 0)?
            .ok_or_else(|| "index zero unexpectedly reported absence".to_owned())?;
        if self.nod_id_of_owner_by_index_on(port, owner, 1)?.is_some() {
            return Err("owner has more than one materialized NOD".to_owned());
        }
        let body = self.nod_data_on(port, &nod_id)?;
        Ok(Some((nod_id, body)))
    }

    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn nod_id_of_owner_by_index_on(
        &self,
        port: u16,
        owner: Address,
        index: u64,
    ) -> std::result::Result<Option<Vec<u8>>, String> {
        let result = eth::read_call_result(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::tokenOfOwnerByIndexCall {
                owner,
                index: U256::from(index),
            },
        )
        .map(|value| value.to_be_bytes::<32>().to_vec());
        classify_owner_index_result(index, result)
    }

    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn nod_data_on(
        &self,
        port: u16,
        nod_id: &[u8],
    ) -> std::result::Result<crate::internal::eth::INod::NodData, String> {
        eth::read_call_result(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::nodDataCall {
                nodId: U256::from_be_slice(nod_id),
            },
        )
    }

    #[cfg(feature = "ocomp-integration")]
    fn materialization_progress_on(
        &self,
        port: u16,
        worldwide_day: u32,
        generation: u64,
    ) -> Option<Vec<NodMaterializationProgressV1>> {
        let finalized_height = eth::finalized_number(&self.url(port))?;
        let topic0 = keccak256(
            b"NodMaterializationProgress(uint64,uint32,uint64,uint32,uint32,bool,uint64)",
        );
        let logs = eth::raw_json_with_params(
            &self.url(port),
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::NOD_FACTORY_ADDR),
                "fromBlock": "0x0",
                "toBlock": format!("0x{finalized_height:x}"),
                "topics": [format!("{topic0:#x}")],
            }]),
        )?;
        Some(
            logs.as_array()?
                .iter()
                .filter_map(decode_nod_materialization_progress)
                .filter(|event| {
                    event.worldwide_day == worldwide_day && event.generation == generation
                })
                .collect::<Vec<_>>(),
        )
    }

    /// Nod total supply on the node at `port`.
    pub fn nod_supply(&self, port: u16) -> Option<u64> {
        eth::read_call(
            &self.url(port),
            addresses::NOD_ADDR,
            &INod::totalSupplyCall {},
        )
        .and_then(|value| u64::try_from(value).ok())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NodMaterializationObservationV1 {
    pub worldwide_day: u32,
    pub generation: u64,
    pub nod_count: u32,
    pub next_nod_ordinal: u32,
    pub successful_batch_transactions: u32,
    pub completion_block_number: u64,
}

#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::world::rpc) struct NodMaterializationProgressV1 {
    pub(super) worldwide_day: u32,
    pub(super) generation: u64,
    pub(super) next_nod_ordinal: u32,
    pub(super) completed: bool,
    pub(super) block_number: u64,
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::rpc) struct MaterializationStallDeadline {
    last_progress: u32,
    deadline: Instant,
    stall: Duration,
}

#[cfg(feature = "ocomp-integration")]
impl MaterializationStallDeadline {
    pub(super) fn new(now: Instant, stall: Duration) -> Self {
        Self {
            last_progress: 0,
            deadline: now + stall,
            stall,
        }
    }

    fn last_progress(&self) -> u32 {
        self.last_progress
    }

    pub(super) fn observe(&mut self, now: Instant, progress: u32) -> bool {
        if progress > self.last_progress {
            self.last_progress = progress;
            self.deadline = now + self.stall;
        }
        now >= self.deadline
    }
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::rpc) fn classify_owner_index_result(
    index: u64,
    result: std::result::Result<Vec<u8>, String>,
) -> std::result::Result<Option<Vec<u8>>, String> {
    match result {
        Ok(nod_id) if nod_id.len() == outbe_compressed_entities::WwdEntityId::len_bytes() => {
            Ok(Some(nod_id))
        }
        Ok(nod_id) => Err(format!(
            "tokenOfOwnerByIndex returned a {}-byte NOD id",
            nod_id.len()
        )),
        Err(error) if index > 0 && error.to_ascii_lowercase().contains("index out of bounds") => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(feature = "ocomp-integration")]
pub(in crate::world::rpc) fn decode_nod_materialization_progress(
    log: &serde_json::Value,
) -> Option<NodMaterializationProgressV1> {
    let topics = log.get("topics")?.as_array()?;
    if topics.len() != 3 {
        return None;
    }
    let topic_word = |index: usize| {
        let value = topics.get(index)?.as_str()?;
        let bytes = hex::decode(value.trim_start_matches("0x")).ok()?;
        (bytes.len() == 32).then(|| U256::from_be_slice(&bytes))
    };
    let _queue_sequence = u64::try_from(topic_word(1)?).ok()?;
    let worldwide_day = u32::try_from(topic_word(2)?).ok()?;
    let data = log
        .get("data")?
        .as_str()
        .and_then(|value| hex::decode(value.trim_start_matches("0x")).ok())?;
    if data.len() != 5 * 32 {
        return None;
    }
    let word = |index: usize| {
        let start = index * 32;
        U256::from_be_slice(&data[start..start + 32])
    };
    let completed = match word(3) {
        value if value.is_zero() => false,
        value if value == U256::from(1) => true,
        _ => return None,
    };
    Some(NodMaterializationProgressV1 {
        worldwide_day,
        generation: u64::try_from(word(0)).ok()?,
        next_nod_ordinal: u32::try_from(word(2)).ok()?,
        completed,
        block_number: u64::try_from(word(4)).ok()?,
    })
}
