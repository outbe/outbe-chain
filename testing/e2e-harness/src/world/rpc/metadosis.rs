use crate::internal::eth::IEmission;
use crate::world::rpc::*;
use alloy_sol_types::SolEvent;

impl Rpc {
    /// Tribute worldwide-day status byte derived from schedule and emission-day state.
    pub fn wwd_status(&self, port: u16, wwd: &str) -> Option<String> {
        let day: u32 = wwd.parse().ok()?;
        Some(self.metadosis_wwd_state_on(port, day)?.status.to_string())
    }

    pub fn metadosis_emission_day_on(
        &self,
        port: u16,
        day: u32,
    ) -> Option<MetadosisEmissionDayStateV1> {
        let r = eth::read_call(
            &self.url(port),
            addresses::EMISSION_ADDR,
            &IEmission::getEmissionDayCall { day },
        )?;
        Some(MetadosisEmissionDayStateV1 {
            emission_day: r.day,
            emission_limit_minor: U256::from(r.metadosisLimit.max(r.emissionLimit)),
            wwd: r.wwd,
            metadosis_start: r.metadosisStart,
            state: r.metadosisState,
        })
    }

    pub fn metadosis_emission_day_at(
        &self,
        port: u16,
        day: u32,
        block_number: u64,
    ) -> Option<MetadosisEmissionDayStateV1> {
        let r = eth::read_call_at(
            &self.url(port),
            addresses::EMISSION_ADDR,
            &IEmission::getEmissionDayCall { day },
            block_number,
        )?;
        Some(MetadosisEmissionDayStateV1 {
            emission_day: r.day,
            emission_limit_minor: U256::from(r.metadosisLimit.max(r.emissionLimit)),
            wwd: r.wwd,
            metadosis_start: r.metadosisStart,
            state: r.metadosisState,
        })
    }

    pub fn metadosis_wwd_state_on(
        &self,
        port: u16,
        day: u32,
    ) -> Option<MetadosisWorldwideDayStateV1> {
        let r = eth::read_call(
            &self.url(port),
            addresses::TRIBUTE_ADDR,
            &ITribute::getWorldwideDayCall { wwd: day },
        )?;
        let emission = self.metadosis_emission_day_on(port, r.day);
        let now = eth::latest_block_timestamp(&self.url(port)).unwrap_or_default();
        Some(build_wwd_state(&r, emission.as_ref(), now))
    }

    pub fn metadosis_terminal_receipt_on(
        &self,
        port: u16,
        day: u32,
    ) -> Option<MetadosisWorldwideDayTerminalReceiptV1> {
        let wwd = eth::read_call(
            &self.url(port),
            addresses::TRIBUTE_ADDR,
            &ITribute::getWorldwideDayCall { wwd: day },
        )?;
        let emission = self.metadosis_emission_day_on(port, wwd.day)?;
        if emission.state == 0 {
            return Some(MetadosisWorldwideDayTerminalReceiptV1 {
                outcome: 0,
                value_routed: U256::ZERO,
                promis_limit_before: U256::ZERO,
                promis_limit_after: U256::ZERO,
                retirement_outcome: 0,
                block_number: 0,
            });
        }
        let finalized = eth::finalized_number(&self.url(port)).unwrap_or(0);
        let block_number = self
            .finalized_metadosis_lysis_completion_blocks_on(port, day, finalized)
            .and_then(|blocks| blocks.first().map(|(b, _)| *b))
            .unwrap_or(1);
        Some(MetadosisWorldwideDayTerminalReceiptV1 {
            outcome: if emission.state == 1 { 4 } else { 3 },
            value_routed: emission.emission_limit_minor,
            promis_limit_before: U256::ZERO,
            promis_limit_after: U256::ZERO,
            retirement_outcome: 0,
            block_number,
        })
    }

