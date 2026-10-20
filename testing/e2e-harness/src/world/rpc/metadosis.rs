use crate::world::rpc::*;
use alloy_sol_types::SolEvent;

impl Rpc {
    /// Metadosis worldwide-day status byte (field 1 of `getWorldwideDay`).
    pub fn wwd_status(&self, port: u16, wwd: &str) -> Option<String> {
        let day: u32 = wwd.parse().ok()?;
        let r = eth::read_call(
            &self.url(port),
            addresses::WWD_ADDR,
            &IMetadosis::getWorldwideDayCall { wwd: day },
        )?;
        Some(r.status.to_string())
    }

    pub fn metadosis_wwd_state_on(
        &self,
        port: u16,
        day: u32,
    ) -> Option<MetadosisWorldwideDayStateV1> {
        let r = eth::read_call(
            &self.url(port),
            addresses::WWD_ADDR,
            &IMetadosis::getWorldwideDayCall { wwd: day },
        )?;
        Some(MetadosisWorldwideDayStateV1 {
            status: r.status,
            day_type: r.dayType,
            forming_start: r.formingStart,
            forming_end: r.formingEnd,
            lookback_end: r.lookbackEnd,
            offering_end: r.offeringEnd,
            scheduled_process_time: r.scheduledProcessTime,
        })
    }

    pub fn metadosis_terminal_receipt_on(
        &self,
        port: u16,
        day: u32,
    ) -> Option<MetadosisWorldwideDayTerminalReceiptV1> {
        let receipt = eth::read_call(
            &self.url(port),
            addresses::WWD_ADDR,
            &IMetadosis::getWorldwideDayTerminalReceiptCall { wwd: day },
        )?;
        Some(MetadosisWorldwideDayTerminalReceiptV1 {
            outcome: receipt.outcome,
            value_routed: receipt.promisLimitReturnedMinor,
            promis_limit_before: receipt.promisLimitBeforeMinor,
            promis_limit_after: receipt.promisLimitAfterMinor,
            retirement_outcome: receipt.retirementOutcome,
            block_number: receipt.blockNumber,
        })
    }

    pub fn metadosis_wwd_state_at(
        &self,
        port: u16,
        day: u32,
        block_number: u64,
    ) -> Option<MetadosisWorldwideDayStateV1> {
        let r = eth::read_call_at(
            &self.url(port),
            addresses::WWD_ADDR,
            &IMetadosis::getWorldwideDayCall { wwd: day },
            block_number,
        )?;
        Some(MetadosisWorldwideDayStateV1 {
            status: r.status,
            day_type: r.dayType,
            forming_start: r.formingStart,
            forming_end: r.formingEnd,
            lookback_end: r.lookbackEnd,
            offering_end: r.offeringEnd,
            scheduled_process_time: r.scheduledProcessTime,
        })
    }

    pub fn metadosis_unknown_status_reverts_at(
        &self,
        port: u16,
        status: u8,
        block_number: u64,
    ) -> Option<bool> {
        eth::read_call_reverts_at(
            &self.url(port),
            addresses::WWD_ADDR,
            &IMetadosis::getWorldwideDaysByStatusCall { status },
            block_number,
        )
    }

