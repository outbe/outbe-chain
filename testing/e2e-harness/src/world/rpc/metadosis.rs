use crate::world::rpc::*;

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

    #[cfg(feature = "ocomp-integration")]
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
            value_routed: receipt.valueRouted,
            carry_over_before: receipt.carryOverBefore,
            carry_over_after: receipt.carryOverAfter,
            retirement_outcome: receipt.retirementOutcome,
            block_number: receipt.blockNumber,
        })
    }

    #[cfg(feature = "ocomp-integration")]
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

    #[cfg(feature = "ocomp-integration")]
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

    #[cfg(feature = "ocomp-integration")]
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

    #[cfg(feature = "ocomp-integration")]
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
    pub carry_over_before: U256,
    pub carry_over_after: U256,
    pub retirement_outcome: u8,
    pub block_number: u64,
}