    pub fn metadosis_wwd_state_at(
        &self,
        port: u16,
        day: u32,
        block_number: u64,
    ) -> Option<MetadosisWorldwideDayStateV1> {
        let url = self.url(port);
        let r = eth::read_call_at(
            &url,
            addresses::TRIBUTE_ADDR,
            &ITribute::getWorldwideDayCall { wwd: day },
            block_number,
        )?;
        let emission = self.metadosis_emission_day_at(port, r.day, block_number);
        let timestamp = block_timestamp_at(&url, block_number).unwrap_or_default();
        Some(build_wwd_state(&r, emission.as_ref(), timestamp))
    }

    pub fn finalized_metadosis_wwd_started_on(
        &self,
        port: u16,
        day: u32,
    ) -> Option<MetadosisWorldwideDayStartedV1> {
        const SIGNATURE: &str = "WorldwideDayStarted(uint32,uint32,uint64,uint64,uint64,uint64)";
        let rpc_url = self.url(port);
        let finalized_height = eth::finalized_number(&rpc_url)?;
        let topic0 = keccak256(SIGNATURE.as_bytes());
        let indexed_day = format!("0x{day:064x}");
        let logs = eth::raw_json_with_params(
            &rpc_url,
            "eth_getLogs",
            serde_json::json!([{
                "address": format!("{:#x}", addresses::TRIBUTE_ADDR),
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
        let topics = log.get("topics")?.as_array()?;
        let emission_day =
            u32::from_str_radix(topics.get(2)?.as_str()?.trim_start_matches("0x"), 16).ok()?;
        let data = decode_rpc_data_words(log, 4)?;
        let block_number = rpc_log_block_number(log)?;
        let block_hash = canonical_rpc_log_block_hash(&rpc_url, log, block_number)?;
        Some(MetadosisWorldwideDayStartedV1 {
            worldwide_day: day,
            day: emission_day,
            forming_start: u64::try_from(data[0]).ok()?,
            forming_end: u64::try_from(data[1]).ok()?,
            offerings_start: u64::try_from(data[2]).ok()?,
            offering_end: u64::try_from(data[3]).ok()?,
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
}

fn block_timestamp_at(url: &str, block_number: u64) -> Option<u64> {
    let block = eth::raw_json_with_params(
        url,
        "eth_getBlockByNumber",
        serde_json::json!([format!("0x{block_number:x}"), false]),
    )?;
    u64::from_str_radix(
        block.get("timestamp")?.as_str()?.trim_start_matches("0x"),
        16,
    )
    .ok()
}

fn build_wwd_state(
    r: &ITribute::getWorldwideDayReturn,
    emission: Option<&MetadosisEmissionDayStateV1>,
    timestamp: u64,
) -> MetadosisWorldwideDayStateV1 {
    let status = match emission.map(|ed| ed.state) {
        Some(1) => 6,
        Some(2) => 7,
        _ if timestamp >= r.offeringsStart && timestamp < r.offeringEnd => 2,
        _ if timestamp < r.formingEnd => 0,
        _ if timestamp < r.offeringsStart => 1,
        _ => 4,
    };
    let scheduled_process_time = emission
        .filter(|ed| ed.metadosis_start > 0)
        .map_or(r.offeringEnd, |ed| ed.metadosis_start);
    MetadosisWorldwideDayStateV1 {
        day: r.day,
        status,
        forming_start: r.formingStart,
        forming_end: r.formingEnd,
        offerings_start: r.offeringsStart,
        lookback_end: r.offeringsStart,
        offering_end: r.offeringEnd,
        scheduled_process_time,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisEmissionDayStateV1 {
    pub emission_day: u32,
    pub emission_limit_minor: U256,
    pub wwd: u32,
    pub metadosis_start: u64,
    pub state: u8,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisWorldwideDayStateV1 {
    pub day: u32,
    pub status: u8,
    pub forming_start: u64,
    pub forming_end: u64,
    pub offerings_start: u64,
    pub lookback_end: u64,
    pub offering_end: u64,
    pub scheduled_process_time: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetadosisWorldwideDayStartedV1 {
    pub worldwide_day: u32,
    pub day: u32,
    pub forming_start: u64,
    pub forming_end: u64,
    pub offerings_start: u64,
    pub offering_end: u64,
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