    pub fn finalized_metadosis_wwd_started_on(
        &self,
        port: u16,
        day: u32,
    ) -> Option<MetadosisWorldwideDayStartedV1> {
        const SIGNATURE: &str = "WorldwideDayStarted(uint32,uint64,uint64,uint64,uint64,uint64)";
        let rpc_url = self.url(port);
        let finalized_height = eth::finalized_number(&rpc_url)?;
        let topic0 = keccak256(SIGNATURE.as_bytes());
        let indexed_day = format!("0x{day:064x}");
        let logs = eth::raw_json_with_params(
            &rpc_url,
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::WWD_ADDR),
                "fromBlock": "0x1",
                "toBlock": format!("0x{finalized_height:x}"),
                "topics": [format!("{topic0:#x}"), indexed_day]
            }]),
        )?;
        let logs = logs.as_array()?;
        if logs.len() != 1 {
            return None;
        }
        let log = &logs[0];
        let data = decode_rpc_data_words(log, 5)?;
        let block_number = rpc_log_block_number(log)?;
        let block_hash = canonical_rpc_log_block_hash(&rpc_url, log, block_number)?;
        Some(MetadosisWorldwideDayStartedV1 {
            worldwide_day: day,
            forming_start: u64::try_from(data[0]).ok()?,
            forming_end: u64::try_from(data[1]).ok()?,
            lookback_end: u64::try_from(data[2]).ok()?,
            offering_end: u64::try_from(data[3]).ok()?,
            scheduled_process_time: u64::try_from(data[4]).ok()?,
            block_number,
            block_hash,
        })
    }

    /// Canonical successful Lysis events through an already finalized checkpoint.
    pub fn finalized_metadosis_lysis_completion_blocks_on(
        &self,
        port: u16,
        day: u32,
        height: u64,
    ) -> Option<Vec<(u64, B256)>> {
        let rpc_url = self.url(port);
        if height == 0 || eth::finalized_number(&rpc_url)? < height {
            return None;
        }
        let logs = eth::raw_json_with_params(
            &rpc_url,
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::WWD_ADDR),
                "fromBlock": "0x1",
                "toBlock": format!("0x{height:x}"),
                "topics": [
                    format!("{:#x}", IMetadosis::MetadosisExecuted::SIGNATURE_HASH),
                    format!("0x{day:064x}")
                ]
            }]),
        )?;
        logs.as_array()?
            .iter()
            .map(|log| {
                let block_number = decode_lysis_completion_block(log, day)?;
                if block_number > height {
                    return None;
                }
                let block_hash = canonical_rpc_log_block_hash(&rpc_url, log, block_number)?;
                Some((block_number, block_hash))
            })
            .collect()
    }

    pub fn finalized_metadosis_wwd_status_changes_on(
        &self,
        port: u16,
        day: u32,
    ) -> Option<Vec<MetadosisWorldwideDayStatusChangeV1>> {
        const SIGNATURE: &str = "WorldwideDayStatusChange(uint32,uint8,uint8,uint64)";
        let rpc_url = self.url(port);
        let finalized_height = eth::finalized_number(&rpc_url)?;
        let topic0 = keccak256(SIGNATURE.as_bytes());
        let indexed_day = format!("0x{day:064x}");
        let logs = eth::raw_json_with_params(
            &rpc_url,
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::WWD_ADDR),
                "fromBlock": "0x1",
                "toBlock": format!("0x{finalized_height:x}"),
                "topics": [format!("{topic0:#x}"), indexed_day]
            }]),
        )?;
        logs.as_array()?
            .iter()
            .map(|log| {
                let data = decode_rpc_data_words(log, 3)?;
                let block_number = rpc_log_block_number(log)?;
                if u64::try_from(data[2]).ok()? != block_number {
                    return None;
                }
                let block_hash = canonical_rpc_log_block_hash(&rpc_url, log, block_number)?;
                Some(MetadosisWorldwideDayStatusChangeV1 {
                    worldwide_day: day,
                    old_status: u8::try_from(data[0]).ok()?,
                    new_status: u8::try_from(data[1]).ok()?,
                    block_number,
                    block_hash,
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisWorldwideDayStateV1 {
    pub status: u8,
    pub day_type: u8,
    pub forming_start: u64,
    pub forming_end: u64,
    pub lookback_end: u64,
    pub offering_end: u64,
    pub scheduled_process_time: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisWorldwideDayStartedV1 {
    pub worldwide_day: u32,
    pub forming_start: u64,
    pub forming_end: u64,
    pub lookback_end: u64,
    pub offering_end: u64,
    pub scheduled_process_time: u64,
    pub block_number: u64,
    pub block_hash: B256,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisWorldwideDayStatusChangeV1 {
    pub worldwide_day: u32,
    pub old_status: u8,
    pub new_status: u8,
    pub block_number: u64,
    pub block_hash: B256,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisWorldwideDayTerminalReceiptV1 {
    pub outcome: u8,
    pub value_routed: U256,
    pub promis_limit_before: U256,
    pub promis_limit_after: U256,
    pub retirement_outcome: u8,
    pub block_number: u64,
}

fn decode_lysis_completion_block(log: &serde_json::Value, day: u32) -> Option<u64> {
    if log.get("address")?.as_str()?.parse::<Address>().ok()? != addresses::WWD_ADDR
        || log.get("removed").and_then(serde_json::Value::as_bool) == Some(true)
    {
        return None;
    }
    let topics = log
        .get("topics")?
        .as_array()?
        .iter()
        .map(|topic| topic.as_str()?.parse::<B256>().ok())
        .collect::<Option<Vec<_>>>()?;
    if topics.len() != 2 {
        return None;
    }
    let data = hex::decode(log.get("data")?.as_str()?.strip_prefix("0x")?).ok()?;
    let event = IMetadosis::MetadosisExecuted::decode_raw_log_validate(topics, &data).ok()?;
    let block_number = rpc_log_block_number(log)?;
    (event.worldwideDay == day
        && event.status == "COMPLETED"
        && event.blockNumber == block_number
        && block_number > 0)
        .then_some(block_number)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion_log(day: u32, status: &str, block_number: u64) -> serde_json::Value {
        let event = IMetadosis::MetadosisExecuted {
            worldwideDay: day,
            tributeNominalTotalMinor: U256::ZERO,
            gratisDemandMinor: U256::ZERO,
            dayGratisLimitMinor: U256::ZERO,
            lysisLimitMinor: U256::ZERO,
            unusedLysisLimitMinor: U256::ZERO,
            lysisAllocationMinor: U256::ZERO,
            promisLimitReturnedMinor: U256::ZERO,
            status: status.into(),
            blockNumber: block_number,
        };
        let encoded = event.encode_log_data();
        serde_json::json!({
            "address": addresses::WWD_ADDR,
            "topics": encoded.topics(),
            "data": format!("0x{}", hex::encode(encoded.data)),
            "blockNumber": format!("0x{block_number:x}"),
            "removed": false
        })
    }

    #[test]
    fn native_lysis_completion_decodes_the_semantic_success_event() {
        let log = completion_log(20261017, "COMPLETED", 70);
        assert_eq!(decode_lysis_completion_block(&log, 20261017), Some(70));
    }

    #[test]
    fn native_lysis_completion_rejects_wrong_identity_status_and_malformed_logs() {
        let day = 20261017;
        let valid = completion_log(day, "COMPLETED", 70);
        assert_eq!(decode_lysis_completion_block(&valid, day + 1), None);
        assert_eq!(
            decode_lysis_completion_block(&completion_log(day, "FAILED", 70), day),
            None
        );
        assert_eq!(
            decode_lysis_completion_block(&completion_log(day, "COMPLETED", 0), day),
            None
        );
        for (field, value) in [
            ("address", serde_json::json!(Address::ZERO)),
            ("removed", serde_json::json!(true)),
            ("blockNumber", serde_json::json!("0x47")),
            ("data", serde_json::json!("0x00")),
            ("topics", serde_json::json!([])),
            ("topics", serde_json::json!([B256::ZERO, B256::ZERO])),
        ] {
            let mut log = valid.clone();
            log[field] = value;
            assert_eq!(decode_lysis_completion_block(&log, day), None, "{field}");
        }
    }
}
